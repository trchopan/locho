use flutter_rust_bridge::frb;
use locho_client_native::{Attachment, Client, ClientError, TcpOptions};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "runtime".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn error(error: ClientError) -> String {
    match error {
        ClientError::Closed => "closed",
        ClientError::Cancelled => "cancelled",
        ClientError::InvalidHostId => "invalidHostId",
        ClientError::InvalidCapability(_) => "invalidCapability",
        ClientError::UnsupportedService => "unsupportedService",
        ClientError::InvalidDirectAddress => "invalidDirectAddress",
        ClientError::InvalidTimeout => "invalidTimeout",
        ClientError::StartupTimeout => "startupTimeout",
        ClientError::ListenerBind => "listenerBind",
        ClientError::EndpointBind => "endpointBind",
        ClientError::ShutdownTimeout => "shutdownTimeout",
        ClientError::Peer(_) => "peer",
        _ => "internal",
    }
    .into()
}

#[frb(opaque)]
pub struct NativeClient {
    inner: Arc<Client>,
}
#[frb(opaque)]
pub struct NativeAttachment {
    inner: Arc<Attachment>,
}

pub struct NativeStatus {
    pub revision: u64,
    pub state: String,
    pub path: String,
    pub local_port: u16,
    pub error_code: Option<String>,
    pub connection_error_code: Option<String>,
}
fn status(attachment: &Attachment) -> NativeStatus {
    let snapshot = attachment.snapshot();
    NativeStatus {
        revision: snapshot.revision,
        state: format!("{:?}", snapshot.state).to_lowercase(),
        path: format!("{:?}", snapshot.path).to_lowercase(),
        local_port: attachment.local_addr().port(),
        error_code: snapshot.terminal_error.map(error),
        connection_error_code: snapshot
            .peer
            .and_then(|peer| peer.last_error)
            .map(|_| "peer".to_owned()),
    }
}

pub async fn create_client() -> Result<NativeClient, String> {
    #[cfg(target_os = "android")]
    if !crate::android::initialized() {
        return Err("androidContext".into());
    }
    runtime()?;
    Ok(NativeClient {
        inner: Arc::new(Client::new()),
    })
}
impl NativeClient {
    pub async fn start(
        &self,
        host_id: String,
        capability: String,
        local_port: u16,
        direct_address: Option<String>,
    ) -> Result<NativeAttachment, String> {
        let mut options = TcpOptions::new(host_id, capability);
        options.local_port = local_port;
        options.direct_address = direct_address
            .map(|value| value.parse().map_err(|_| "invalidDirectAddress".to_owned()))
            .transpose()?;
        let client = self.inner.clone();
        runtime()?
            .spawn(async move { client.start_tcp(options).await.map_err(error) })
            .await
            .map_err(|_| "internal".to_owned())?
            .map(|attachment| NativeAttachment {
                inner: Arc::new(attachment),
            })
    }
    pub async fn close(&self) -> Result<(), String> {
        let client = self.inner.clone();
        runtime()?
            .spawn(async move { client.close().await.map_err(error) })
            .await
            .map_err(|_| "internal".to_owned())?
    }
}
impl NativeAttachment {
    pub fn snapshot(&self) -> NativeStatus {
        status(&self.inner)
    }
    pub async fn wait_connected(&self, timeout_ms: u32) -> Result<(), String> {
        let attachment = self.inner.clone();
        runtime()?
            .spawn(async move {
                attachment
                    .wait_connected(Duration::from_millis(timeout_ms.into()))
                    .await
                    .map_err(error)
            })
            .await
            .map_err(|_| "internal".to_owned())?
    }
    /// Bounded watch long-poll: no queue or task outlives a one-second call.
    pub async fn next_status(&self, revision: u64) -> Result<NativeStatus, String> {
        let attachment = self.inner.clone();
        runtime()?
            .spawn(async move {
                let mut watch = attachment.subscribe();
                if watch.borrow_and_update().revision == revision {
                    let _ = tokio::time::timeout(Duration::from_secs(1), watch.changed()).await;
                }
                status(&attachment)
            })
            .await
            .map_err(|_| "internal".to_owned())
    }
    pub async fn close(&self) -> Result<(), String> {
        let attachment = self.inner.clone();
        runtime()?
            .spawn(async move { attachment.close().await.map_err(error) })
            .await
            .map_err(|_| "internal".to_owned())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_share_persistent_runtime_and_errors_are_sanitized() {
        let first = runtime().unwrap() as *const _;
        assert_eq!(first, runtime().unwrap() as *const _);
        assert_eq!(error(ClientError::InvalidHostId), "invalidHostId");
        assert_eq!(runtime().unwrap().block_on(async {
            let client = create_client().await.unwrap();
            assert!(matches!(client.start("bad-host-secret".into(), "bad-capability-secret".into(), 0, None).await, Err(code) if code == "invalidHostId"));
            client.close().await
        }), Ok(()));
    }
}

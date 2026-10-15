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
        ClientError::InvalidPeerRelay => "invalidPeerRelay",
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

#[frb(opaque)]
pub struct NativeTicket {
    inner: locho_client_native::Ticket,
}

pub struct TicketMetadata {
    pub host_id: String,
    pub service: String,
    pub service_type: String,
    pub peer_relay_urls: Vec<String>,
}

pub fn parse_ticket(value: String) -> Result<NativeTicket, String> {
    locho_client_native::parse_ticket(&value)
        .map(|inner| NativeTicket { inner })
        .map_err(|_| "invalidTicket".into())
}

impl NativeTicket {
    pub fn metadata(&self) -> TicketMetadata {
        TicketMetadata {
            host_id: self.inner.host_id().into(),
            service: self.inner.capability().service.clone(),
            service_type: self.inner.capability().service_type.as_str().into(),
            peer_relay_urls: self.inner.peer_relay_urls().to_vec(),
        }
    }
    pub fn export(&self) -> Result<String, String> {
        self.inner.encode().map_err(|_| "invalidTicket".into())
    }
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
        ticket: Option<String>,
    ) -> Result<NativeAttachment, String> {
        let mut options = if let Some(ticket) = ticket {
            if !host_id.is_empty() || !capability.is_empty() {
                return Err("conflictingOptions".into());
            }
            let parsed = locho_client_native::parse_ticket(&ticket)
                .map_err(|_| "invalidTicket".to_owned())?;
            TcpOptions::from_ticket(&parsed).map_err(error)?
        } else {
            TcpOptions::new(host_id, capability)
        };
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
    fn ticket_vectors_and_unsupported_services() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../crates/locho-core/tests/vectors/tickets-v1.json"
        ))
        .unwrap();
        for vector in vectors["valid"].as_array().unwrap() {
            let ticket = parse_ticket(vector["ticket"].as_str().unwrap().into()).unwrap();
            assert_eq!(
                ticket.export().unwrap(),
                vector["canonical"].as_str().unwrap()
            );
            assert_eq!(
                ticket.metadata().host_id,
                vector["host_id"].as_str().unwrap()
            );
            if ticket.metadata().service_type == "http" {
                assert_eq!(
                    TcpOptions::from_ticket(&ticket.inner).unwrap_err(),
                    ClientError::UnsupportedService
                );
            }
        }
        for vector in vectors["invalid"].as_array().unwrap() {
            assert!(
                matches!(parse_ticket(vector.as_str().unwrap().into()), Err(code) if code == "invalidTicket")
            );
        }
    }

    #[test]
    fn calls_share_persistent_runtime_and_errors_are_sanitized() {
        let first = runtime().unwrap() as *const _;
        assert_eq!(first, runtime().unwrap() as *const _);
        assert_eq!(error(ClientError::InvalidHostId), "invalidHostId");
        assert_eq!(runtime().unwrap().block_on(async {
            let client = create_client().await.unwrap();
            assert!(matches!(client.start("bad-host-secret".into(), "bad-capability-secret".into(), 0, None, None).await, Err(code) if code == "invalidHostId"));
            client.close().await
        }), Ok(()));
    }
}

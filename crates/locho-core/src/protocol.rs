use anyhow::{bail, Context, Result};
use bytes::Bytes;
use n0_future::time::{sleep_until, Instant};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN: &[u8] = b"locho/3";
pub const PROTOCOL_VERSION: u8 = 3;
pub const MAX_BODY_LEN: usize = 32 * 1024 * 1024;
pub const MAX_HEAD_LEN: usize = 1024 * 1024;
pub const MAX_TCP_CONNECTIONS: usize = 128;
pub const MAX_HTTP_CONNECTIONS: usize = 128;
pub const TCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const TCP_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const HTTP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
pub const MAX_HTTP_REQUEST_TIMEOUT_SECS: u64 = 300;
pub const ATTACH_RECONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const RECONNECT_INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);
pub const RECONNECT_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
pub const RECONNECT_STABLE_DURATION: std::time::Duration = std::time::Duration::from_secs(5);
pub const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const BODY_CHUNK_LEN: usize = 16 * 1024;

#[derive(Debug)]
pub struct TunnelIdleTimeout;

impl std::fmt::Display for TunnelIdleTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TCP tunnel idle timeout")
    }
}

impl std::error::Error for TunnelIdleTimeout {}

#[derive(Serialize, Deserialize)]
pub struct TcpRequestHead {
    pub version: u8,
    pub service: String,
    pub secret_proof: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum StreamRequestHead {
    Http(LochoRequestHead),
    Tcp(TcpRequestHead),
}

#[derive(Serialize, Deserialize)]
pub struct LochoRequestHead {
    pub version: u8,
    pub service: String,
    pub secret_proof: String,
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body_len: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct LochoResponseHead {
    pub version: u8,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_len: Option<u64>,
}

impl std::fmt::Debug for TcpRequestHead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TcpRequestHead")
            .field("version", &self.version)
            .field("secret_proof", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for LochoRequestHead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Paths, service input and application headers may carry credentials too.
        formatter
            .debug_struct("LochoRequestHead")
            .field("version", &self.version)
            .field("secret_proof", &"[REDACTED]")
            .field("body_len", &self.body_len)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for LochoResponseHead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LochoResponseHead")
            .field("version", &self.version)
            .field("status", &self.status)
            .field("body_len", &self.body_len)
            .finish_non_exhaustive()
    }
}

pub async fn write_json_head<T: Serialize, W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    let json = serde_json::to_vec(value).context("serialize tunnel header")?;
    let len = u32::try_from(json.len()).context("tunnel header is too large")?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&json).await?;
    Ok(())
}

pub async fn read_json_head<T: DeserializeOwned, R: AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> Result<T> {
    let mut prefix = [0u8; 4];
    reader
        .read_exact(&mut prefix)
        .await
        .context("read tunnel header length")?;
    let len = u32::from_be_bytes(prefix) as usize;
    #[cfg(feature = "transport")]
    tracing::debug!(
        stage = "head_prefix_read",
        bytes = len,
        "Locho protocol head"
    );
    if len > max_len {
        bail!("tunnel header exceeds limit")
    }
    let mut data = vec![0u8; len];
    reader
        .read_exact(&mut data)
        .await
        .context("read tunnel header")?;
    #[cfg(feature = "transport")]
    tracing::debug!(
        stage = "head_bytes_read",
        bytes = len,
        "Locho protocol head"
    );
    serde_json::from_slice(&data).context("decode tunnel header")
}

pub async fn write_body<W: AsyncWrite + Unpin>(writer: &mut W, body: &[u8]) -> Result<()> {
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn write_body_chunk<W: AsyncWrite + Unpin>(writer: &mut W, body: &[u8]) -> Result<()> {
    let len = u32::try_from(body.len()).context("body chunk is too large")?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn write_body_end<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    writer.write_all(&0u32.to_be_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_body_chunk<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Bytes>> {
    let mut prefix = [0u8; 4];
    reader
        .read_exact(&mut prefix)
        .await
        .context("read body chunk length")?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > BODY_CHUNK_LEN {
        bail!("body chunk exceeds limit")
    }
    if len == 0 {
        return Ok(None);
    }
    let mut data = vec![0u8; len];
    reader
        .read_exact(&mut data)
        .await
        .context("read body chunk")?;
    Ok(Some(Bytes::from(data)))
}

pub async fn read_body_with_limit<R: AsyncRead + Unpin>(
    reader: &mut R,
    len: Option<u64>,
    max_len: usize,
) -> Result<Bytes> {
    let len = len.context("body length is required")?;
    if len > max_len as u64 {
        bail!("body exceeds limit")
    }
    let mut data = vec![0u8; len as usize];
    reader.read_exact(&mut data).await.context("read body")?;
    Ok(Bytes::from(data))
}

pub async fn relay_with_idle_timeout<A, B>(a: A, b: B) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    relay_with_idle_policy(a, b, Some(TCP_IDLE_TIMEOUT)).await
}

/// Relay two streams with an optional application-byte idle deadline.
///
/// `None` disables the deadline. The legacy helper above intentionally keeps
/// the existing five-minute behavior for SDK callers.
pub async fn relay_with_idle_policy<A, B>(
    a: A,
    b: B,
    idle_timeout: Option<std::time::Duration>,
) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let activity = std::sync::Mutex::new(Instant::now());
    let mut a = ActivityStream {
        inner: a,
        activity: &activity,
    };
    let mut b = ActivityStream {
        inner: b,
        activity: &activity,
    };
    // Keep one copy future alive throughout: cancelling/recreating a copy on
    // activity would discard partially written buffers and corrupt the stream.
    let copy = tokio::io::copy_bidirectional_with_sizes(&mut a, &mut b, 16 * 1024, 16 * 1024);
    tokio::pin!(copy);
    loop {
        let last_activity = *activity.lock().expect("relay activity lock poisoned");
        tokio::select! {
            result = &mut copy => return result.map(|_| ()).context("relay streams"),
            _ = async {
                if let Some(duration) = idle_timeout {
                    sleep_until(last_activity + duration).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                if idle_timeout.is_some_and(|duration| {
                    Instant::now().duration_since(*activity.lock().expect("relay activity lock poisoned")) >= duration
                }) {
                    return Err(TunnelIdleTimeout.into());
                }
            }
        }
    }
}

struct ActivityStream<'a, T> {
    inner: T,
    activity: &'a std::sync::Mutex<Instant>,
}

impl<T: AsyncRead + Unpin> AsyncRead for ActivityStream<'_, T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before {
            *self.activity.lock().expect("relay activity lock poisoned") = Instant::now();
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ActivityStream<'_, T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes);
        if matches!(result, std::task::Poll::Ready(Ok(count)) if count > 0) {
            *self.activity.lock().expect("relay activity lock poisoned") = Instant::now();
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test(start_paused = true)]
    async fn blocked_write_still_expires() {
        use std::time::Duration;
        let (mut left, a) = duplex(64);
        let (_right, b) = duplex(64);
        let writer = tokio::spawn(async move { left.write_all(&vec![7; 128 * 1024]).await });
        let relay = relay_with_idle_policy(a, b, Some(Duration::from_secs(10)));
        let error = tokio::time::timeout(Duration::from_secs(11), relay)
            .await
            .expect("blocked writes must not block idle expiry")
            .unwrap_err();
        assert!(error.downcast_ref::<TunnelIdleTimeout>().is_some());
        writer.abort();
        let _ = writer.await;
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_shutdown_still_expires() {
        use std::{
            pin::Pin,
            task::{Context, Poll},
            time::Duration,
        };
        struct PendingShutdown;
        impl AsyncWrite for PendingShutdown {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                Poll::Pending
            }
        }
        let (mut left, a) = duplex(64);
        let (_right, read) = duplex(64);
        left.shutdown().await.unwrap();
        let relay = relay_with_idle_policy(
            a,
            tokio::io::join(read, PendingShutdown),
            Some(Duration::from_secs(10)),
        );
        let error = tokio::time::timeout(Duration::from_secs(11), relay)
            .await
            .expect("blocked shutdown must not prevent expiry")
            .unwrap_err();
        assert!(error.downcast_ref::<TunnelIdleTimeout>().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_direction_does_not_block_reverse_traffic() {
        use std::time::Duration;
        let (left, a) = duplex(64);
        let (mut right, b) = duplex(64);
        let (mut left_read, mut left_write) = tokio::io::split(left);
        let writer = tokio::spawn(async move { left_write.write_all(&vec![7; 128 * 1024]).await });
        let relay = tokio::spawn(relay_with_idle_policy(a, b, None));
        tokio::task::yield_now().await;
        right.write_all(b"pong").await.unwrap();
        let mut reply = [0; 4];
        tokio::time::timeout(Duration::from_secs(1), left_read.read_exact(&mut reply))
            .await
            .expect("reverse traffic stalled behind a blocked write")
            .unwrap();
        assert_eq!(&reply, b"pong");
        writer.abort();
        relay.abort();
        let _ = writer.await;
        let _ = relay.await;
    }

    #[tokio::test]
    async fn tcp_request_matches_existing_wire_vector() {
        assert_eq!(ALPN, b"locho/3");
        assert_eq!(PROTOCOL_VERSION, 3);
        let payload = br#"{"kind":"tcp","version":3,"service":"database","secret_proof":"proof"}"#;
        let request = StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service: "database".into(),
            secret_proof: "proof".into(),
        });
        let (mut writer, mut reader) = duplex(4096);
        write_json_head(&mut writer, &request).await.unwrap();
        writer.shutdown().await.unwrap();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).await.unwrap();
        let mut expected = (payload.len() as u32).to_be_bytes().to_vec();
        expected.extend_from_slice(payload);
        assert_eq!(actual, expected);
    }

    #[test]
    fn diagnostic_headers_redact_proofs_and_application_credentials() {
        let secret = "sensitive-credential";
        let tcp = StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service: secret.into(),
            secret_proof: secret.into(),
        });
        let http = StreamRequestHead::Http(LochoRequestHead {
            version: PROTOCOL_VERSION,
            service: secret.into(),
            secret_proof: secret.into(),
            method: "GET".into(),
            path_and_query: format!("/?token={secret}"),
            headers: vec![("authorization".into(), secret.into())],
            body_len: Some(0),
        });
        let response = LochoResponseHead {
            version: PROTOCOL_VERSION,
            status: 200,
            headers: vec![("set-cookie".into(), secret.into())],
            body_len: Some(0),
        };
        assert!(!format!("{tcp:?} {http:?} {response:?}").contains(secret));
        // Sanitizing Debug must not change serialization on the wire.
        assert!(serde_json::to_string(&tcp).unwrap().contains(secret));
    }

    #[tokio::test]
    async fn headers_and_body_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        let head = LochoResponseHead {
            version: PROTOCOL_VERSION,
            status: 200,
            headers: vec![("x-test".into(), "yes".into())],
            body_len: Some(3),
        };
        write_json_head(&mut a, &head).await.unwrap();
        write_body(&mut a, b"abc").await.unwrap();
        let got: LochoResponseHead = read_json_head(&mut b, MAX_HEAD_LEN).await.unwrap();
        assert_eq!(got.status, 200);
        assert_eq!(
            read_body_with_limit(&mut b, got.body_len, MAX_BODY_LEN)
                .await
                .unwrap(),
            Bytes::from_static(b"abc")
        );
    }

    #[tokio::test]
    async fn oversized_body_is_rejected() {
        let (mut a, mut b) = duplex(4096);
        write_json_head(
            &mut a,
            &LochoResponseHead {
                version: PROTOCOL_VERSION,
                status: 200,
                headers: vec![],
                body_len: Some(3),
            },
        )
        .await
        .unwrap();
        assert!(read_json_head::<LochoResponseHead, _>(&mut b, 2)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn malformed_json_header_is_rejected() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&[0, 0, 0, 3]).await.unwrap();
        a.write_all(b"bad").await.unwrap();
        assert!(read_json_head::<LochoResponseHead, _>(&mut b, MAX_HEAD_LEN)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn zero_length_header_is_rejected() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&[0, 0, 0, 0]).await.unwrap();
        assert!(read_json_head::<LochoResponseHead, _>(&mut b, MAX_HEAD_LEN)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn body_limit_is_enforced() {
        let (mut a, mut b) = duplex(64);
        a.write_all(b"abc").await.unwrap();
        assert!(read_body_with_limit(&mut b, Some(3), 2).await.is_err());
    }

    #[tokio::test]
    async fn stream_request_variants_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        let request = StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service: "database".into(),
            secret_proof: "proof".into(),
        });
        write_json_head(&mut a, &request).await.unwrap();
        let decoded: StreamRequestHead = read_json_head(&mut b, MAX_HEAD_LEN).await.unwrap();
        match decoded {
            StreamRequestHead::Tcp(request) => assert_eq!(request.service, "database"),
            StreamRequestHead::Http(_) => panic!("decoded wrong stream kind"),
        }
    }

    #[tokio::test]
    async fn unknown_stream_kind_is_rejected() {
        let len = br#"{"kind":"udp"}"#.len() as u32;
        let mut bytes = len.to_be_bytes().to_vec();
        bytes.extend_from_slice(br#"{"kind":"udp"}"#);
        let (mut a, mut b) = duplex(128);
        a.write_all(&bytes).await.unwrap();
        assert!(read_json_head::<StreamRequestHead, _>(&mut b, MAX_HEAD_LEN)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn chunked_body_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        write_body_chunk(&mut a, b"abc").await.unwrap();
        write_body_chunk(&mut a, b"def").await.unwrap();
        write_body_end(&mut a).await.unwrap();
        assert_eq!(
            read_body_chunk(&mut b).await.unwrap(),
            Some(Bytes::from_static(b"abc"))
        );
        assert_eq!(
            read_body_chunk(&mut b).await.unwrap(),
            Some(Bytes::from_static(b"def"))
        );
        assert_eq!(read_body_chunk(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_body_chunk_is_rejected() {
        let (mut a, mut b) = duplex(4096);
        let len = (BODY_CHUNK_LEN + 1) as u32;
        a.write_all(&len.to_be_bytes()).await.unwrap();
        assert!(read_body_chunk(&mut b).await.is_err());
    }

    #[tokio::test]
    async fn chunked_body_preserves_large_payload_boundaries() {
        let (mut a, mut b) = duplex(BODY_CHUNK_LEN * 2);
        let payload = vec![b'x'; BODY_CHUNK_LEN + 1];
        write_body_chunk(&mut a, &payload[..BODY_CHUNK_LEN])
            .await
            .unwrap();
        write_body_chunk(&mut a, &payload[BODY_CHUNK_LEN..])
            .await
            .unwrap();
        write_body_end(&mut a).await.unwrap();
        let first = read_body_chunk(&mut b).await.unwrap().unwrap();
        let second = read_body_chunk(&mut b).await.unwrap().unwrap();
        assert_eq!(first.len(), BODY_CHUNK_LEN);
        assert_eq!(second.len(), 1);
        assert_eq!(read_body_chunk(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn concurrent_relays_are_isolated() {
        let mut tasks = Vec::new();
        for value in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            let value = value.to_vec();
            tasks.push(tokio::spawn(async move {
                let (mut left, left_peer) = duplex(128);
                let (mut right, right_peer) = duplex(128);
                let relay = tokio::spawn(relay_with_idle_timeout(left_peer, right_peer));
                left.write_all(&value).await.unwrap();
                left.shutdown().await.unwrap();
                let mut received = vec![0; value.len()];
                right.read_exact(&mut received).await.unwrap();
                right.shutdown().await.unwrap();
                relay.await.unwrap().unwrap();
                assert_eq!(received, value);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn policy_aware_relay_supports_custom_and_disabled_idle_deadlines() {
        let (a, b) = duplex(64);
        let result = relay_with_idle_policy(a, b, Some(std::time::Duration::from_millis(5))).await;
        assert!(result
            .unwrap_err()
            .downcast_ref::<TunnelIdleTimeout>()
            .is_some());

        let (a, b) = duplex(64);
        let mut task = tokio::spawn(relay_with_idle_policy(a, b, None));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3600), &mut task)
                .await
                .is_err()
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn default_relay_expires_after_300_seconds() {
        let (_left, a) = duplex(64);
        let (_right, b) = duplex(64);
        let started = tokio::time::Instant::now();
        let error = relay_with_idle_timeout(a, b).await.unwrap_err();
        assert!(error.downcast_ref::<TunnelIdleTimeout>().is_some());
        assert_eq!(started.elapsed(), TCP_IDLE_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn bytes_reset_idle_deadline_but_half_close_does_not() {
        use std::time::Duration;

        let (mut left, a) = duplex(64);
        let (mut right, b) = duplex(64);
        let relay = tokio::spawn(relay_with_idle_policy(a, b, Some(Duration::from_secs(10))));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(8)).await;
        left.write_all(b"ping").await.unwrap();
        let mut received = [0; 4];
        right.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"ping");
        tokio::time::advance(Duration::from_secs(8)).await;
        tokio::task::yield_now().await;
        assert!(
            !relay.is_finished(),
            "bytes did not reset the idle deadline"
        );
        left.shutdown().await.unwrap();
        assert_eq!(right.read(&mut received).await.unwrap(), 0);
        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        assert!(
            relay.is_finished(),
            "EOF must not reset the application-byte deadline"
        );
        assert!(relay
            .await
            .unwrap()
            .unwrap_err()
            .downcast_ref::<TunnelIdleTimeout>()
            .is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_idle_preserves_half_close_and_late_reply() {
        use std::time::Duration;

        let (mut left, a) = duplex(64);
        let (mut right, b) = duplex(64);
        let relay = tokio::spawn(relay_with_idle_policy(a, b, None));
        left.write_all(b"request").await.unwrap();
        left.shutdown().await.unwrap();
        let mut request = Vec::new();
        right.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request");
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(!relay.is_finished());
        right.write_all(b"reply").await.unwrap();
        right.shutdown().await.unwrap();
        let mut reply = Vec::new();
        left.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"reply");
        relay.await.unwrap().unwrap();
    }
}

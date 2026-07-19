use anyhow::{bail, Context, Result};
use bytes::Bytes;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN: &[u8] = b"locho/1";
pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_BODY_LEN: usize = 32 * 1024 * 1024;
pub const MAX_HEAD_LEN: usize = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct LochoRequestHead {
    pub version: u8,
    pub service: String,
    pub secret_proof: String,
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<(String, String)>,
    pub body_len: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LochoResponseHead {
    pub version: u8,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_len: Option<u64>,
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
    if len > max_len {
        bail!("tunnel header exceeds limit")
    }
    let mut data = vec![0u8; len];
    reader
        .read_exact(&mut data)
        .await
        .context("read tunnel header")?;
    serde_json::from_slice(&data).context("decode tunnel header")
}

pub async fn write_body<W: AsyncWrite + Unpin>(writer: &mut W, body: &[u8]) -> Result<()> {
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

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
    async fn body_limit_is_enforced() {
        let (mut a, mut b) = duplex(64);
        a.write_all(b"abc").await.unwrap();
        assert!(read_body_with_limit(&mut b, Some(3), 2).await.is_err());
    }
}

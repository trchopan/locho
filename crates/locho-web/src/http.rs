//! Bounded, one-request-per-stream HTTP/1.1 subset.
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

pub const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_REQUEST: usize = 4 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;
const MAX_FIELDS: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub authority: String,
    pub headers: Vec<(String, String)>,
    pub timeout_ms: u32,
}

#[derive(Serialize)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[serde(skip)]
    pub body: Vec<u8>,
}

fn hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub fn encode(request: &Request, body: &[u8]) -> Result<Vec<u8>, &'static str> {
    if !(1..=300_000).contains(&request.timeout_ms) {
        return Err("invalidTimeout");
    }
    if !matches!(
        request.method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) {
        return Err("invalidMethod");
    }
    if body.len() > MAX_REQUEST {
        return Err("requestLimit");
    }
    if matches!(request.method.as_str(), "GET" | "HEAD") && !body.is_empty() {
        return Err("invalidBody");
    }
    if !request.path.starts_with('/')
        || request.path.starts_with("//")
        || request
            .path
            .bytes()
            .any(|b| b <= 32 || b >= 127 || b == b'#' || b == b'\\')
        || request.path.parse::<::http::uri::PathAndQuery>().is_err()
    {
        return Err("invalidPath");
    }
    let authority = request
        .authority
        .parse::<::http::uri::Authority>()
        .map_err(|_| "invalidAuthority")?;
    if authority.host().is_empty()
        || request.authority.contains('@')
        || !request.authority.is_ascii()
        || request.authority.bytes().any(|b| b <= 32 || b == 127)
        || (request.authority.ends_with(':'))
        || (authority.port().is_some() && authority.port_u16().is_none())
    {
        return Err("invalidAuthority");
    }
    if request.headers.len() + 4 > MAX_FIELDS {
        return Err("requestLimit");
    }
    let mut out = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: {}\r\n",
        request.method, request.path, request.authority, body.len()).into_bytes();
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if hop(&lower)
            || matches!(
                lower.as_str(),
                "host" | "content-length" | "expect" | "accept-encoding"
            )
            || ::http::header::HeaderName::from_bytes(name.as_bytes()).is_err()
            || value.bytes().any(|b| (b < 32 && b != b'\t') || b >= 127)
        {
            return Err("invalidHeader");
        }
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        if out.len() > MAX_HEAD {
            return Err("requestLimit");
        }
    }
    out.extend_from_slice(b"\r\n");
    if out.len() > MAX_HEAD {
        return Err("requestLimit");
    }
    out.extend_from_slice(body);
    Ok(out)
}

async fn line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    remaining: usize,
) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|_| "transport")?;
        if available.is_empty() {
            return Err("truncatedResponse");
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if out.len() + count > remaining {
            return Err("responseLimit");
        }
        out.extend_from_slice(&available[..count]);
        reader.consume(count);
        if out.ends_with(b"\n") {
            break;
        }
    }
    if !out.ends_with(b"\r\n") {
        return Err("invalidResponse");
    }
    Ok(out)
}

async fn append_exact<R: AsyncRead + Unpin>(
    reader: &mut R,
    body: &mut Vec<u8>,
    count: usize,
) -> Result<(), &'static str> {
    if count > MAX_BODY - body.len() {
        return Err("responseLimit");
    }
    let start = body.len();
    body.resize(start + count, 0);
    reader
        .read_exact(&mut body[start..])
        .await
        .map_err(|_| "truncatedResponse")?;
    Ok(())
}

pub async fn read<R: AsyncRead + Unpin>(stream: R, method: &str) -> Result<Response, &'static str> {
    let mut reader = BufReader::new(stream);
    let mut header_bytes = 0;
    let mut field_count = 0;
    let (status, mut headers) = {
        let mut final_head = None;
        for _ in 0..=8 {
            let mut head = Vec::new();
            loop {
                let next = line(&mut reader, MAX_HEAD - header_bytes).await?;
                header_bytes += next.len();
                let done = next == b"\r\n";
                head.extend(next);
                if done {
                    break;
                }
            }
            let mut fields = [httparse::EMPTY_HEADER; MAX_FIELDS];
            let mut parsed = httparse::Response::new(&mut fields);
            if !parsed
                .parse(&head)
                .map_err(|_| "invalidResponse")?
                .is_complete()
                || parsed.version != Some(1)
            {
                return Err("invalidResponse");
            }
            let code = parsed.code.ok_or("invalidResponse")?;
            if !(100..=599).contains(&code) || code == 101 {
                return Err("unsupportedResponse");
            }
            field_count += parsed.headers.len();
            if field_count > MAX_FIELDS {
                return Err("responseLimit");
            }
            let headers = parsed
                .headers
                .iter()
                .map(|field| {
                    if field
                        .value
                        .iter()
                        .any(|b| (*b < 32 && *b != b'\t') || *b >= 127)
                    {
                        return Err("invalidResponse");
                    }
                    Ok((
                        field.name.to_ascii_lowercase(),
                        std::str::from_utf8(field.value)
                            .map_err(|_| "invalidResponse")?
                            .trim()
                            .to_owned(),
                    ))
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            if code >= 200 {
                final_head = Some((code, headers));
                break;
            }
            if headers
                .iter()
                .any(|(n, _)| n == "content-length" || n == "transfer-encoding")
            {
                return Err("invalidResponse");
            }
        }
        final_head.ok_or("responseLimit")?
    };
    let mut length = None;
    let mut chunked = false;
    let mut connection_fields = Vec::new();
    for (name, value) in &headers {
        match name.as_str() {
            "content-length" => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("invalidResponse");
                }
                let current = value.parse::<usize>().map_err(|_| "invalidResponse")?;
                if length.is_some_and(|previous| previous != current) {
                    return Err("invalidResponse");
                }
                length = Some(current);
            }
            "transfer-encoding" => {
                if chunked || !value.eq_ignore_ascii_case("chunked") {
                    return Err("unsupportedFraming");
                }
                chunked = true;
            }
            "content-encoding" if !value.eq_ignore_ascii_case("identity") => {
                return Err("unsupportedEncoding")
            }
            "connection" => {
                for field in value.split(',') {
                    let field = field.trim().to_ascii_lowercase();
                    if ::http::header::HeaderName::from_bytes(field.as_bytes()).is_err() {
                        return Err("invalidResponse");
                    }
                    connection_fields.push(field);
                }
            }
            _ => {}
        }
    }
    if chunked && length.is_some() {
        return Err("invalidResponse");
    }
    if status == 205 && (chunked || length.is_some_and(|n| n != 0)) {
        return Err("invalidResponse");
    }
    let mut body = Vec::new();
    if status == 204 && (chunked || length.is_some()) {
        return Err("invalidResponse");
    }
    if method != "HEAD" && !matches!(status, 204 | 205 | 304) {
        if chunked {
            let mut framing_bytes = 0;
            loop {
                let size_line = line(&mut reader, MAX_HEAD - framing_bytes).await?;
                framing_bytes += size_line.len();
                let text = std::str::from_utf8(&size_line[..size_line.len() - 2])
                    .map_err(|_| "invalidResponse")?;
                // Chunk extensions are outside the initial supported subset.
                if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalidResponse");
                }
                let size = usize::from_str_radix(text, 16).map_err(|_| "invalidResponse")?;
                if size == 0 {
                    let trailer = line(&mut reader, MAX_HEAD - header_bytes).await?;
                    if trailer != b"\r\n" {
                        return Err("unsupportedTrailers");
                    }
                    break;
                }
                append_exact(&mut reader, &mut body, size).await?;
                let mut ending = [0; 2];
                reader
                    .read_exact(&mut ending)
                    .await
                    .map_err(|_| "truncatedResponse")?;
                if ending != *b"\r\n" {
                    return Err("invalidResponse");
                }
            }
        } else if let Some(length) = length {
            append_exact(&mut reader, &mut body, length).await?;
        } else {
            reader
                .take((MAX_BODY + 1) as u64)
                .read_to_end(&mut body)
                .await
                .map_err(|_| "transport")?;
            if body.len() > MAX_BODY {
                return Err("responseLimit");
            }
        }
    }
    headers.retain(|(name, _)| {
        !hop(name) && !connection_fields.contains(name) && name != "content-length"
    });
    Ok(Response {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fragmented_length_response_completes_without_waiting_for_eof() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(64);
        let task = tokio::spawn(async move {
            for byte in b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok" {
                writer.write_all(&[*byte]).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), read(reader, "GET"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.body, b"ok");
        task.abort();
    }
    #[tokio::test]
    async fn rejects_limits_ambiguous_framing_and_trailers() {
        for bytes in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxZZ0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Trailer: yes\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n800001\r\n",
        ] {
            assert!(read(bytes, "GET").await.is_err());
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nX-Large: {}\r\n\r\n",
            "a".repeat(MAX_HEAD)
        );
        assert!(matches!(
            read(head.as_bytes(), "GET").await,
            Err("responseLimit")
        ));
        let head = format!("HTTP/1.1 200 OK\r\n{}\r\n", "X-Test: yes\r\n".repeat(101));
        assert!(read(head.as_bytes(), "GET").await.is_err());
        let eof = read(&b"HTTP/1.1 404 Missing\r\nConnection: close, x-hidden\r\nX-Hidden: omit\r\nX-Keep: yes\r\n\r\neof"[..], "GET").await.unwrap();
        assert_eq!(eof.body, b"eof");
        assert_eq!(eof.headers, vec![("x-keep".into(), "yes".into())]);
        let r = read(
            &b"HTTP/1.1 304 Not Modified\r\nContent-Length: 99\r\n\r\n"[..],
            "GET",
        )
        .await
        .unwrap();
        assert!(r.body.is_empty());
    }
    #[tokio::test]
    async fn framing_and_binary() {
        let r = read(&b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Test: yes\r\n\r\n3\r\n\x00\xffx\r\n0\r\n\r\n"[..], "GET").await.unwrap();
        assert_eq!(r.body, b"\x00\xffx");
        assert_eq!(r.headers, vec![("x-test".into(), "yes".into())]);
        assert!(
            read(&b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nx"[..], "GET")
                .await
                .is_err()
        );
        assert!(read(
            &b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n"[..],
            "GET"
        )
        .await
        .is_err());
        assert!(read(
            &b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n"[..],
            "GET"
        )
        .await
        .is_err());
        assert!(
            read(&b"HTTP/1.1 101 Switching Protocols\r\n\r\n"[..], "GET")
                .await
                .is_err()
        );
        assert!(read(
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\n"[..],
            "GET"
        )
        .await
        .is_err());
        assert!(read(&b"HTTP/1.1 204 No Content\r\n\r\n"[..], "GET")
            .await
            .unwrap()
            .body
            .is_empty());
        assert!(read(
            &b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n"[..],
            "HEAD"
        )
        .await
        .unwrap()
        .body
        .is_empty());
    }
    #[test]
    fn request_validation() {
        let mut r = Request {
            method: "POST".into(),
            path: "/notes?q=yes".into(),
            authority: "sample-api".into(),
            headers: vec![("Authorization".into(), "Bearer synthetic".into())],
            timeout_ms: 15000,
        };
        assert!(encode(&r, b"{}").is_ok());
        for path in [
            "https://other/",
            "//other/",
            "/bad\r\n",
            "/x#fragment",
            "/back\\slash",
        ] {
            r.path = path.into();
            assert!(encode(&r, b"").is_err());
        }
        r.path = "/".into();
        for name in [
            "Host",
            "Content-Length",
            "Connection",
            "Accept-Encoding",
            "Upgrade",
            "Proxy-Authorization",
        ] {
            r.headers = vec![(name.into(), "x".into())];
            assert!(encode(&r, b"").is_err());
        }
        r.headers.clear();
        r.method = "GET".into();
        assert!(encode(&r, b"x").is_err());
    }
}

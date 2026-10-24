//! Bounded, one-request-per-stream HTTP/1.1 subset.
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};

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

pub const READ_BYTES: usize = 16 * 1024;
const MAX_CHUNK_LINE: usize = 1024;

enum Framing {
    Empty,
    Length(usize),
    Chunked { remaining: usize, ending: bool },
    Eof,
}

pub struct Body<R> {
    reader: BufReader<R>,
    framing: Framing,
    framing_bytes: usize,
    trailer_limit: usize,
}

impl<R: AsyncRead + Unpin> Body<R> {
    pub fn empty(&self) -> bool {
        matches!(self.framing, Framing::Empty | Framing::Length(0))
    }

    // Timeout only waits for upstream bytes, never time spent paused by the consumer.
    async fn available(&mut self, idle: Option<u32>) -> Result<&[u8], &'static str> {
        let read = self.reader.fill_buf();
        match idle {
            Some(ms) => {
                n0_future::time::timeout(n0_future::time::Duration::from_millis(ms.into()), read)
                    .await
                    .map_err(|_| "idleTimeout")?
                    .map_err(|_| "transport")
            }
            None => read.await.map_err(|_| "transport"),
        }
    }

    async fn framing_line(
        &mut self,
        limit: usize,
        idle: Option<u32>,
    ) -> Result<Vec<u8>, &'static str> {
        let mut out = Vec::new();
        loop {
            let bytes = self.available(idle).await?;
            if bytes.is_empty() {
                return Err("truncatedResponse");
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| i + 1);
            if out.len() + count > limit {
                return Err("responseLimit");
            }
            out.extend_from_slice(&bytes[..count]);
            self.reader.consume(count);
            if out.ends_with(b"\n") {
                break;
            }
        }
        if !out.ends_with(b"\r\n") {
            return Err("invalidResponse");
        }
        Ok(out)
    }

    pub async fn next(
        &mut self,
        idle: Option<u32>,
        buffered: bool,
    ) -> Result<Option<Vec<u8>>, &'static str> {
        loop {
            match self.framing {
                Framing::Empty | Framing::Length(0) => return Ok(None),
                Framing::Chunked {
                    remaining: 0,
                    ending,
                } => {
                    if ending && self.framing_line(2, idle).await? != b"\r\n" {
                        return Err("invalidResponse");
                    }
                    let limit = if buffered {
                        MAX_HEAD.saturating_sub(self.framing_bytes)
                    } else {
                        MAX_CHUNK_LINE
                    };
                    let size_line = self.framing_line(limit, idle).await?;
                    self.framing_bytes = if buffered {
                        self.framing_bytes + size_line.len()
                    } else {
                        0
                    };
                    let text = std::str::from_utf8(&size_line[..size_line.len() - 2])
                        .map_err(|_| "invalidResponse")?;
                    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("invalidResponse");
                    }
                    let size = usize::from_str_radix(text, 16).map_err(|_| "invalidResponse")?;
                    if size == 0 {
                        if self.framing_line(self.trailer_limit, idle).await? != b"\r\n" {
                            return Err("unsupportedTrailers");
                        }
                        self.framing = Framing::Empty;
                        return Ok(None);
                    }
                    if buffered && size > MAX_BODY {
                        return Err("responseLimit");
                    }
                    self.framing = Framing::Chunked {
                        remaining: size,
                        ending: false,
                    };
                }
                _ => {
                    let max = match self.framing {
                        Framing::Length(n) | Framing::Chunked { remaining: n, .. } => {
                            n.min(READ_BYTES)
                        }
                        _ => READ_BYTES,
                    };
                    let bytes = self.available(idle).await?;
                    if bytes.is_empty() {
                        if matches!(self.framing, Framing::Eof) {
                            self.framing = Framing::Empty;
                            return Ok(None);
                        }
                        return Err("truncatedResponse");
                    }
                    let count = max.min(bytes.len());
                    let out = bytes[..count].to_vec();
                    self.reader.consume(count);
                    match &mut self.framing {
                        Framing::Length(n) => *n -= count,
                        Framing::Chunked { remaining, ending } => {
                            *remaining -= count;
                            *ending = *remaining == 0;
                        }
                        _ => {}
                    }
                    return Ok(Some(out));
                }
            }
        }
    }
}

pub async fn head<R: AsyncRead + Unpin>(
    stream: R,
    method: &str,
) -> Result<(Response, Body<R>), &'static str> {
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
    if status == 204 && (chunked || length.is_some()) {
        return Err("invalidResponse");
    }
    let framing = if method == "HEAD" || matches!(status, 204 | 205 | 304) {
        Framing::Empty
    } else if chunked {
        Framing::Chunked {
            remaining: 0,
            ending: false,
        }
    } else if let Some(n) = length {
        Framing::Length(n)
    } else {
        Framing::Eof
    };
    headers.retain(|(name, _)| {
        !hop(name) && !connection_fields.contains(name) && name != "content-length"
    });
    Ok((
        Response {
            status,
            headers,
            body: Vec::new(),
        },
        Body {
            reader,
            framing,
            framing_bytes: 0,
            trailer_limit: MAX_HEAD - header_bytes,
        },
    ))
}

pub async fn read<R: AsyncRead + Unpin>(stream: R, method: &str) -> Result<Response, &'static str> {
    let (mut response, mut body) = head(stream, method).await?;
    if matches!(body.framing, Framing::Length(n) if n > MAX_BODY) {
        return Err("responseLimit");
    }
    while let Some(bytes) = body.next(None, true).await? {
        if bytes.len() > MAX_BODY - response.body.len() {
            return Err("responseLimit");
        }
        response.body.extend(bytes);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cookie_headers_preserve_order_duplicates_and_attributes() {
        let bytes = b"HTTP/1.1 200 OK\r\nSet-Cookie: session=synthetic; Path=/; HttpOnly; Secure; SameSite=Lax\r\nSet-Cookie: preference=one; Expires=Wed, 21 Oct 2037 07:28:00 GMT; Path=/\r\nSet-Cookie: session=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let buffered = read(bytes.as_slice(), "GET").await.unwrap();
        let (streamed, _) = head(bytes.as_slice(), "GET").await.unwrap();
        let expected = vec![
            (
                "set-cookie".into(),
                "session=synthetic; Path=/; HttpOnly; Secure; SameSite=Lax".into(),
            ),
            (
                "set-cookie".into(),
                "preference=one; Expires=Wed, 21 Oct 2037 07:28:00 GMT; Path=/".into(),
            ),
            (
                "set-cookie".into(),
                "session=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/".into(),
            ),
        ];
        assert_eq!(buffered.headers, expected);
        assert_eq!(streamed.headers, expected);
    }

    #[tokio::test]
    async fn streaming_returns_headers_and_partial_chunk_before_completion() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(64);
        writer
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        let (response, mut body) = head(reader, "GET").await.unwrap();
        assert_eq!(response.status, 200);
        writer.write_all(b"1000000\r\nfirst").await.unwrap();
        assert_eq!(
            body.next(Some(100), false).await.unwrap().unwrap(),
            b"first"
        );
        // A server HTTP chunk can exceed the buffered lifetime body cap without allocation.
        assert_eq!(body.next(Some(10), false).await.unwrap_err(), "idleTimeout");
    }

    #[tokio::test]
    async fn streaming_has_per_line_not_lifetime_framing_limits() {
        let mut bytes = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..25000 {
            bytes.extend_from_slice(b"1\r\nx\r\n");
        }
        bytes.extend_from_slice(b"0\r\n\r\n");
        assert_eq!(
            read(bytes.as_slice(), "GET").await.err(),
            Some("responseLimit")
        );
        let (_, mut body) = head(bytes.as_slice(), "GET").await.unwrap();
        let mut count = 0;
        while let Some(chunk) = body.next(None, false).await.unwrap() {
            count += chunk.len();
            assert!(chunk.len() <= READ_BYTES);
        }
        assert_eq!(count, 25000);
        let malformed = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}\r\n",
            "0".repeat(MAX_CHUNK_LINE)
        );
        let (_, mut body) = head(malformed.as_bytes(), "GET").await.unwrap();
        assert_eq!(body.next(None, false).await.err(), Some("responseLimit"));
    }

    #[tokio::test]
    async fn streaming_exceeds_buffered_body_cap_with_bounded_reads() {
        let bytes = [
            b"HTTP/1.1 200 OK\r\n\r\n".as_slice(),
            &vec![42; MAX_BODY + 1],
        ]
        .concat();
        let (_, mut body) = head(bytes.as_slice(), "GET").await.unwrap();
        let mut count = 0;
        while let Some(chunk) = body.next(None, false).await.unwrap() {
            assert!(chunk.len() <= READ_BYTES);
            count += chunk.len();
        }
        assert_eq!(count, MAX_BODY + 1);
    }
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

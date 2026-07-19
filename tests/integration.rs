#![cfg(feature = "integration-test")]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_IO_TIMEOUT: Duration = Duration::from_secs(5);
const BODY_CHUNK_LEN: usize = 16 * 1024;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "locho-integration-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ProcessOutput {
    child: Child,
    lines: mpsc::Receiver<String>,
    output: Arc<Mutex<Vec<String>>>,
}

impl ProcessOutput {
    fn spawn(mut command: Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let stdout_output = Arc::clone(&output);
        let output_sender = sender.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                stdout_output.lock().unwrap().push(line.clone());
                let _ = sender.send(line);
            }
        });
        let stderr_output = Arc::clone(&output);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let line = format!("stderr: {line}");
                stderr_output.lock().unwrap().push(line.clone());
                let _ = output_sender.send(line);
            }
        });
        Self {
            child,
            lines,
            output,
        }
    }

    fn wait_for(&self, text: &str) -> String {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(line) = self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                if line.contains(text) {
                    return line;
                }
                output.push(line);
            }
        }
        panic!("timed out waiting for {text}; output: {output:?}");
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn output(&self) -> Vec<String> {
        self.output.lock().unwrap().clone()
    }
}

impl Drop for ProcessOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(feature = "integration-test")]
#[test]
fn tcp_attachment_supports_concurrency_restart_and_rotation() {
    let state_dir = TestDir::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        for _ in 0..3 {
            upstream_listener
                .set_nonblocking(false)
                .expect("configure upstream listener");
            let (mut stream, _) = upstream_listener.accept().unwrap();
            let mut request = [0u8; 5];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(&request).unwrap();
        }
    });

    let config_path = state_dir.path().join("locho.toml");
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"database\"\ntype = \"tcp\"\nendpoint = \"{upstream_address}\"\n"
        ),
    )
    .unwrap();

    let direct_address = format!("127.0.0.1:{}", free_port());
    let mut host = start_host(state_dir.path(), &config_path, &direct_address);
    host.wait_for("locho direct-address ");
    let attach_command = host.wait_for("locho attach ");
    let (host_id, service, secret) = parse_attach_command(&attach_command);
    let first_port = free_port();
    let mut attachment = start_ready_attachment(
        state_dir.path(),
        &attach_command,
        first_port,
        &direct_address,
    );
    assert_round_trip(first_port, b"one!!");

    let second_port = free_port();
    let mut second_attachment = start_ready_attachment(
        state_dir.path(),
        &attach_command,
        second_port,
        &direct_address,
    );
    assert_round_trip(second_port, b"two!!");

    host.stop();
    attachment.stop();
    second_attachment.stop();

    run_cli(state_dir.path(), ["rotate-secret", service.as_str()]);

    let mut restarted_host = start_host(state_dir.path(), &config_path, &direct_address);
    restarted_host.wait_for("locho direct-address ");
    let rotated_command = restarted_host.wait_for("locho attach ");
    let (rotated_host_id, rotated_service, rotated_secret) = parse_attach_command(&rotated_command);
    assert_eq!(rotated_host_id, host_id);
    assert_eq!(rotated_service, service);
    assert_ne!(rotated_secret, secret);

    let old_port = free_port();
    let mut old_attachment = start_attachment(
        state_dir.path(),
        &format!("locho attach {host_id} {service} {secret}"),
        old_port,
        &direct_address,
    );
    old_attachment.wait_for("Local TCP listener");
    assert_rejected(old_port);
    old_attachment.wait_for("TCP attachment rejected with status 403");
    old_attachment.stop();

    let new_port = free_port();
    let mut new_attachment = start_ready_attachment(
        state_dir.path(),
        &rotated_command,
        new_port,
        &direct_address,
    );

    // The restarted host must retain its identity while accepting only the replacement secret.
    assert_round_trip(new_port, b"three");
    assert!(upstream_thread.join().is_ok());
    restarted_host.stop();
    new_attachment.stop();
}

#[cfg(feature = "integration-test")]
#[test]
fn http_attachment_proxies_methods_headers_and_streamed_bodies() {
    let state_dir = TestDir::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..11 {
            let (stream, _) = accept_with_deadline(&upstream_listener);
            stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
            handlers.push(thread::spawn(move || handle_http_upstream(stream)));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });

    let config_path = state_dir.path().join("locho.toml");
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"api\"\ntype = \"http\"\nupstream = \"http://{upstream_address}\"\n"
        ),
    )
    .unwrap();
    let direct_address = format!("127.0.0.1:{}", free_port());
    let mut host = start_host(state_dir.path(), &config_path, &direct_address);
    host.wait_for("locho direct-address ");
    let attach_command = host.wait_for("locho attach ");
    let attach_port = free_port();
    let mut attachment = start_http_attachment(
        state_dir.path(),
        &attach_command,
        attach_port,
        &direct_address,
    );
    attachment.wait_for("Local proxy:");

    for (method, path, body) in [
        ("GET", "/get", b"".as_slice()),
        ("POST", "/post", b"request-body".as_slice()),
        ("PUT", "/put", b"put-body".as_slice()),
        ("PATCH", "/patch", b"patch-body".as_slice()),
        ("DELETE", "/delete", b"".as_slice()),
        ("OPTIONS", "/options", b"".as_slice()),
    ] {
        let response = send_http_request(attach_port, method, path, body, false);
        assert_eq!(response.status, 200, "unexpected response for {method}");
        assert_eq!(response.body, b"method-ok");
        assert_eq!(response.headers.get("x-upstream"), Some(&"yes".to_string()));
    }

    let response = send_http_request(attach_port, "HEAD", "/head", b"", false);
    assert_eq!(response.status, 200);
    assert!(response.body.is_empty());

    let response = send_http_request(
        attach_port,
        "POST",
        "/chunked-response",
        b"chunked-request-body",
        true,
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"streamed-response-body");
    assert_eq!(
        response.headers.get("x-upstream"),
        Some(&"streamed".to_string())
    );

    let response = send_http_request(attach_port, "CONNECT", "/unsupported", b"", false);
    assert_eq!(response.status, 405);

    let response = read_streaming_response(attach_port);
    assert_eq!(response.status, 200);
    assert_eq!(response.body.len(), BODY_CHUNK_LEN * 2 + 3);
    assert!(response.body.iter().all(|byte| *byte == b'x'));

    let response = send_oversized_request(attach_port);
    assert_eq!(response.status, 413);

    let first = thread::spawn({
        move || send_http_request(attach_port, "GET", "/concurrent/one", b"", false)
    });
    let second =
        thread::spawn(move || send_http_request(attach_port, "GET", "/concurrent/two", b"", false));
    assert_eq!(first.join().unwrap().body, b"method-ok");
    assert_eq!(second.join().unwrap().body, b"method-ok");

    assert!(upstream_thread.join().is_ok());
    attachment.stop();
    host.stop();
}

#[cfg(feature = "integration-test")]
#[test]
fn http_attachment_reports_unavailable_upstream() {
    let state_dir = TestDir::new();
    let unused_address = free_port();
    let config_path = state_dir.path().join("locho.toml");
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"api\"\ntype = \"http\"\nupstream = \"http://127.0.0.1:{unused_address}\"\n"
        ),
    )
    .unwrap();
    let direct_address = format!("127.0.0.1:{}", free_port());
    let mut host = start_host(state_dir.path(), &config_path, &direct_address);
    host.wait_for("locho direct-address ");
    let attach_command = host.wait_for("locho attach ");
    let attach_port = free_port();
    let mut attachment = start_http_attachment(
        state_dir.path(),
        &attach_command,
        attach_port,
        &direct_address,
    );
    attachment.wait_for("Local proxy:");

    let response = send_http_request(attach_port, "GET", "/unavailable", b"", false);
    assert_eq!(response.status, 502);
    assert!(response.body.is_empty());

    attachment.stop();
    host.stop();
}

#[cfg(feature = "integration-test")]
#[test]
fn http_attachment_reports_upstream_timeout() {
    let state_dir = TestDir::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        let (stream, _) = accept_with_deadline(&upstream_listener);
        stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        thread::sleep(Duration::from_millis(500));
    });
    let config_path = state_dir.path().join("locho.toml");
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"api\"\ntype = \"http\"\nupstream = \"http://{upstream_address}\"\n"
        ),
    )
    .unwrap();
    let direct_address = format!("127.0.0.1:{}", free_port());
    let mut host =
        start_host_with_timeout(state_dir.path(), &config_path, &direct_address, Some("100"));
    host.wait_for("locho direct-address ");
    let attach_command = host.wait_for("locho attach ");
    let attach_port = free_port();
    let mut attachment = start_http_attachment(
        state_dir.path(),
        &attach_command,
        attach_port,
        &direct_address,
    );
    attachment.wait_for("Local proxy:");

    let response = send_http_request(attach_port, "GET", "/slow", b"", false);
    assert_eq!(response.status, 504);
    assert!(upstream_thread.join().is_ok());
    attachment.stop();
    host.stop();
}

#[cfg(feature = "integration-test")]
#[test]
fn http_attachment_stops_active_request_with_host_shutdown() {
    let state_dir = TestDir::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        let (stream, _) = accept_with_deadline(&upstream_listener);
        stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
        thread::sleep(Duration::from_secs(2));
    });
    let config_path = state_dir.path().join("locho.toml");
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"api\"\ntype = \"http\"\nupstream = \"http://{upstream_address}\"\n"
        ),
    )
    .unwrap();
    let direct_address = format!("127.0.0.1:{}", free_port());
    let mut host = start_host(state_dir.path(), &config_path, &direct_address);
    host.wait_for("locho direct-address ");
    let attach_command = host.wait_for("locho attach ");
    let attach_port = free_port();
    let mut attachment = start_http_attachment(
        state_dir.path(),
        &attach_command,
        attach_port,
        &direct_address,
    );
    attachment.wait_for("Local proxy:");

    let request_thread =
        thread::spawn(move || send_http_request(attach_port, "GET", "/active", b"", false));
    thread::sleep(Duration::from_millis(100));
    host.stop();
    let response = request_thread.join().unwrap();
    assert!(matches!(response.status, 502 | 504));
    assert!(upstream_thread.join().is_ok());
    attachment.stop();
}

struct HttpResponse {
    status: u16,
    headers: std::collections::HashMap<String, String>,
    body: Vec<u8>,
}

fn send_http_request(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    chunked: bool,
) -> HttpResponse {
    let mut stream = connect_with_retry(port);
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nX-Client: integration\r\nX-Hop-By-Hop: should-not-forward\r\nConnection: x-hop-by-hop, close\r\n"
    )
    .into_bytes();
    if chunked {
        request.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
        request.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
        request.extend_from_slice(body);
        request.extend_from_slice(b"\r\n0\r\n\r\n");
    } else {
        request.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        request.extend_from_slice(body);
    }
    stream.write_all(&request).unwrap();
    read_http_response(&mut stream, method == "HEAD")
}

fn send_oversized_request(port: u16) -> HttpResponse {
    let mut stream = connect_with_retry(port);
    stream
        .write_all(
            b"POST /oversized HTTP/1.1\r\nHost: localhost\r\nContent-Length: 33554433\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    read_http_response(&mut stream, false)
}

fn read_streaming_response(port: u16) -> HttpResponse {
    let mut stream = connect_with_retry(port);
    stream
        .write_all(
            b"GET /large-response HTTP/1.1\r\nHost: localhost\r\nX-Client: integration\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    let mut bytes = read_until_headers(&mut stream);
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = header_text.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = parse_headers(lines);
    let body = read_message_body(&mut stream, &mut bytes, header_end, &headers);
    assert!(
        !body.is_empty(),
        "streamed response did not deliver an initial body chunk"
    );
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn handle_http_upstream(mut stream: TcpStream) {
    let request = read_http_message(&mut stream);
    assert_eq!(
        request.headers.get("x-client"),
        Some(&"integration".to_string())
    );
    assert_eq!(
        request.headers.get("x-hop-by-hop"),
        None,
        "hop-by-hop headers must not cross the tunnel"
    );

    let response_body = if request.path == "/chunked-response" {
        assert_eq!(request.body, b"chunked-request-body");
        b"streamed-response-body".to_vec()
    } else if request.path == "/large-response" {
        vec![b'x'; BODY_CHUNK_LEN * 2 + 3]
    } else if request.method == "HEAD" {
        b"head-body-must-not-be-forwarded".to_vec()
    } else {
        assert!(request.path.starts_with('/'));
        if request.path == "/post" {
            assert_eq!(request.body, b"request-body");
        }
        b"method-ok".to_vec()
    };

    if request.path == "/chunked-response" {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Upstream: streamed\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        write!(stream, "{:x}\r\n", response_body.len()).unwrap();
        stream.write_all(&response_body).unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").unwrap();
    } else if request.path == "/large-response" {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Upstream: large\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .unwrap();
        for chunk in response_body.chunks(BODY_CHUNK_LEN) {
            if stream.write_all(chunk).is_err() {
                return;
            }
            if stream.flush().is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    } else {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Upstream: yes\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .unwrap();
        stream.write_all(&response_body).unwrap();
    }
}

struct HttpRequest {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: Vec<u8>,
}

fn read_http_message(stream: &mut TcpStream) -> HttpRequest {
    let mut bytes = read_until_headers(stream);
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap().to_string();
    let path = request_parts.next().unwrap().to_string();
    let headers = parse_headers(lines);
    let body = read_message_body(stream, &mut bytes, header_end, &headers);
    HttpRequest {
        method,
        path,
        headers,
        body,
    }
}

fn read_http_response(stream: &mut TcpStream, head_only: bool) -> HttpResponse {
    let mut bytes = read_until_headers(stream);
    let Some(header_end) = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
    else {
        return HttpResponse {
            status: 504,
            headers: std::collections::HashMap::new(),
            body: Vec::new(),
        };
    };
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = header_text.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = parse_headers(lines);
    if head_only {
        return HttpResponse {
            status,
            headers,
            body: Vec::new(),
        };
    }
    let body = read_message_body(stream, &mut bytes, header_end, &headers);
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn read_until_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("failed to read HTTP headers: {error}"),
        }
    }
    bytes
}

fn parse_headers<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> std::collections::HashMap<String, String> {
    lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect()
}

fn read_message_body(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    header_end: usize,
    headers: &std::collections::HashMap<String, String>,
) -> Vec<u8> {
    let mut body = bytes.split_off(header_end);
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        while !body.windows(5).any(|window| window == b"0\r\n\r\n") {
            let mut buffer = [0u8; 1024];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "connection closed during chunked body");
            body.extend_from_slice(&buffer[..count]);
        }
        decode_chunked_body(&body)
    } else {
        let length = headers
            .get("content-length")
            .map(|value| value.parse::<usize>().unwrap())
            .unwrap_or(0);
        while body.len() < length {
            let mut buffer = [0u8; 1024];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "connection closed during fixed-length body");
            body.extend_from_slice(&buffer[..count]);
        }
        body.truncate(length);
        body
    }
}

fn decode_chunked_body(bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut cursor = 0;
    loop {
        let line_end = bytes[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .unwrap();
        let length = usize::from_str_radix(
            std::str::from_utf8(&bytes[cursor..cursor + line_end]).unwrap(),
            16,
        )
        .unwrap();
        cursor += line_end + 2;
        if length == 0 {
            break;
        }
        body.extend_from_slice(&bytes[cursor..cursor + length]);
        cursor += length + 2;
    }
    body
}

fn start_host(state_dir: &Path, config_path: &Path, direct_address: &str) -> ProcessOutput {
    start_host_with_timeout(state_dir, config_path, direct_address, None)
}

fn start_host_with_timeout(
    state_dir: &Path,
    config_path: &Path,
    direct_address: &str,
    timeout_milliseconds: Option<&str>,
) -> ProcessOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_locho"));
    command
        .env("LOCHO_STATE_DIR", state_dir)
        .env("LOCHO_TEST_BIND_ADDR", direct_address)
        .arg("host")
        .arg("--config")
        .arg(config_path);
    if let Some(timeout_milliseconds) = timeout_milliseconds {
        command.env("LOCHO_TEST_HTTP_TIMEOUT_MS", timeout_milliseconds);
    }
    ProcessOutput::spawn(command)
}

fn start_attachment(
    state_dir: &Path,
    command_line: &str,
    port: u16,
    direct_address: &str,
) -> ProcessOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_locho"));
    command
        .env("LOCHO_STATE_DIR", state_dir)
        .env("LOCHO_TEST_DIRECT_ADDR", direct_address);
    for argument in command_line
        .split_whitespace()
        .skip_while(|argument| *argument == "locho")
    {
        command.arg(argument);
    }
    command.args(["--tcp", "--listen", &format!("127.0.0.1:{port}")]);
    ProcessOutput::spawn(command)
}

fn start_http_attachment(
    state_dir: &Path,
    command_line: &str,
    port: u16,
    direct_address: &str,
) -> ProcessOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_locho"));
    command
        .env("LOCHO_STATE_DIR", state_dir)
        .env("LOCHO_TEST_DIRECT_ADDR", direct_address);
    for argument in command_line
        .split_whitespace()
        .skip_while(|argument| *argument == "locho")
    {
        command.arg(argument);
    }
    command.args(["--listen", &format!("127.0.0.1:{port}")]);
    ProcessOutput::spawn(command)
}

fn start_ready_attachment(
    state_dir: &Path,
    command_line: &str,
    port: u16,
    direct_address: &str,
) -> ProcessOutput {
    let mut last_output = Vec::new();
    for _ in 0..2 {
        let mut attachment = start_attachment(state_dir, command_line, port, direct_address);
        if attachment.wait_for_ready().is_some() {
            return attachment;
        }
        last_output = attachment.output();
        attachment.stop();
        thread::sleep(Duration::from_secs(1));
    }
    panic!("attachment failed to start after retries; output: {last_output:?}");
}

fn run_cli<const N: usize>(state_dir: &Path, arguments: [&str; N]) {
    let status = Command::new(env!("CARGO_BIN_EXE_locho"))
        .env("LOCHO_STATE_DIR", state_dir)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success());
}

fn parse_attach_command(line: &str) -> (String, String, String) {
    let mut parts = line.split_whitespace();
    assert_eq!(parts.next(), Some("locho"));
    assert_eq!(parts.next(), Some("attach"));
    (
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
        parts.next().unwrap().to_string(),
    )
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(TEST_IO_TIMEOUT)).unwrap();
                return stream;
            }
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("failed to connect to attachment: {error}"),
        }
    }
}

fn accept_with_deadline(listener: &TcpListener) -> (TcpStream, std::net::SocketAddr) {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + TEST_IO_TIMEOUT;
    loop {
        match listener.accept() {
            Ok(connection) => {
                listener.set_nonblocking(false).unwrap();
                return connection;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for upstream request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to accept upstream request: {error}"),
        }
    }
}

fn assert_round_trip(port: u16, payload: &[u8]) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                assert!(error.kind() == std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("failed to connect to attachment: {error}"),
        }
    };
    stream.set_read_timeout(Some(STARTUP_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(STARTUP_TIMEOUT)).unwrap();
    stream.write_all(payload).unwrap();
    let mut response = vec![0u8; payload.len()];
    stream.read_exact(&mut response).unwrap();
    assert_eq!(&response, payload);
}

fn assert_rejected(port: u16) {
    match TcpStream::connect(("127.0.0.1", port)) {
        Ok(mut stream) => {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => panic!("unexpected connection error: {error}"),
    }
}

impl ProcessOutput {
    fn wait_for_ready(&mut self) -> Option<String> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return None;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(line) = self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                if line.contains("Local TCP listener") {
                    return Some(line);
                }
            }
        }
        None
    }
}

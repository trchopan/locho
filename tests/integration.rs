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
fn http_attachment_proxies_requests_and_responses() {
    let state_dir = TestDir::new();
    let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        let (mut stream, _) = upstream_listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("POST /hello?value=1 HTTP/1.1\r\n"));
        assert!(request.contains("x-client: integration\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\nx-upstream: yes\r\nconnection: close\r\n\r\nhello world",
            )
            .unwrap();
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

    let mut client = connect_with_retry(attach_port);
    client
        .write_all(
            b"POST /hello?value=1 HTTP/1.1\r\nHost: localhost\r\nX-Client: integration\r\nContent-Length: 7\r\nConnection: close\r\n\r\nrequest",
        )
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("x-upstream: yes"));
    assert!(
        response.contains("hello world"),
        "unexpected response: {response:?}"
    );

    assert!(upstream_thread.join().is_ok());
    attachment.stop();
    host.stop();
}

fn start_host(state_dir: &Path, config_path: &Path, direct_address: &str) -> ProcessOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_locho"));
    command
        .env("LOCHO_STATE_DIR", state_dir)
        .env("LOCHO_TEST_BIND_ADDR", direct_address)
        .arg("host")
        .arg("--config")
        .arg(config_path);
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
            Ok(stream) => return stream,
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("failed to connect to attachment: {error}"),
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

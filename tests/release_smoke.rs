//! Smoke tests for the binary produced by `cargo build --release`.
//!
//! The deterministic protocol and failure tests live in `integration.rs` and
//! use the `integration-test` feature. This test deliberately does not enable
//! that feature: it exercises the shipped binary's explicit address and TLS
//! paths.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig, ServerConnection, StreamOwned,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "locho-release-smoke-{}-{}",
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
    output: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl ProcessOutput {
    fn spawn(mut command: Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let stdout_output = std::sync::Arc::clone(&output);
        let stderr_output = std::sync::Arc::clone(&output);
        let stderr_sender = sender.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                stdout_output.lock().unwrap().push(line.clone());
                let _ = sender.send(line);
            }
        });
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let line = format!("stderr: {line}");
                stderr_output.lock().unwrap().push(line.clone());
                let _ = stderr_sender.send(line);
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
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(line) = self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                if line.contains(text) {
                    return line;
                }
            }
        }
        panic!("timed out waiting for {text}; output: {:?}", self.output());
    }

    fn output(&self) -> Vec<String> {
        self.output.lock().unwrap().clone()
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProcessOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

struct HttpsUpstream {
    address: std::net::SocketAddr,
    ca_cert: PathBuf,
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HttpsUpstream {
    fn start(state_dir: &Path) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let ca_key_pair = rcgen::KeyPair::generate().unwrap();
        let mut ca_params =
            rcgen::CertificateParams::new(vec!["locho-test-ca".to_string()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key_pair).unwrap();
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let params =
            rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .unwrap();
        let cert = params.signed_by(&key_pair, &ca_cert, &ca_key_pair).unwrap();
        let ca_cert_path = state_dir.join("upstream-ca.pem");
        fs::write(&ca_cert_path, ca_cert.pem()).unwrap();
        let tls_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.der().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der())),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stop_receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_receiver.try_recv().is_ok() || Instant::now() >= deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let connection = ServerConnection::new(Arc::new(tls_config)).unwrap();
            let mut stream = StreamOwned::new(connection, stream);
            stream.get_ref().set_read_timeout(Some(IO_TIMEOUT)).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = match stream.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) => {
                        eprintln!("HTTPS fixture read failed: {error}");
                        return;
                    }
                };
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let body = b"release-smoke-https-response";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Release-Smoke: yes\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        Self {
            address,
            ca_cert: ca_cert_path,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for HttpsUpstream {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[ignore = "requires a separately built release binary"]
fn release_binary_completes_http_tcp_and_rotation_workflow() {
    let binary = release_binary();
    let state_dir = TestDir::new();
    let https_upstream = HttpsUpstream::start(state_dir.path());
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_thread = thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().unwrap();
            handlers.push(thread::spawn(move || {
                stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
                let mut request = [0u8; 5];
                stream.read_exact(&mut request).unwrap();
                stream.write_all(&request).unwrap();
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });

    let config_path = state_dir.path().join("locho.toml");
    let direct_address = format!("127.0.0.1:{}", free_port());
    let unavailable_port = free_port();
    let ca_cert_path = toml_string(&https_upstream.ca_cert);
    fs::write(
        &config_path,
        format!(
            "[[services]]\nname = \"web\"\ntype = \"http\"\nupstream = \"https://127.0.0.1:{}\"\nca_cert = \"{}\"\n\n[[services]]\nname = \"unavailable\"\ntype = \"http\"\nupstream = \"https://127.0.0.1:{unavailable_port}\"\nca_cert = \"{}\"\n\n[[services]]\nname = \"database\"\ntype = \"tcp\"\nendpoint = \"{upstream_address}\"\n",
            https_upstream.address.port(),
            ca_cert_path,
            ca_cert_path
        ),
    )
    .unwrap();

    let mut host = start_process(
        &binary,
        state_dir.path(),
        [
            "host",
            "--config",
            config_path.to_str().unwrap(),
            "--bind-address",
            &direct_address,
        ],
    );
    let web_command = host.wait_for_attach("web");
    let unavailable_command = host.wait_for_attach("unavailable");
    let tcp_command = host.wait_for_attach("database");

    let (host_id, _, web_secret) = parse_attach_command(&web_command);
    let diagnostics = run_cli_output(
        &binary,
        state_dir.path(),
        [
            "diagnose",
            "--host-id",
            &host_id,
            "--direct-address",
            &direct_address,
        ],
    );
    let diagnostics_stdout = String::from_utf8_lossy(&diagnostics.stdout);
    assert!(diagnostics.status.success());
    assert!(
        diagnostics_stdout.contains(&format!("transport path: direct({direct_address})")),
        "unexpected diagnostics output: {}",
        diagnostics_stdout
    );
    assert!(!diagnostics_stdout.contains(&web_secret));

    let http_port = free_port();
    let mut http = start_attachment(
        &binary,
        state_dir.path(),
        &web_command,
        &direct_address,
        http_port,
        false,
    );
    http.wait_for("Local proxy:");
    let response = http_get(http_port, "/");
    assert_eq!(
        response.0,
        200,
        "HTTP smoke output: {:?}; host output: {:?}",
        http.output(),
        host.output()
    );
    assert!(
        response.1.contains("release-smoke-https-response"),
        "unexpected HTTPS upstream body: {}; output: {:?}",
        response.1,
        http.output()
    );

    let unavailable_listener_port = free_port();
    let mut unavailable = start_attachment(
        &binary,
        state_dir.path(),
        &unavailable_command,
        &direct_address,
        unavailable_listener_port,
        false,
    );
    unavailable.wait_for("Local proxy:");
    assert_eq!(http_get(unavailable_listener_port, "/unavailable").0, 502);

    let tcp_port = free_port();
    let mut tcp = start_attachment(
        &binary,
        state_dir.path(),
        &tcp_command,
        &direct_address,
        tcp_port,
        true,
    );
    tcp.wait_for("Local TCP listener");
    let first = thread::spawn(move || try_round_trip(tcp_port, b"first"));
    let second = thread::spawn(move || try_round_trip(tcp_port, b"other"));
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert!(upstream_thread.join().is_ok());

    host.stop();
    http.stop();
    unavailable.stop();
    tcp.stop();
    run_cli(&binary, state_dir.path(), ["rotate-secret", "database"]);

    let replacement_upstream = TcpListener::bind(upstream_address).unwrap();
    let replacement_upstream_thread = thread::spawn(move || {
        let (mut stream, _) = replacement_upstream.accept().unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        let mut request = [0u8; 6];
        stream.read_exact(&mut request).unwrap();
        stream.write_all(&request).unwrap();
    });

    let mut restarted_host = start_process(
        &binary,
        state_dir.path(),
        [
            "host",
            "--config",
            config_path.to_str().unwrap(),
            "--bind-address",
            &direct_address,
        ],
    );
    let rotated_command = restarted_host.wait_for_attach("database");
    let (_, _, old_secret) = parse_attach_command(&tcp_command);
    let (_, _, new_secret) = parse_attach_command(&rotated_command);
    assert_ne!(old_secret, new_secret);

    let new_port = free_port();
    let mut new_tcp = start_attachment(
        &binary,
        state_dir.path(),
        &rotated_command,
        &direct_address,
        new_port,
        true,
    );
    new_tcp.wait_for("Local TCP listener");
    if let Err(error) = try_round_trip(new_port, b"second") {
        panic!(
            "TCP round trip failed: {error}; attachment output: {:?}; host output: {:?}",
            new_tcp.output(),
            restarted_host.output()
        );
    }

    assert!(replacement_upstream_thread.join().is_ok());
    restarted_host.stop();
    new_tcp.stop();
}

impl ProcessOutput {
    fn wait_for_attach(&self, service: &str) -> String {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(line) = self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                if line.starts_with("locho attach ")
                    && line.split_whitespace().any(|part| part == service)
                {
                    return line;
                }
            }
        }
        panic!(
            "timed out waiting for {service} attach command; output: {:?}",
            self.output()
        );
    }
}

fn release_binary() -> PathBuf {
    let path = std::env::var_os("LOCHO_TEST_BINARY")
        .map(PathBuf::from)
        .expect("LOCHO_TEST_BINARY must point to cargo build --release output");
    if path.exists() {
        return path;
    }
    #[cfg(windows)]
    {
        let mut executable = path.clone();
        executable.set_extension("exe");
        if executable.exists() {
            return executable;
        }
    }
    panic!("release binary does not exist: {}", path.display());
}

fn start_process<const N: usize>(
    binary: &Path,
    state_dir: &Path,
    arguments: [&str; N],
) -> ProcessOutput {
    let mut command = Command::new(binary);
    command.env("LOCHO_STATE_DIR", state_dir).args(arguments);
    ProcessOutput::spawn(command)
}

fn start_attachment(
    binary: &Path,
    state_dir: &Path,
    attach_command: &str,
    direct_address: &str,
    port: u16,
    tcp: bool,
) -> ProcessOutput {
    let mut command = Command::new(binary);
    command.env("LOCHO_STATE_DIR", state_dir);
    for argument in attach_command.split_whitespace().skip(1) {
        command.arg(argument);
    }
    if !attach_command
        .split_whitespace()
        .any(|argument| argument == "--direct-address")
    {
        command.args(["--direct-address", direct_address]);
    }
    if tcp {
        command.arg("--tcp");
    }
    command.args(["--listen", &format!("127.0.0.1:{port}")]);
    ProcessOutput::spawn(command)
}

fn run_cli<const N: usize>(binary: &Path, state_dir: &Path, arguments: [&str; N]) {
    let status = Command::new(binary)
        .env("LOCHO_STATE_DIR", state_dir)
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success());
}

fn run_cli_output<const N: usize>(
    binary: &Path,
    state_dir: &Path,
    arguments: [&str; N],
) -> std::process::Output {
    Command::new(binary)
        .env("LOCHO_STATE_DIR", state_dir)
        .args(arguments)
        .output()
        .unwrap()
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

fn toml_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
                return stream;
            }
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("failed to connect to attachment: {error}"),
        }
    }
}

fn try_round_trip(port: u16, payload: &[u8]) -> std::io::Result<()> {
    let mut stream = connect_with_retry(port);
    stream.write_all(payload)?;
    let mut response = vec![0; payload.len()];
    stream.read_exact(&mut response)?;
    assert_eq!(response, payload);
    Ok(())
}

fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = connect_with_retry(port);
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let response = String::from_utf8(response).unwrap();
    let status = response
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, response)
}

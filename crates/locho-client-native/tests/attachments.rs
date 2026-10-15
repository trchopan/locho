use iroh::{endpoint::presets, Endpoint, RelayMode, SecretKey};
use locho_client_native::{Attachment, Client, ClientError, SessionState, TcpOptions};
use locho_core::protocol::*;
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);

struct Host {
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
    address: SocketAddr,
}
impl Host {
    async fn start(key: SecretKey, address: Option<SocketAddr>) -> Self {
        let mut builder = Endpoint::builder(presets::N0)
            .secret_key(key)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()]);
        if let Some(SocketAddr::V4(address)) = address {
            builder = builder.bind_addr(address).unwrap();
        }
        let endpoint = builder.bind().await.unwrap();
        let port = endpoint
            .bound_sockets()
            .into_iter()
            .find(|a| a.is_ipv4())
            .unwrap()
            .port();
        let address = (Ipv4Addr::LOCALHOST, port).into();
        let server = endpoint.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Some(incoming) = server.accept().await {
                connections.spawn(async move {
                    let connection = incoming.await.unwrap();
                    let mut streams = JoinSet::new();
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        streams.spawn(async move {
                            let request: StreamRequestHead = read_json_head(&mut recv, MAX_HEAD_LEN).await.unwrap();
                            let StreamRequestHead::Tcp(request) = request else { panic!("wrong service kind") };
                            let status = if request.service != "api" { 404 }
                                else if request.secret_proof != locho_core::auth::secret_proof("private-secret") { 403 }
                                else { 200 };
                            write_json_head(&mut send, &LochoResponseHead { version: 3, status, headers: vec![], body_len: Some(0) }).await.unwrap();
                            if status != 200 { send.finish().unwrap(); let _ = send.stopped().await; return; }
                            let mut request = Vec::new();
                            loop {
                                let mut byte = [0];
                                if recv.read_exact(&mut byte).await.is_err() { return; }
                                request.push(byte[0]);
                                if request.ends_with(b"\r\n\r\n") { break; }
                                if request.len() > 8192 { return; }
                            }
                            let text = String::from_utf8(request.clone()).unwrap();
                            let length = text.lines().find_map(|line| line.strip_prefix("Content-Length: "))
                                .map(|value| value.parse::<usize>().unwrap()).unwrap_or(0);
                            let mut body = vec![0; length];
                            if recv.read_exact(&mut body).await.is_err() { return; }
                            if text.starts_with("GET /hang ") {
                                let _ = recv.read_to_end(1024).await; return;
                            }
                            let payload = if length == 0 { b"{\"ok\":true}".to_vec() } else { body };
                            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
                            if send.write_all(head.as_bytes()).await.is_err() { return; }
                            if send.write_all(&payload).await.is_err() { return; }
                            send.finish().unwrap(); let _ = send.stopped().await;
                        });
                    }
                    streams.abort_all(); while streams.join_next().await.is_some() {}
                });
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            endpoint,
            task,
            address,
        }
    }
    fn options(&self) -> TcpOptions {
        let mut options = TcpOptions::new(
            self.endpoint.id().to_string(),
            "api:tcp:private-secret".into(),
        );
        options.direct_address = Some(self.address);
        options
    }
    async fn close(self) {
        self.endpoint.close().await;
        drop(self.endpoint);
        self.task.await.unwrap();
    }
}

async fn request(attachment: &Attachment, body: &[u8]) -> Vec<u8> {
    timeout(DEADLINE, async {
        let mut socket = TcpStream::connect(attachment.local_addr()).await.unwrap();
        let method = if body.is_empty() { "GET" } else { "POST" };
        let head = format!(
            "{method} /health HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n\r\n",
            attachment.local_addr(),
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        response
    })
    .await
    .unwrap()
}

async fn released(address: SocketAddr) {
    timeout(DEADLINE, async {
        loop {
            match TcpListener::bind(address).await {
                Ok(listener) => {
                    drop(listener);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("listener port was not released");
}

#[tokio::test]
async fn get_post_concurrent_requests_and_independent_close() {
    let host = Host::start(SecretKey::generate(), None).await;
    let client = Client::new();
    let ticket = locho_client_native::Ticket::new(
        &host.endpoint.id().to_string(),
        "api:tcp:private-secret",
        vec!["https://127.0.0.1:1/".into()],
    )
    .unwrap();
    let mut options = TcpOptions::from_ticket(&ticket).unwrap();
    options.direct_address = Some(host.address);
    let first = client.attach_tcp(options, DEADLINE).await.unwrap();
    let second = client.attach_tcp(host.options(), DEADLINE).await.unwrap();
    assert_eq!(first.local_addr().ip(), Ipv4Addr::LOCALHOST);
    assert_ne!(first.local_addr().port(), 0);
    let (get, post) = tokio::join!(
        request(&first, b""),
        request(&first, b"{\"note\":\"exact\"}")
    );
    assert!(get.ends_with(b"{\"ok\":true}"));
    assert!(post.ends_with(b"{\"note\":\"exact\"}"));
    let (a, b) = tokio::join!(first.close(), first.close());
    a.unwrap();
    b.unwrap();
    first.close().await.unwrap();
    released(first.local_addr()).await;
    assert!(request(&second, b"").await.ends_with(b"{\"ok\":true}"));
    let mut late = first.subscribe();
    assert_eq!(late.borrow_and_update().state, SessionState::Closed);
    let (a, b) = tokio::join!(client.close(), client.close());
    a.unwrap();
    b.unwrap();
    client.close().await.unwrap();
    released(second.local_addr()).await;
    assert_eq!(
        client.start_tcp(host.options()).await.err(),
        Some(ClientError::Closed)
    );
    host.close().await;
}

#[tokio::test]
async fn validation_occupied_port_and_secret_redaction() {
    let host = Host::start(SecretKey::generate(), None).await;
    let client = Client::new();
    let mut options = host.options();
    options.host_id = "invalid-private-secret".into();
    assert_eq!(
        client.start_tcp(options).await.err(),
        Some(ClientError::InvalidHostId)
    );
    let mut options = host.options();
    options.capability = "api:http:private-secret".into();
    assert_eq!(
        client.start_tcp(options).await.err(),
        Some(ClientError::UnsupportedService)
    );
    let mut options = host.options();
    options.capability = "api:private-secret:x".into();
    let error = client.start_tcp(options.clone()).await.err().unwrap();
    assert!(!format!("{options:?} {error} {error:?}").contains("private-secret"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut options = host.options();
    options.local_port = listener.local_addr().unwrap().port();
    assert_eq!(
        client.start_tcp(options).await.err(),
        Some(ClientError::ListenerBind)
    );
    let mut options = host.options();
    options.direct_address = Some("0.0.0.0:0".parse().unwrap());
    assert_eq!(
        client.start_tcp(options).await.err(),
        Some(ClientError::InvalidDirectAddress)
    );
    assert_eq!(
        client
            .attach_tcp(host.options(), Duration::ZERO)
            .await
            .err(),
        Some(ClientError::InvalidTimeout)
    );
    client.close().await.unwrap();
    host.close().await;
}

#[tokio::test]
async fn timeout_wait_and_cancelled_start_release_resources() {
    let host = Host::start(SecretKey::generate(), None).await;
    let mut options = host.options();
    host.close().await;
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = reservation.local_addr().unwrap();
    options.local_port = address.port();
    drop(reservation);
    let client = Client::new();
    assert_eq!(
        client
            .attach_tcp(options.clone(), Duration::from_millis(30))
            .await
            .err(),
        Some(ClientError::StartupTimeout)
    );
    released(address).await;
    let attachment = client.start_tcp(options.clone()).await.unwrap();
    assert_eq!(
        attachment.wait_connected(Duration::from_millis(20)).await,
        Err(ClientError::StartupTimeout)
    );
    assert!(TcpListener::bind(address).await.is_err());
    attachment.close().await.unwrap();
    released(address).await;
    let startup_client = client.clone();
    let startup = tokio::spawn(async move { startup_client.attach_tcp(options, DEADLINE).await });
    timeout(DEADLINE, async {
        while TcpStream::connect(address).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    startup.abort();
    let _ = startup.await;
    released(address).await;
    client.close().await.unwrap();
}

#[tokio::test]
async fn client_close_races_start_and_dropped_owners_stop_listeners() {
    let host = Host::start(SecretKey::generate(), None).await;
    for _ in 0..5 {
        let client = Client::new();
        let (startup, close) = tokio::join!(client.start_tcp(host.options()), client.close());
        close.unwrap();
        match startup {
            Ok(attachment) => {
                attachment.close().await.unwrap();
                released(attachment.local_addr()).await;
            }
            Err(error) => assert!(matches!(
                error,
                ClientError::Cancelled | ClientError::Closed
            )),
        }
    }
    let client = Client::new();
    let attachment = client.attach_tcp(host.options(), DEADLINE).await.unwrap();
    let addr = attachment.local_addr();
    let mut status = attachment.subscribe();
    drop(attachment);
    released(addr).await;
    // Listener release precedes peer/task cleanup. Wait on the lifecycle signal
    // independently rather than assuming a free port means cleanup has completed.
    timeout(Duration::from_secs(13), async {
        loop {
            if status.borrow_and_update().state == SessionState::Closed {
                break;
            }
            status.changed().await.expect("status ended before closure");
        }
    })
    .await
    .expect("dropped attachment did not finish cleanup");
    assert_eq!(status.borrow().state, SessionState::Closed);
    assert_eq!(status.borrow().terminal_error, None);
    let attachment = client.attach_tcp(host.options(), DEADLINE).await.unwrap();
    let addr = attachment.local_addr();
    drop(client);
    released(addr).await;
    attachment.close().await.unwrap();
    host.close().await;
}

#[tokio::test]
async fn restart_preserves_listener_port_and_active_traffic_closes() {
    let key = SecretKey::generate();
    let host = Host::start(key.clone(), None).await;
    let options = host.options();
    let address = host.address;
    let client = Client::new();
    let attachment = client.attach_tcp(options, DEADLINE).await.unwrap();
    let local = attachment.local_addr();
    let mut active = TcpStream::connect(local).await.unwrap();
    active
        .write_all(b"GET /hang HTTP/1.1\r\nHost: sample-api\r\n\r\n")
        .await
        .unwrap();
    host.close().await;
    let mut body = Vec::new();
    let _ = timeout(DEADLINE, active.read_to_end(&mut body))
        .await
        .unwrap();
    let replacement = Host::start(key, Some(address)).await;
    timeout(Duration::from_secs(15), async {
        loop {
            if attachment
                .wait_connected(Duration::from_millis(200))
                .await
                .is_ok()
            {
                let probe = async {
                    let mut socket = TcpStream::connect(local).await.ok()?;
                    socket
                        .write_all(b"GET /health HTTP/1.1\r\nHost: sample-api\r\n\r\n")
                        .await
                        .ok()?;
                    let mut body = Vec::new();
                    socket.read_to_end(&mut body).await.ok()?;
                    body.ends_with(b"{\"ok\":true}").then_some(())
                };
                if timeout(DEADLINE, probe).await.ok().flatten().is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(attachment.local_addr(), local);
    client.close().await.unwrap();
    released(local).await;
    replacement.close().await;
}

#[tokio::test]
async fn service_rejection_is_socket_failure_not_peer_failure() {
    let host = Host::start(SecretKey::generate(), None).await;
    let client = Client::new();
    let mut options = host.options();
    options.capability = "api:tcp:wrong-secret".into();
    let attachment = client.attach_tcp(options, DEADLINE).await.unwrap();
    let mut socket = TcpStream::connect(attachment.local_addr()).await.unwrap();
    socket
        .write_all(b"GET /health HTTP/1.1\r\nHost: sample-api\r\n\r\n")
        .await
        .unwrap();
    let mut bytes = Vec::new();
    let _ = timeout(DEADLINE, socket.read_to_end(&mut bytes))
        .await
        .unwrap();
    assert!(bytes.is_empty());
    assert_eq!(attachment.snapshot().state, SessionState::Connected);
    client.close().await.unwrap();
    host.close().await;
}

#[tokio::test]
async fn close_during_active_request_and_repeated_cycles_release_ports() {
    let host = Host::start(SecretKey::generate(), None).await;
    let client = Client::new();
    for _ in 0..5 {
        let attachment = client.attach_tcp(host.options(), DEADLINE).await.unwrap();
        let mut socket = TcpStream::connect(attachment.local_addr()).await.unwrap();
        socket
            .write_all(b"GET /hang HTTP/1.1\r\nHost: sample-api\r\n\r\n")
            .await
            .unwrap();
        attachment.close().await.unwrap();
        let mut body = Vec::new();
        let _ = timeout(DEADLINE, socket.read_to_end(&mut body))
            .await
            .unwrap();
        released(attachment.local_addr()).await;
        assert_eq!(
            attachment.wait_connected(DEADLINE).await,
            Err(ClientError::Closed)
        );
        assert_eq!(attachment.snapshot().terminal_error, None);
    }
    client.close().await.unwrap();
    host.close().await;
}

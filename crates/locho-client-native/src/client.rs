use crate::{forward::serve_tcp, ClientError};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, EndpointId};
use locho_core::{
    capability,
    relay::RelaySettings,
    service::ServiceType,
    session::{PeerSession, SessionState, SessionStatus, TransportPath},
};
use std::{
    collections::HashMap,
    fmt,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{watch, Semaphore},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const CLEANUP_BUDGET: Duration = Duration::from_secs(12);

#[derive(Clone)]
pub struct TcpOptions {
    pub host_id: String,
    pub capability: String,
    pub local_port: u16,
    pub direct_address: Option<SocketAddr>,
    pub relay_settings: Option<RelaySettings>,
}
impl TcpOptions {
    pub fn new(host_id: String, capability: String) -> Self {
        Self {
            host_id,
            capability,
            local_port: 0,
            direct_address: None,
            relay_settings: None,
        }
    }
}
impl fmt::Debug for TcpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpOptions")
            .field("host_id", &"[UNVALIDATED]")
            .field("capability", &"[REDACTED]")
            .field("local_port", &self.local_port)
            .field("direct_address", &self.direct_address)
            .field("relay_settings", &self.relay_settings)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentStatus {
    pub revision: u64,
    pub state: SessionState,
    pub local_addr: Option<SocketAddr>,
    pub path: TransportPath,
    pub peer: Option<SessionStatus>,
    pub terminal_error: Option<ClientError>,
}

struct Control {
    cancel: CancellationToken,
    status: watch::Sender<AttachmentStatus>,
}
impl Control {
    fn new() -> Arc<Self> {
        let (status, _) = watch::channel(AttachmentStatus {
            revision: 0,
            state: SessionState::Starting,
            local_addr: None,
            path: TransportPath::Unknown,
            peer: None,
            terminal_error: None,
        });
        Arc::new(Self {
            cancel: CancellationToken::new(),
            status,
        })
    }
    fn closing(&self) {
        self.status.send_modify(|s| {
            if s.state != SessionState::Closed {
                s.revision += 1;
                s.state = SessionState::Closing;
            }
        });
    }
    fn closed(&self, error: Option<ClientError>) {
        self.status.send_modify(|s| {
            s.revision += 1;
            s.state = SessionState::Closed;
            s.path = TransportPath::Unknown;
            s.terminal_error = error;
        });
    }
    async fn wait_closed(&self) -> Result<(), ClientError> {
        let mut status = self.status.subscribe();
        timeout(CLEANUP_BUDGET + Duration::from_secs(1), async {
            loop {
                let snapshot = status.borrow_and_update().clone();
                if snapshot.state == SessionState::Closed {
                    return snapshot.terminal_error.map_or(Ok(()), Err);
                }
                status
                    .changed()
                    .await
                    .map_err(|_| ClientError::TaskFailed)?;
            }
        })
        .await
        .map_err(|_| ClientError::ShutdownTimeout)?
    }
}

struct Registry {
    closed: bool,
    next: u64,
    attachments: HashMap<u64, Arc<Control>>,
    close_error: Option<ClientError>,
}
struct ClientOwner {
    registry: Mutex<Registry>,
}
impl Drop for ClientOwner {
    fn drop(&mut self) {
        for control in self.registry.get_mut().unwrap().attachments.values() {
            control.cancel.cancel();
        }
    }
}
#[derive(Clone)]
pub struct Client {
    owner: Arc<ClientOwner>,
}
impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
impl Client {
    pub fn new() -> Self {
        Self {
            owner: Arc::new(ClientOwner {
                registry: Mutex::new(Registry {
                    closed: false,
                    next: 0,
                    attachments: HashMap::new(),
                    close_error: None,
                }),
            }),
        }
    }

    pub async fn start_tcp(&self, options: TcpOptions) -> Result<Attachment, ClientError> {
        self.start(options, None, Duration::from_secs(15)).await
    }
    pub async fn attach_tcp(
        &self,
        options: TcpOptions,
        startup_timeout: Duration,
    ) -> Result<Attachment, ClientError> {
        self.start(options, Some(startup_timeout), startup_timeout)
            .await
    }
    async fn start(
        &self,
        options: TcpOptions,
        connect: Option<Duration>,
        budget: Duration,
    ) -> Result<Attachment, ClientError> {
        if budget.is_zero() || budget > Duration::from_secs(300) {
            return Err(ClientError::InvalidTimeout);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ClientError::RuntimeRequired)?;
        let control = Control::new();
        let id = {
            let mut registry = self.owner.registry.lock().unwrap();
            if registry.closed {
                return Err(ClientError::Closed);
            }
            registry.next += 1;
            let id = registry.next;
            registry.attachments.insert(id, control.clone());
            id
        };
        let mut guard = StartupGuard {
            control: control.clone(),
            armed: true,
        };
        let owner = Arc::downgrade(&self.owner);
        let task_control = control.clone();
        // Startup is owned by this task, not the calling future: dropping a caller
        // cancels through the guard and the task cleans up any bound resources.
        let (sender, receiver) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _completion = Completion {
                control: task_control.clone(),
                owner,
                id,
            };
            run_attachment(options, connect, budget, task_control, sender).await;
        });
        let result = receiver.await.map_err(|_| ClientError::TaskFailed)?;
        match result {
            Ok(local_addr) => {
                guard.armed = false;
                Ok(Attachment {
                    owner: Arc::new(AttachmentOwner { control }),
                    local_addr,
                })
            }
            Err(error) => {
                control.wait_closed().await.ok();
                guard.armed = false;
                Err(error)
            }
        }
    }
    pub async fn close(&self) -> Result<(), ClientError> {
        let controls: Vec<_> = {
            let mut registry = self.owner.registry.lock().unwrap();
            registry.closed = true;
            registry.attachments.values().cloned().collect()
        };
        for control in &controls {
            control.cancel.cancel();
        }
        let mut error = None;
        let waiting = async {
            for control in controls {
                if let Err(value) = control.wait_closed().await {
                    error.get_or_insert(value);
                }
            }
        };
        if timeout(CLEANUP_BUDGET + Duration::from_secs(1), waiting)
            .await
            .is_err()
        {
            error.get_or_insert(ClientError::ShutdownTimeout);
        }
        let mut registry = self.owner.registry.lock().unwrap();
        if let Some(error) = error {
            registry.close_error.get_or_insert(error);
        }
        registry.close_error.map_or(Ok(()), Err)
    }
}

struct StartupGuard {
    control: Arc<Control>,
    armed: bool,
}
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if self.armed {
            self.control.cancel.cancel();
        }
    }
}
struct Completion {
    control: Arc<Control>,
    owner: Weak<ClientOwner>,
    id: u64,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if self.control.status.borrow().state != SessionState::Closed {
            self.control.cancel.cancel();
            self.control.closed(Some(ClientError::TaskFailed));
        }
        if let Some(owner) = self.owner.upgrade() {
            let mut registry = owner.registry.lock().unwrap();
            if registry.closed {
                if let Some(error) = self.control.status.borrow().terminal_error {
                    registry.close_error.get_or_insert(error);
                }
            }
            registry.attachments.remove(&self.id);
        }
    }
}
struct AttachmentOwner {
    control: Arc<Control>,
}

struct TaskCleanup(Vec<tokio::task::AbortHandle>);
impl Drop for TaskCleanup {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
impl Drop for AttachmentOwner {
    fn drop(&mut self) {
        self.control.cancel.cancel();
    }
}
#[derive(Clone)]
pub struct Attachment {
    owner: Arc<AttachmentOwner>,
    local_addr: SocketAddr,
}
impl Attachment {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
    pub fn snapshot(&self) -> AttachmentStatus {
        self.owner.control.status.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<AttachmentStatus> {
        self.owner.control.status.subscribe()
    }
    pub async fn wait_connected(&self, deadline: Duration) -> Result<(), ClientError> {
        let mut status = self.subscribe();
        timeout(deadline, async {
            loop {
                if self.owner.control.cancel.is_cancelled() {
                    return Err(ClientError::Closed);
                }
                let snapshot = status.borrow_and_update().clone();
                match snapshot.state {
                    SessionState::Connected => return Ok(()),
                    SessionState::Closing | SessionState::Closed | SessionState::Failed => {
                        return Err(snapshot.terminal_error.unwrap_or(ClientError::Closed))
                    }
                    _ => {}
                }
                status
                    .changed()
                    .await
                    .map_err(|_| ClientError::TaskFailed)?;
            }
        })
        .await
        .map_err(|_| ClientError::StartupTimeout)?
    }
    pub async fn close(&self) -> Result<(), ClientError> {
        self.owner.control.cancel.cancel();
        self.owner.control.wait_closed().await
    }
}

async fn run_attachment(
    options: TcpOptions,
    connect: Option<Duration>,
    budget: Duration,
    control: Arc<Control>,
    sender: tokio::sync::oneshot::Sender<Result<SocketAddr, ClientError>>,
) {
    let setup = async {
        let host: EndpointId = options
            .host_id
            .parse()
            .map_err(|_| ClientError::InvalidHostId)?;
        let capability =
            capability::parse(&options.capability).map_err(ClientError::InvalidCapability)?;
        if capability.service_type != ServiceType::Tcp {
            return Err(ClientError::UnsupportedService);
        }
        if options
            .direct_address
            .is_some_and(|a| a.port() == 0 || a.ip().is_unspecified() || a.ip().is_multicast())
        {
            return Err(ClientError::InvalidDirectAddress);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.local_port))
            .await
            .map_err(|_| ClientError::ListenerBind)?;
        let addr = listener
            .local_addr()
            .map_err(|_| ClientError::ListenerBind)?;
        let mut builder = Endpoint::builder(presets::N0);
        if let Some(settings) = options.relay_settings {
            builder = builder.relay_mode(settings.mode());
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|_| ClientError::EndpointBind)?;
        let target = options
            .direct_address
            .map(|a| EndpointAddr::new(host).with_ip_addr(a))
            .unwrap_or_else(|| EndpointAddr::new(host));
        Ok((listener, addr, endpoint, target, capability))
    };
    let started = tokio::time::Instant::now();
    let resources = tokio::select! {
        biased;
        _ = control.cancel.cancelled() => Err(ClientError::Cancelled),
        result = timeout(budget, setup) => result.unwrap_or(Err(ClientError::StartupTimeout)),
    };
    let (listener, addr, endpoint, target, capability) = match resources {
        Ok(resources) => resources,
        Err(error) => {
            control.closed(if error == ClientError::Cancelled {
                None
            } else {
                Some(error)
            });
            let _ = sender.send(Err(error));
            return;
        }
    };
    let (session, driver) = PeerSession::start(endpoint, target);
    let mut driver_task = tokio::spawn(driver.run());
    let mut peer = session.subscribe();
    control.status.send_modify(|s| {
        s.revision += 1;
        s.local_addr = Some(addr);
        s.state = SessionState::Connecting;
    });
    let mut listener_task = tokio::spawn(serve_tcp(
        listener,
        session.clone(),
        capability,
        Arc::new(Semaphore::new(512)),
        control.cancel.clone(),
    ));
    let _tasks = TaskCleanup(vec![
        driver_task.abort_handle(),
        listener_task.abort_handle(),
    ]);
    let mut sender = Some(sender);
    let mut failure = None;
    if connect.is_none() {
        let _ = sender.take().unwrap().send(Ok(addr));
    }
    let startup_deadline = started + budget;
    let mut listener_done = false;
    let mut driver_done = false;
    loop {
        let snapshot = peer.borrow_and_update().clone();
        control.status.send_modify(|s| {
            s.revision += 1;
            s.state = snapshot.state;
            s.path = snapshot.path;
            s.peer = Some(snapshot.clone());
        });
        if snapshot.state == SessionState::Closed {
            failure = Some(
                snapshot
                    .terminal_error
                    .map(ClientError::Peer)
                    .unwrap_or(ClientError::TaskFailed),
            );
            break;
        }
        if snapshot.state == SessionState::Connected {
            if let Some(sender) = sender.take() {
                let _ = sender.send(Ok(addr));
            }
        }
        tokio::select! {
            biased;
            _ = control.cancel.cancelled() => break,
            result = &mut driver_task => { driver_done = true; let _ = result; failure = Some(ClientError::TaskFailed); break; }
            result = &mut listener_task => {
                listener_done = true;
                failure = Some(match result { Ok(Err(_)) => ClientError::ListenerFailed, _ => ClientError::TaskFailed }); break;
            }
            _ = tokio::time::sleep_until(startup_deadline), if sender.is_some() => { failure = Some(ClientError::StartupTimeout); break; }
            changed = peer.changed() => { if changed.is_err() { failure = Some(ClientError::TaskFailed); break; } }
        }
    }
    control.cancel.cancel();
    if let Some(error) = failure {
        control.status.send_modify(|s| {
            s.revision += 1;
            s.state = SessionState::Failed;
            s.terminal_error = Some(error);
        });
    }
    control.closing();
    let cleanup = timeout(CLEANUP_BUDGET, async {
        if let Err(error) = session.close().await {
            failure.get_or_insert(ClientError::Peer(error));
        }
        if !listener_done {
            let _ = (&mut listener_task).await;
            listener_done = true;
        }
        if !driver_done {
            let _ = (&mut driver_task).await;
            driver_done = true;
        }
    })
    .await;
    if cleanup.is_err() {
        listener_task.abort();
        driver_task.abort();
        if !listener_done {
            let _ = listener_task.await;
        }
        if !driver_done {
            let _ = driver_task.await;
        }
        failure.get_or_insert(ClientError::ShutdownTimeout);
    }
    if let Some(sender) = sender {
        let _ = sender.send(Err(failure.unwrap_or(ClientError::Cancelled)));
    }
    control.closed(failure);
}

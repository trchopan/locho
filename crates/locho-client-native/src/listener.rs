//! Cancellation-aware recovery from transient local accept failures.

use std::{future::Future, io, time::Duration};
use tokio_util::sync::CancellationToken;

fn retryable(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    ) {
        return true;
    }
    false
}

/// Retry state survives cancellation by a listener owner's task-reaping select.
/// Warn once per failure episode, then report recovery without logging peers.
pub struct AcceptRetry {
    delay: Duration,
    next_attempt: Option<tokio::time::Instant>,
}

impl Default for AcceptRetry {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(100),
            next_attempt: None,
        }
    }
}

impl AcceptRetry {
    /// Keep the listener bound while resources recover. Permanent failures
    /// propagate; cancellation returns None without delaying shutdown.
    pub async fn accept<F, Fut, T>(
        &mut self,
        mut accept: F,
        shutdown: &CancellationToken,
    ) -> io::Result<Option<T>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = io::Result<T>>,
    {
        loop {
            if let Some(deadline) = self.next_attempt {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(None),
                    _ = tokio::time::sleep_until(deadline) => {},
                }
            }
            let result = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(None),
                result = accept() => result,
            };
            match result {
                Ok(value) => {
                    if self.next_attempt.is_some() {
                        tracing::info!("local accept recovered");
                    }
                    *self = Self::default();
                    return Ok(Some(value));
                }
                Err(error) if retryable(&error) => {
                    if self.next_attempt.is_none() {
                        tracing::warn!(kind = ?error.kind(), os_error = error.raw_os_error(), "local accept temporarily unavailable; retrying");
                    }
                    self.next_attempt = Some(tokio::time::Instant::now() + self.delay);
                    self.delay = (self.delay * 2).min(Duration::from_secs(1));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Current soft descriptor limit; this does not change process-global limits.
pub fn descriptor_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: getrlimit writes exactly one rlimit to a valid output pointer.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } == 0 {
            // SAFETY: successful getrlimit initialized the output.
            let limit = unsafe { limit.assume_init() };
            if limit.rlim_cur != libc::RLIM_INFINITY {
                return Some(limit.rlim_cur);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn accept_with_retry<F, Fut, T>(
        accept: F,
        shutdown: &CancellationToken,
    ) -> io::Result<Option<T>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = io::Result<T>>,
    {
        AcceptRetry::default().accept(accept, shutdown).await
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_survives_outer_select_cancellation() {
        let shutdown = CancellationToken::new();
        let mut retry = AcceptRetry::default();
        let mut attempts = 0;
        for _ in 0..5 {
            let _ = tokio::time::timeout(
                Duration::from_millis(10),
                retry.accept(
                    || {
                        attempts += 1;
                        std::future::ready(Err::<(), _>(io::Error::from(io::ErrorKind::WouldBlock)))
                    },
                    &shutdown,
                ),
            )
            .await;
        }
        assert_eq!(attempts, 1);
        assert_eq!(retry.delay, Duration::from_millis(200));
        shutdown.cancel();
        assert!(retry
            .accept(|| std::future::ready(Ok(())), &shutdown)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_is_capped_and_resets_after_recovery() {
        let shutdown = CancellationToken::new();
        let mut retry = AcceptRetry::default();
        let mut attempts = 0;
        let started = tokio::time::Instant::now();
        let value = retry
            .accept(
                || {
                    attempts += 1;
                    std::future::ready(if attempts <= 6 {
                        Err(io::Error::from(io::ErrorKind::WouldBlock))
                    } else {
                        Ok(42)
                    })
                },
                &shutdown,
            )
            .await
            .unwrap();
        assert_eq!(value, Some(42));
        assert_eq!(started.elapsed(), Duration::from_millis(3500));
        assert_eq!(retry.delay, Duration::from_millis(100));
        assert!(retry.next_attempt.is_none());
    }

    #[tokio::test]
    async fn transient_failure_recovers_without_dropping_listener() {
        let mut attempts = 0;
        let started = std::time::Instant::now();
        let result = accept_with_retry(
            || {
                attempts += 1;
                std::future::ready(if attempts == 1 {
                    Err(io::Error::from(io::ErrorKind::ConnectionAborted))
                } else {
                    Ok(42)
                })
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result, Some(42));
        assert_eq!(attempts, 2);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn cancellation_interrupts_resource_backoff() {
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            accept_with_retry(
                || std::future::ready(Err::<(), _>(io::Error::from(io::ErrorKind::WouldBlock))),
                &shutdown,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn permanent_failures_propagate() {
        let error = accept_with_retry(
            || {
                std::future::ready(Err::<(), _>(io::Error::from(
                    io::ErrorKind::PermissionDenied,
                )))
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn low_descriptor_limit_recovers_in_isolated_child() {
        if std::env::var_os("LOCHO_TEST_LOW_NOFILE").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "listener::tests::low_descriptor_limit_recovers_in_isolated_child",
                    "--test-threads=1",
                ])
                .env("LOCHO_TEST_LOW_NOFILE", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        };
        // SAFETY: only the disposable child changes its own descriptor limit.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let mut files = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => files.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                    break;
                }
            }
        }
        runtime.block_on(async move {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                drop(files);
            });
            let file = tokio::time::timeout(
                Duration::from_secs(2),
                accept_with_retry(
                    || std::future::ready(std::fs::File::open("/dev/null")),
                    &CancellationToken::new(),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(file.is_some());
        });
    }

    #[cfg(unix)]
    fn isolated(test: &str) -> bool {
        if std::env::var("LOCHO_TEST_ACCEPT_NOFILE").as_deref() == Ok(test) {
            return true;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads=1"])
            .env("LOCHO_TEST_ACCEPT_NOFILE", test)
            .status()
            .unwrap();
        assert!(status.success());
        false
    }

    #[cfg(unix)]
    fn exhaust_descriptors() -> Vec<std::fs::File> {
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        };
        // SAFETY: called only in an isolated disposable child after runtime setup.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        let mut files = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => files.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                    return files;
                }
            }
        }
    }

    #[cfg(unix)]
    async fn recover_accept<F, Fut, T, C, Peer>(mut accept: F, mut connect: C)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = io::Result<T>>,
        C: FnMut() -> Peer,
    {
        let files = exhaust_descriptors();
        match tokio::time::timeout(Duration::from_secs(1), accept())
            .await
            .unwrap()
        {
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EMFILE)),
            Ok(_) => panic!("accept unexpectedly succeeded with exhausted descriptors"),
        }
        let shutdown = CancellationToken::new();
        let recovery = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            drop(files);
            // BSD can consume queued connections on failed accepts. A real
            // reconnect after recovery must succeed on the same bound listener.
            connect()
        };
        let (result, _reconnected_peer) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(3),
                accept_with_retry(&mut accept, &shutdown)
            ),
            recovery,
        );
        let result = result.unwrap().unwrap();
        assert!(result.is_some());
        // Exhaustion also must not delay shutdown of a real bound listener.
        let _pending_peer = connect();
        let files = exhaust_descriptors();
        match tokio::time::timeout(Duration::from_secs(1), accept())
            .await
            .unwrap()
        {
            Err(error) => assert_eq!(error.raw_os_error(), Some(libc::EMFILE)),
            Ok(_) => panic!("accept unexpectedly succeeded with exhausted descriptors"),
        }
        let cancel = shutdown.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        });
        let result =
            tokio::time::timeout(Duration::from_secs(1), accept_with_retry(accept, &shutdown))
                .await
                .unwrap()
                .unwrap();
        assert!(result.is_none());
        canceller.await.unwrap();
        drop(files);
    }

    #[cfg(unix)]
    #[test]
    fn tcp_accept_recovers_after_descriptor_exhaustion() {
        if !isolated("listener::tests::tcp_accept_recovers_after_descriptor_exhaustion") {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let _waiting_peer =
                std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let address = listener.local_addr().unwrap();
            recover_accept(
                || listener.accept(),
                || std::net::TcpStream::connect(address).unwrap(),
            )
            .await;
            // A second accept also works, proving the original listener remains bound.
            let _peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .unwrap()
                .unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn unix_accept_recovers_after_descriptor_exhaustion() {
        if !isolated("listener::tests::unix_accept_recovers_after_descriptor_exhaustion") {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let path = std::env::temp_dir().join(format!("la{}", std::process::id()));
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let _peer = std::os::unix::net::UnixStream::connect(&path).unwrap();
            let _waiting_peer = std::os::unix::net::UnixStream::connect(&path).unwrap();
            recover_accept(
                || listener.accept(),
                || std::os::unix::net::UnixStream::connect(&path).unwrap(),
            )
            .await;
            let _peer = std::os::unix::net::UnixStream::connect(&path).unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .unwrap()
                .unwrap();
            drop(listener);
            std::fs::remove_file(path).unwrap();
        });
    }
}

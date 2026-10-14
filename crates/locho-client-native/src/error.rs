use locho_core::{capability::CapabilityError, session::SessionError};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    Closed,
    Cancelled,
    RuntimeRequired,
    InvalidHostId,
    InvalidDirectAddress,
    InvalidCapability(CapabilityError),
    UnsupportedService,
    InvalidTimeout,
    ListenerBind,
    ListenerFailed,
    EndpointBind,
    StartupTimeout,
    ShutdownTimeout,
    TaskFailed,
    Peer(SessionError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapability(error) => write!(f, "{error}"),
            Self::Peer(error) => write!(f, "{error}"),
            other => f.write_str(match other {
                Self::Closed => "native client or attachment is closed",
                Self::Cancelled => "attachment startup cancelled",
                Self::RuntimeRequired => "a persistent Tokio runtime with I/O and time is required",
                Self::InvalidHostId => "invalid host ID",
                Self::InvalidDirectAddress => {
                    "direct address requires a specific IP and nonzero port"
                }
                Self::UnsupportedService => "native attachment requires a TCP capability",
                Self::InvalidTimeout => "startup timeout must be nonzero and at most 300 seconds",
                Self::ListenerBind => "could not bind local TCP listener",
                Self::ListenerFailed => "local TCP listener failed",
                Self::EndpointBind => "could not bind peer endpoint",
                Self::StartupTimeout => "attachment startup timed out",
                Self::ShutdownTimeout => "attachment cleanup timed out",
                Self::TaskFailed => "native attachment task stopped unexpectedly",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for ClientError {}

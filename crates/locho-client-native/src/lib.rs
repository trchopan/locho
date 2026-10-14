//! Native TCP attachments on a caller-owned, persistent Tokio runtime.
//! No process signals, configuration files or global logging initialization.

mod client;
mod error;
pub mod forward;

pub use client::{Attachment, AttachmentStatus, Client, TcpOptions};
pub use error::ClientError;
pub use locho_core::session::{SessionState, TransportPath};

//! Shared wire-protocol primitives for Locho clients and hosts.
//!
//! This crate does not install a runtime, initialize logging, read configuration
//! files/environment variables, or own process signals. Async protocol helpers
//! use Tokio-compatible I/O and require the caller's runtime/clock adapter.

pub mod auth;
pub mod capability;
pub mod protocol;
#[cfg(feature = "transport")]
pub mod relay;
pub mod service;
#[cfg(feature = "transport")]
pub mod session;

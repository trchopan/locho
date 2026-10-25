//! Disposable N0 host with buffered tracing, not a production/release CLI.
#![allow(dead_code, unused_imports)]
#[path = "../../../../src/auth.rs"]
mod auth;
#[path = "../buffered_trace.rs"]
mod buffered_trace;
#[path = "../../../../src/config.rs"]
mod config;
#[path = "../../../../src/host.rs"]
mod host;
#[path = "../../../../src/http_utils.rs"]
mod http_utils;
#[path = "../../../../src/relay.rs"]
mod relay;
#[path = "../../../../src/state.rs"]
mod state;
#[path = "../../../../src/task.rs"]
mod task;
use locho_core::protocol;

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
    let mut args = std::env::args().skip(1);
    let config = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected disposable host config"))?;
    let dump = std::path::PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("expected private trace dump path"))?,
    );
    anyhow::ensure!(args.next().is_none(), "unexpected argument");
    // Create the output ourselves with restrictive permissions; never follow or
    // overwrite an existing user path. The launcher owns the parent directory.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dump)?;
    let buffer = buffered_trace::Buffer::default();
    host::investigation::enable();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(buffer.clone())
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()?)
        .init();
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    let writer = tokio::spawn(async move {
        let mut sequence = 0;
        while signal.recv().await.is_some() {
            sequence += 1;
            let snapshot = buffer.drain(sequence);
            let data = serde_json::to_vec(&serde_json::json!({
                "sequence": snapshot.sequence, "dropped_bytes": snapshot.dropped_bytes,
                "data": snapshot.data, "connections": host::investigation::snapshot(),
            }))?;
            // Requested only after a test case has settled. Partial writes are
            // tolerated by the launcher until the sequence + JSON are complete.
            use std::io::{Seek, SeekFrom};
            output.set_len(0)?;
            output.seek(SeekFrom::Start(0))?;
            output.write_all(&data)?;
            output.flush()?;
        }
        Ok::<(), anyhow::Error>(())
    });
    // Both endpoint construction and host implementation are the production
    // modules. Only the logging sink and this disposable binary's CLI differ.
    let result = host::run(config.into(), None, None).await;
    writer.abort();
    result
}

#[cfg(not(unix))]
fn main() {
    panic!("buffered trace fixture currently requires Unix signals");
}

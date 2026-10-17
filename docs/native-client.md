# Native TCP Attachment API

`locho-client-native` is an unpublished Rust 0.1.0 workspace crate for foreground
native applications. It uses `locho-core` peer supervision and the existing
`locho/3` TCP protocol. The published [Flutter beta](sdk-releases.md) wraps it with
FRB and source-build tooling. Physical iOS and broad device/network acceptance
remain open; native Rust tests alone do not establish mobile runtime support.

## Usage

The caller supplies a persistent Tokio runtime with I/O and time enabled. The
library creates no runtime, reads no files/environment variables, installs no
signals, and does not initialize global tracing.

```rust,no_run
use locho_client_native::{Client, ClientError, TcpOptions};
use std::time::Duration;

async fn foreground(host_id: String, capability: String) -> Result<(), ClientError> {
    let client = Client::new();
    let attachment = client.attach_tcp(
        TcpOptions::new(host_id, capability),
        Duration::from_secs(15),
    ).await?;
    let address = attachment.local_addr();
    // Use an ordinary HTTP client with http://{address}/health.
    // Reuse attachment for the foreground session, not one tunnel per request.
    let _ = address;
    attachment.close().await?;
    client.close().await
}
```

Options contain host ID, TCP capability, local port (default zero), optional direct
address and optional validated in-memory `RelaySettings`. The public SDK always
binds `127.0.0.1`; an OS-assigned port is returned. Non-TCP capabilities, malformed
host IDs and invalid direct addresses fail with sanitized typed errors.
`TcpOptions::from_ticket(&ticket)` accepts the shared [ticket v1](tickets.md)
metadata and peer relay hints; client relay policy remains separate.

`start_tcp` bounds validation/listener/endpoint setup to 15 seconds and returns
before peer connection. `attach_tcp` uses a caller-supplied nonzero deadline, at
most 300 seconds, covering setup and initial peer connection. Failure returns
after bounded resource cleanup. A standalone `wait_connected` timeout does not
close its attachment. Peer connectivity does not validate the capability or API:
applications perform an explicit `/health` request separately.

## Ownership and Lifecycle

- Client clones share one owner. A client registry tracks startup before any await,
  so shutdown cannot miss an in-progress attachment. Closing the client cancels all
  startups/attachments and prevents new starts. Independent attachments use distinct
  peer endpoints; closing one does not cancel another.
- Attachment clones share ownership. Last attachment-handle drop requests closure
  even while the client exists. Last client-handle drop requests closure of all its
  attachments even while attachment handles exist. Subscriptions do not own tunnels.
- Dropping/cancelling a startup future cancels through a startup guard; an owned
  supervisor completes resource cleanup. Background tasks hold controls and weak
  client references, not ownership handles that would keep themselves alive.
- Async close is idempotent and concurrent calls observe the same final result.
  Attachment cleanup runs under a 12-second budget and close waits at most 13
  seconds; client close has the same total wait bound, regardless of attachment count.
  The executor must continue polling for cleanup. Runtime destruction is best effort.
- Shutdown stops accepting, closes the peer, drains local tasks and aborts/joins
  remaining work when needed. The listener is released before final closed status.
  Returned local addresses are diagnostic/history after closure, not live endpoints.
- `snapshot`/`subscribe` include local address, state, path, revision, peer snapshot
  and terminal cause. Read the initial watch value before waiting for changes;
  delivery coalesces and does not require unbounded queues. Listener/task failures
  close the attachment; the final snapshot retains their cause.

## Requests and Recovery

One attachment selects one TCP service. Listener forwarding permits 128 concurrent
local connections, under a per-attachment native budget of 512; excess connections close
immediately. Shared peer stream/host limits also apply. Acquisition waits up to
ten seconds during recovery, service handshakes up to ten seconds, and forwarded
TCP traffic uses the existing 300-second idle timeout. Application request
deadlines are separate and caller-controlled.

Local ports stay bound across peer recovery. Active streams fail on transport loss;
new requests can work after reconnection. No application request is replayed.
Interrupted writes have unknown server outcome. A new foreground session after
pause/resume may have a different local port, so rebuild HTTP base URLs.

Service rejection is observed by normal HTTP clients as socket closure/invalid
response, not a guaranteed typed authorization exception. It does not mark a
healthy peer disconnected. Backend HTTP status/body remains ordinary application
data. Loopback Host headers, redirects/cookies and upstream authority configuration
remain application concerns; do not disable HTTPS certificate validation.

The CLI uses `forward::serve_tcp` with its own listener addresses and existing
multi-service peer session and shared process connection budget. That low-level adapter is separate from the SDK's
loopback-only API; CLI HTTP framing/output/signals remain native CLI adapters.

## Verification

```sh
cargo test -p locho-client-native -- --test-threads=1
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features -- --test-threads=1
```

Native tests use a controlled version-3 direct-address host and exercise HTTP
GET/POST, concurrency, port release, scoped ownership, startup cancellation/timeouts,
client-close races, service rejection and restart recovery. CLI optimized integration
and release-smoke gates remain required. FRB runtime ownership, Android JNI/DNS
initialization, standalone package consumption and Android/iOS build checks are
implemented. See [SDK evidence](sdk-releases.md) for the distinction between
compilation, emulator runtime, earlier physical Android feasibility and remaining
physical-device acceptance.

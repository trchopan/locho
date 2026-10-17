# SDK Foundation

The workspace contains the `locho` CLI, unpublished `locho-core` 0.1.0 shared
library and `locho-client-native` 0.1.0 native attachment adapter. See
[Native TCP attachment API](native-client.md) for caller-owned runtime, loopback
startup/status/shutdown and HTTP integration. The published browser and Flutter
packages are independently versioned at `0.1.0-beta.1`; see
[SDK releases](sdk-releases.md) for installation, provenance and registry integrity.

The [Flutter SDK](../packages/locho_flutter/README.md) wraps the native adapter with
pinned FRB, a persistent runtime and bundled core/native source-build integration.
Android/iOS compilation and fresh external consumers are checked in CI. Android
emulator notes/ticket/runtime evidence and earlier physical Android feasibility are
recorded separately from the remaining physical iOS and broad network matrix.

The browser adapter is `crates/locho-web` plus `packages/locho-web`. Its public
TypeScript API reuses shared peer supervision and service authorization through
WASM. Production external-consumer tests cover Chromium/Firefox/WebKit through
public N0 relay/discovery, including explicit, absent, dotted and unreachable host
relay hints. Controlled build runners are not a controlled-relay environment.
Local macOS builds require a WebAssembly-capable LLVM rather than Apple Clang.

Operation-scoped cancellation, typed errors, bounded watch subscriptions and shared
close completion are implemented. Tests check upstream-observed stream release,
independent concurrent traffic, interruption and repeated cycles. Applications own
page/foreground lifecycle and backend readiness checks; peer connectivity alone
does not establish service/API readiness.

## Implemented Boundary

SDK [status observation](status-observation.md) now provides bounded cancellation
for Flutter subscriptions and cancellable Rust-watch-backed browser observers.
Both standalone notes apps consume the registry betas in
[locho_sdk_examples](https://github.com/trchopan/locho_sdk_examples).

The browser exposes a [buffered and streaming HTTP subset](browser-http.md), backed
by bounded Rust framing and binary WASM responses. External production integration
includes reference API login/notes/logout and incremental authenticated SSE. The
Flutter app uses ordinary Dart buffered/streamed HTTP through its loopback tunnel.

`crates/locho-core` owns:

- Existing `locho/3` protocol constants, request/response serialization, bounded
  framing/body helpers, and forwarded-TCP idle behavior.
- Existing authorization proof derivation and constant-time verification.
- Service types/name validation and typed, sanitized capability parsing.
- Optional `transport` feature: validated in-memory relay settings, plus owned
  peer sessions, watch-style status, generation-scoped leases and authorized TCP
  streams. The CLI drives the shared supervisor on its existing runtime.

The CLI/host use these same implementations. Native secret generation remains in
the CLI, avoiding an OS RNG requirement in the protocol-only library. Filesystem,
TOML/environment adaptation, process signals, logging initialization, endpoint
creation and local listener ownership live in native adapters. The CLI and native
client share TCP listener forwarding; CLI HTTP framing and process adaptation stay
in CLI modules.
Debug representations redact capability secrets, protocol proofs and application
headers; serialization retains the existing wire format.

Example in-memory native relay policy:

```rust,no_run
use locho_core::relay::{RelayError, RelayOptions, RelaySettings};

fn relay_policy(token_from_application: String) -> Result<RelaySettings, RelayError> {
    RelaySettings::new(false, vec![RelayOptions {
        url: "https://relay.example.org".into(),
        auth_token: Some(token_from_application),
        quic_address_discovery: true,
    }])
}
// The caller applies the returned settings.mode() to its iroh endpoint builder.
```

Relay policy configures the client's endpoint. It is not a host peer-address hint.
Passing a token to native iroh does not establish browser relay-auth support.

## Peer Session Ownership

`PeerSession::start(endpoint, target)` transfers exclusive logical endpoint ownership
and returns a handle plus `SessionDriver`. The caller must run `driver.run()` on a
persistent executor; the core never calls `spawn` or creates a runtime. Native
callers use their Tokio runtime. The WASM adapter supplies browser-compatible task
execution and clocks; the Flutter bridge owns its persistent native runtime.

```rust,no_run
use iroh::{Endpoint, EndpointAddr};
use locho_core::session::{PeerSession, SessionError};
use std::time::Duration;

async fn foreground_session(endpoint: Endpoint, target: EndpointAddr)
    -> Result<(), SessionError>
{
    let (session, driver) = PeerSession::start(endpoint, target);
    let task = tokio::spawn(driver.run());
    let connected = session.wait_connected(Duration::from_secs(15)).await;
    let closed = session.close().await;
    let _ = task.await;
    connected?;
    closed
}
```

- Handle clones share ownership. Dropping the last handle requests cancellation;
  subscriptions and connection leases do not keep the owner alive.
- Endpoint bind/initialization deadlines remain caller-owned. A timed-out
  `wait_connected` leaves the session running. No automatic upstream probe occurs.
- `connected` authenticates the host identity only. Current status and `subscribe()`
  expose revisions and sanitized connection/terminal errors. Read the initial watch
  value before waiting for changes; updates can coalesce, including close transitions.
- `close()` is idempotent and cancels active peer traffic. The running driver bounds
  endpoint cleanup to ten seconds; close waits at most eleven seconds. A retained
  but unpolled driver may yield a shutdown timeout; a dropped/aborted driver publishes
  `DriverStopped` in the final snapshot. Orderly endpoint cleanup requires polling
  the driver through completion. Status survives for late subscribers.
- One session can serve multiple capabilities; independent sessions require distinct
  owned endpoints. An old lease cannot invalidate a newer connection generation.
- `open_tcp` rejects non-TCP capabilities and bounds acquisition plus stream opening,
  request writing and handshake reading by the caller deadline. It returns an
  authorized `AsyncRead + AsyncWrite` stream, not an HTTP response. Locho rejection
  statuses are typed errors separate from application HTTP responses.
- Successful v3 handshakes require version 3, status 200, empty headers and body
  length zero. Invalid responses fail without forwarding handshake bytes. Service
  rejection/cancellation does not invalidate a healthy peer.
- At most 512 shared TCP streams can be opening/owned per session. Excess operations
  fail immediately rather than queue; stream drop resets both halves and releases
  capacity. The host's existing per-service limits still apply. Cancellation passed
  to `open_tcp` applies to startup only; after return, the application owns the stream
  and drops it to cancel. Existing native listener limits are preserved separately.
- Transport loss fails in-flight streams; no requests/messages are replayed. CLI HTTP
  framing/error mapping remains in its adapter and uses generation-scoped leases.

## Build and Verification

```sh
cargo test -p locho-core --no-default-features
cargo test -p locho-core --features transport
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features -- --test-threads=1
```

Default core features do not include iroh or native host dependencies. Async helpers
still use Tokio I/O/time; callers own runtime/clock integration. Cross-target
compilation and browser runtime execution are separate evidence. Root CLI builds
require neither Flutter nor WASM tools. Native regression and release gates are
documented in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Remaining Integration Gates

1. Physical iOS and full physical Android/mobile-browser notes and ticket journeys,
   device storage behavior, lifecycle and real Wi-Fi/cellular transitions.
2. Actual BFCache and full browser-process restart evidence beyond synthetic page
   events and new page owners; verified transport-path/live-relay migration evidence.
3. Controlled-relay coverage, authenticated browser relay support and diagnosis of
   historical public-N0 initial-health/restore timeouts. Later green runs do not
   establish the cause of those timeouts.
4. Candidate-specific host/package/examples compatibility and published CLI artifact
   verification for each release. See [1.3.0-beta.1 evidence](release-1.3.0-beta.1.md).

Streaming/SSE has desktop/emulator evidence. Browser managed WebSockets, QR and
background mobile execution are later scope. Package installation and source-build
requirements are documented in [SDK releases](sdk-releases.md) and
[Flutter distribution](flutter-package.md).

The core API is an internal prerelease boundary and may evolve during these gates.
No wire version change or new host configuration is introduced by this extraction.

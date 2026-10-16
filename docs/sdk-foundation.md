# SDK Foundation

The workspace contains the `locho` CLI, unpublished `locho-core` 0.1.0 shared
library and `locho-client-native` 0.1.0 native attachment adapter. See
[Native TCP attachment API](native-client.md) for caller-owned runtime, loopback
startup/status/shutdown and HTTP integration. Flutter/browser packages remain
subsequent deliverables.

The Android-first [Flutter feasibility package](../packages/locho_flutter/README.md)
now wraps the native adapter with pinned FRB/source-build integration. Physical
Android debug/release attach/GET/close evidence is recorded there. iOS build CI and
remaining platform/network/distribution gates are separate from that evidence.

The browser feasibility scaffold is `crates/locho-web` plus `packages/locho-web`.
It reuses the shared peer session and authorization path and currently exposes only
a raw `/health` proof. Controlled Linux CI builds WASM and exercises the official
N0 relay. Local macOS builds also work with a WebAssembly-capable LLVM compiler
instead of Apple Clang. The proof is not a final browser API or ticket contract.

The follow-up proof adds bounded response framing, authority validation, startup and
request cancellation hooks, late-connect cleanup, and production Playwright asset
loading tests for Chromium/Firefox/WebKit. Controlled CI now starts a real Locho
host and TCP health backend and exercises the browser through the official N0 relay
using a peer relay hint derived from N0 diagnostics. The follow-up addressing matrix
adds host-ID-only discovery, dotted hints and unreachable-hint recovery in fresh
browser contexts. The browser adapter normalizes DNS root dots in hints and verified
Pkarr results while retaining N0 publishing. Local macOS production WASM passed all
18 cases on 2026-10-15; PR #89 also passed hosted Linux verification. Custom
authenticated relays and live relay migration remain separate evidence.

Browser lifecycle hardening propagates operation-scoped cancellation into Rust,
preserves typed errors, and shares close completion across repeated callers.
Health abort drops its stream without closing independent traffic. The local
production WASM matrix now passes 33 cases, including upstream-observed stream
release, active close, interruption/unavailability and repeated cycles. Hosted
Linux lifecycle evidence remains pending; see the browser package README for
startup and page-destruction cleanup bounds.

## Implemented Boundary

The browser now also exposes a [buffered HTTP subset](browser-http.md), backed by
bounded Rust framing and binary WASM responses. Local production integration passes
48 browser cases including the pinned reference API health/login/notes/logout
journey. Hosted Linux verification and external package delivery are separate gates.

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
callers use their Tokio runtime. A browser executor and clock still need feasibility
verification; avoiding native spawning does not by itself establish WASM support.

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

1. Browser `wasm32-unknown-unknown` crypto/runtime/discovery/relay proof against a
   real host is covered through the official N0 relay; record custom relay
   authentication and stale peer-hint behavior separately.
2. FRB runtime/native packaging and physical iOS/Android attach/GET/close proof on
   the implemented native loopback attachment adapter.
3. The [single-service ticket v1 contract](tickets.md) now has a shared core codec,
   interoperability vectors and explicit `locho share --ticket` output. SDK ticket
   consumption now uses the shared Rust parser through WASM and FRB, with redacted
   objects, metadata/export and peer hints. Local production browser and real FRB
   vector tests pass. Android 17/API 37 ARM64 emulator ticket debug/release runtime
   checks pass on 16 KiB pages; physical-device ticket acceptance and standalone
   packages remain open.
4. Deliver distributable Dart/npm packages and exercise external consuming apps
   against one versioned health/login/notes backend.

Browser package delivery now builds an allowlisted ESM/types/WASM tarball with
source/version metadata. A temporary external Vite consumer installs only that
tarball, builds below `/sdk/`, and runs the N0/notes integration matrix. Public
declarations contain no raw WASM handles. Hosted package evidence is pending;
Flutter now has a [standalone source-build archive](flutter-package.md), with exact
public Git native/core dependencies instead of sibling Rust paths. Fresh external
Android debug/release builds and 16 KiB emulator ticket runtime pass locally;
external iOS builds are delegated to hosted CI.

The core API is an internal prerelease boundary and may evolve during these gates.
No wire version change or new host configuration is introduced by this extraction.

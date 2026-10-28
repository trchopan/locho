# Changelog

All notable changes to `locho` are documented here.

## Unreleased

## [1.3.0] - 2026-10-28

This release promotes the `1.3.0` CLI feature and reliability series to the
stable release channel, adding native macOS/Linux Unix stream socket forwarding,
portable single-service tickets (`locho1:`), shared core/native client libraries,
and shared-peer stream hardening.

- Adds Unix filesystem stream upstreams (`unix_socket`) for TCP services and
  secure Unix socket listeners (`--listen-unix` and `listen_unix`) for TCP
  attachments on macOS and Linux, including mixed TCP/Unix multi-service
  attachments and configurable raw-stream application-byte idle deadlines
  (`tcp_idle_timeout_secs`, defaulting to 300 seconds; `0` disables idle expiry).
- Anchors Unix listener creation (`0600`) to verified private (`0700`) directory
  handles, rejects unsafe ancestry/symlinks/macOS ACLs, checks device/inode
  identity before cleanup, and installs attachment shutdown handlers before
  startup. Root/same-UID filesystem races remain an explicit trust boundary.
- Adds explicit `locho share SERVICE --config PATH --ticket` output with optional
  `--peer-relay-url` hints. Ticket v1 (`locho1:`) packages an existing service
  capability as a bearer credential; it is not an encrypted, expiring, or
  independently revocable invitation.
- Extracts shared protocol, capability proof, ticket v1, and `PeerSession`
  supervision into `locho-core` and owned loopback TCP attachments into
  `locho-client-native` while preserving CLI HTTP/TCP and multi-service behavior.
- Isolates HTTP upstream failures and truncated responses from other streams on
  the same peer session; hardens stream cancellation, half-close, and cleanup so
  one failed request does not disconnect unrelated services.
- Aligns the host QUIC stream window with its 128-connection TCP budget, avoiding
  handshake stalls above 100 concurrent streams on a shared peer.
- Adds native Unix/TCP release smoke, stress, and soak harnesses across all four
  TCP/Unix forwarding combinations, plus explicit multi-baseline upgrade
  rehearsal verification.
- Updates `iroh` to `1.4.1`, `tokio` to `1.54.1`, `hyper` to `1.12.0`, `base64`
  to `0.23.1`, `ctutils` to `0.4.4` (`RUSTSEC-2026-0385`), and TLS certificate
  verification/root-trust dependencies.

No host-state or capability migration is required; wire protocol `locho/3`,
ticket format `1`, and host-state schema `2` are unchanged. Upgrading replaces
only the CLI executable and retains host identity and service capabilities. Unix
stream sockets are supported on macOS and Linux, not Windows. CLI HTTP WebSocket
upgrades remain unsupported.

The independently versioned `locho_web` (npm) and `locho_flutter` (pub.dev) SDK
packages remain in beta at `0.1.0-beta.3`; a CLI upgrade does not update
installed SDK packages. Physical iOS runtime, broad physical-device/mobile-browser
and network-transition acceptance, actual BFCache, and authenticated browser
relays remain open SDK beta gates, and browser discovery with stale relay hints
can still intermittently time out over public N0 relays. See
[SDK and prerelease provenance](docs/sdk-releases.md).

Linux ARM64 artifacts are cross-compiled; native verification is pending.

## [1.3.0-beta.3] - 2026-10-25

This limited integration prerelease refreshes the CLI security dependencies;
`1.2.0` remains the stable release.

- Updates ctutils from 0.4.2 to 0.4.4 to address RUSTSEC-2026-0385.
- Keeps native CLI/runtime behavior, iroh 1.4.0, `locho/3`, ticket format 1 and
  host-state schema 2 unchanged from CLI beta.2. No state/capability migration is
  required; upgrading replaces only the executable.

The coordinated but independently published `locho_web` and `locho_flutter`
`0.1.0-beta.3` packages carry their own changes and provenance. Browser beta.3
adds opt-in upstream-header metadata and aligns client/peer relay URL keys;
Flutter beta.3 refreshes its bundled core/native implementation. These changes
are not embedded in the CLI executable, and a CLI upgrade does not update SDKs.

Public-N0 stale-hint intermittency remains a documented limitation: the relay-key
configuration mismatch has a deterministic regression, but the stricter network
A/B control was inconclusive and does not prove every timeout is resolved. HTTP
requests are not replayed and deadlines are unchanged. Physical iOS, broad
device/network-transition acceptance and Linux ARM64 native verification remain
open; cross-builds and hosted compilation are not device/runtime proof.

See [SDK and prerelease provenance](docs/sdk-releases.md) and the
[release checklist](docs/releasing.md). Prerelease publishing must leave
the stable CLI installers and npm `latest` channel unchanged.

## [1.3.0-beta.2] - 2026-10-21

This prerelease adds native Unix stream forwarding and hardens shared peer
sessions. It is intended for beta testing; `1.2.0` remains the stable release.

- Updates iroh to 1.4.0 across CLI and SDK source builds, including relay receive
  correctness fixes and the updated QUIC transport dependency.
- Updates TLS certificate verification and root-trust dependencies to address
  security advisories.
- Isolates HTTP upstream failures and truncated responses from other streams on
  the same peer session; hardens stream cancellation, half-close and cleanup so
  one failed request does not disconnect unrelated services.
- Aligns the host QUIC stream window with its 128-connection TCP budget, avoiding
  handshake stalls above 100 concurrent streams on a shared peer.
- Retries transient browser discovery failures within the existing startup budget,
  without replaying application HTTP requests or extending timeouts.
- Anchors Unix listener operations to verified directory handles, rejects unsafe
  ancestry, installs attachment shutdown handlers before startup, and isolates
  local client resets from shared-peer recovery. Root/same-UID filesystem races
  remain an explicit trust boundary.
- Keeps idle expiry and reverse-direction forwarding active under backpressure;
  adds same-PID resource-drain and harness-finalization regression checks.
- Adds a native Unix/TCP stress profile with ramped concurrency across all four
  forwarding combinations, latency/resource artifacts, and strict corruption,
  replay, failure and descriptor-growth gates.

- Adds Unix filesystem stream upstreams for TCP services and secure Unix socket
  listeners for TCP attachments on macOS/Linux, including mixed TCP/Unix
  multi-service attachments and configurable raw-stream idle deadlines.
- Keeps the `locho/3` wire protocol, ticket format, HTTP API, and public SDK
  defaults unchanged. Unix paths are local policy and are not carried in tickets.
- Adds native release smoke and soak harnesses for all four TCP/Unix forwarding
  combinations, binary/full-duplex integrity, idle policy, connection budgets,
  recovery, listener security and cleanup, using generic fixtures rather than Herdr.

No host-state or capability migration is required. Unix paths remain local policy;
Unix listeners and upstreams are supported on macOS/Linux, not Windows. Existing
TCP forwarding remains supported on all distributed platforms. Raw TCP/Unix streams
default to a 300-second application-byte idle timeout; set `tcp_idle_timeout_secs`
to `0` to disable it.

Browser stale-address discovery can still intermittently time out over public N0
relays. This was reproduced with iroh 1.2.0, 1.3.0 and 1.4.0; the dependency upgrade
is not a demonstrated fix. Physical iOS, broad physical-device/mobile-browser and
real network-transition acceptance remain open. CLI HTTP WebSocket upgrades remain
unsupported. Linux ARM64 native verification remains pending; cross-builds do not
establish native runtime acceptance.

The independently published `locho_web` and `locho_flutter` packages remain at
`0.1.0-beta.2`; this CLI release does not republish them or update their bundled
implementation. See [SDK and prerelease provenance](docs/sdk-releases.md) for
target-specific limitations and verification records.

## [1.3.0-beta.1] - 2026-10-17

This prerelease introduces portable service tickets and SDK integration for browser
and foreground Flutter applications. It is intended for beta testing; `1.2.0`
remains the stable release channel.

- Adds explicit `locho share SERVICE --ticket` output with optional host peer-relay
  hints. Ticket v1 packages an existing service capability; it is a bearer credential,
  not an encrypted, expiring or independently revocable invitation.
- Extracts shared protocol, authorization, peer-session supervision and native TCP
  attachment libraries while preserving CLI HTTP/TCP and multi-service behavior.
- Supports the independently versioned public `locho_web` and `locho_flutter`
  `0.1.0-beta.1` packages using the existing `locho/3` protocol.
- Provides browser TCP-service HTTP with bounded buffered/streaming responses,
  operation cancellation, status observation and explicit cleanup. Browser traffic
  uses iroh's WebSocket-compatible relay transport; no client-side Locho process is
  required. This is a documented HTTP subset, not the browser's native Fetch stack.
- Provides foreground Flutter TCP attachments through a phone-local loopback
  listener, ordinary Dart HTTP clients and bundled Rust source-build integration.
- Adds packaged external-consumer checks and shared browser/Flutter notes examples
  with separate backend authentication, remembered sessions, recovery and SSE.

No host configuration or wire-protocol migration is required. Upgrading replaces
the executable and retains host identity and service capabilities. SDK packages are
installed separately from npm/pub.dev; see `docs/sdk-releases.md`.

Beta evidence covers desktop Chromium/Firefox/WebKit, Android emulator runtime,
Android/iOS builds and limited earlier physical Android feasibility. Physical iOS,
full physical-device/mobile-browser acceptance, real network transitions, actual
BFCache and controlled-relay acceptance remain open. Earlier browser health/restore
timeouts over public N0 remain unexplained despite subsequent green runs. Browser
managed WebSockets, authenticated browser relays, QR scanning and background mobile
execution are outside this beta's scope. The CLI HTTP service still does not support
WebSocket upgrades; native TCP byte forwarding remains available.

Linux ARM64 artifacts are cross-compiled; native verification is pending. Other
target verification boundaries and candidate evidence are in
`docs/sdk-releases.md`.

## [1.2.0] - 2026-09-29

This release adds configurable HTTP timeout controls, custom relay transport
support, and more useful request logging context.

- Adds per-service upstream HTTP timeouts and attachment-side HTTP response
  timeouts, defaulting to 60 seconds and configurable from 1 to 300 seconds.
- Adds shared custom iroh relay configuration for host, attach, and diagnose,
  including custom-only and custom-plus-N0 transport maps.
- Supports optional relay bearer tokens from environment variables and
  per-relay QUIC Address Discovery settings.
- Includes an explicitly provided relay configuration path in generated share
  and rotation attach commands.
- Adds service and short endpoint identifiers to request and connection logs.
- Enables ANSI log colors only for terminal output and honors non-empty
  `NO_COLOR` for plain logs.

Linux ARM64 artifacts are cross-compiled; native verification is pending.

## [1.2.0-beta.2] - 2026-09-28

This prerelease adds request logging context and configurable relay transport
support. It is intended for beta testing and is not the stable release channel.

- Adds service and short endpoint identifiers to host request and attachment
  connection logs, with HTTP request paths kept at the end of request log
  fields.
- Enables ANSI log colors only for terminal output and honors non-empty
  `NO_COLOR` for plain logs.
- Adds shared custom iroh relay transport configuration for host, attach, and
  diagnose.
- Supports custom-only and custom-plus-N0 relay transport maps, optional bearer
  tokens from environment variables, and per-relay QUIC Address Discovery
  settings.
- Includes the relay configuration path in generated share and rotation attach
  commands when explicitly provided.

## [1.2.0-beta.1] - 2026-08-31

This prerelease adds configurable HTTP timeout controls. It is intended for
beta testing and is not the stable release channel.

- Adds an optional per-HTTP-service `upstream_timeout_secs` setting, defaulting
  to 60 seconds and capped at 300 seconds.
- Adds optional attachment-side HTTP response timeouts through
  `http_timeout_secs` and `--http-timeout-secs`, defaulting to 60 seconds.

## [1.1.1] - 2026-08-06

- Fixes release binaries reporting a dirty checkout when generated cargo-dist
  manifests are present.
- Verifies release binary commit provenance and clean-checkout metadata across
  packaged artifacts.
- Removes internal distribution manifests from published GitHub Releases.

## [1.1.0] - 2026-08-06

- Adds typed, service-scoped capability tokens with explicit `share` and
  `secret` commands.
- Adds multi-service attachment configurations for HTTP and TCP listeners that
  share one reconnecting iroh connection.
- Improves reconnect behavior after host termination and for shared
  attachments.
- Hardens capability proof verification with constant-time comparison.
- Adds dependency security policy checks and automated dependency updates.

This release is intended for developer workflows, internal tools, controlled
service sharing, and debugging. It does not provide WebSocket upgrades, live
configuration reload, arbitrary host routing, per-user authorization, or
production availability guarantees. Configuration changes and capability
rotation require restarting the host.

## [1.1.0-beta.3] - 2026-07-29

- Stops `locho host` from printing service capabilities automatically.
- Adds explicit `locho share` and `locho secret` commands with service-typed
  capability tokens consumed directly by `locho attach`.
- Adds `locho attach --config` for multiple local HTTP and TCP listeners sharing
  one reconnecting iroh connection.
- Stabilizes reconnect handling when multiple services share an attachment
  process.

This prerelease continues the reliability and attachment workflow work from
`1.1.0-beta.2`. It is intended for beta testing and is not the stable release
channel.

## [1.1.0-beta.2] - 2026-07-28

This prerelease continues the reliability work from `1.1.0-beta.1`. It is
intended for beta testing and is not the stable release channel.

- Suppresses the non-actionable iroh QAD mapping warning at the default log
  level while preserving it through `RUST_LOG` debugging.
- Makes `locho diagnose` wait briefly for a relay connection to upgrade to a
  direct path before reporting the transport path.

- Upgrades iroh to 1.0.3 to improve relay-path reliability and avoid stalled
  TCP transfers under relay congestion.
- Preserves complete direct, relay, and mixed transport diagnostics.
- Includes the TCP mode when generating attachment commands for TCP services.
- Documents Rust 1.96 as the minimum supported version.

## [1.0.0] - 2026-07-21

- Adds CI verification for native cargo-dist archives, checksums, generated
  installers, and the documented release-binary workflow.
- Adds explicit direct-address hints for hosts and attachments when peer
  discovery cannot advertise a reachable address.
- Adds optional PEM CA configuration for private HTTPS upstreams while keeping
  normal system-root validation as the default.
- Adds cross-platform release-binary smoke coverage for HTTPS, TCP concurrency,
  upstream failure, restart, and capability rotation.

This is the first stable release of `locho`.

- Supports multiple explicitly configured HTTP and TCP services.
- Adds service-scoped bearer capabilities with persistence, rotation, and revocation.
- Adds streaming HTTP proxying and bidirectional TCP forwarding.
- Adds bounded resources, timeouts, graceful shutdown, and diagnostics.
- Adds cross-platform release builds and verified installation artifacts.

## [0.1.0]

Initial development release.

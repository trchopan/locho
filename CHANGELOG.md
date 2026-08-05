# Changelog

All notable changes to `locho` are documented here.

## [1.1.0] - 2026-08-05

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

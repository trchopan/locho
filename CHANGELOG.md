# Changelog

All notable changes to `locho` are documented here.

## [1.1.0-beta.1] - 2026-07-25

This prerelease focuses on tunnel reliability and operational validation after
the `1.0.0` stable release. It is intended for beta testing and is not the
stable release channel.

- Keeps attachment listeners available while the tunnel reconnects after a
  connection loss.
- Reuses HTTP upstream clients and reaps completed connection tasks to improve
  long-running host and attachment behavior.
- Adds Docker-based stress and bounded soak-test harnesses for mixed HTTP/TCP
  traffic, connection churn, and process restarts.

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

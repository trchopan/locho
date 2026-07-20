# Changelog

All notable changes to `locho` are documented here.

## [Unreleased]

- Adds CI verification for native cargo-dist archives, checksums, generated
  installers, and the documented release-binary workflow.
- Adds explicit direct-address hints for hosts and attachments when peer
  discovery cannot advertise a reachable address.
- Adds optional PEM CA configuration for private HTTPS upstreams while keeping
  normal system-root validation as the default.
- Adds cross-platform release-binary smoke coverage for HTTPS, TCP concurrency,
  upstream failure, restart, and capability rotation.

## [0.2.0] - Beta

This release establishes the first distributable beta of `locho`.

- Supports multiple explicitly configured HTTP and TCP services.
- Adds service-scoped bearer capabilities with persistence, rotation, and revocation.
- Adds streaming HTTP proxying and bidirectional TCP forwarding.
- Adds bounded resources, timeouts, graceful shutdown, and diagnostics.
- Adds cross-platform release builds and verified installation artifacts.

## [0.1.0]

Initial development release.

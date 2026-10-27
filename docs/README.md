# locho Documentation

Welcome to the `locho` documentation hub. Use this index to find user guides,
protocol and SDK specifications, architecture docs, and maintainer workflows.

## Getting Started & Product Overview

- **[Main README](../README.md)** — Installation, 5-minute quickstart, core
  service model, and CLI usage.
- **[Frequently Asked Questions (FAQ)](faq.md)** — Common questions about the
  service model, capabilities, HTTP/TCP forwarding, connectivity, and security.
- **[Comparison with Other Tools](comparison.md)** — How `locho` compares with
  VPNs/network overlays (Tailscale, ZeroTier), public tunnels (ngrok, Cloudflare
  Tunnel), SSH port forwarding, and TCP forwarders (`bore`, `zrok`).
- **[Product Roadmap & Boundaries](roadmap.md)** — Product thesis, completed
  milestones, explicit non-goals, and architectural decision gates.

## CLI & Operational Guides

- **[Unix Stream Sockets (`unix-sockets.md`)](unix-sockets.md)** — Forwarding
  TCP capabilities to/from filesystem Unix stream sockets on macOS and Linux,
  idle timeout policies, and listener security.
- **[Custom iroh Relays (`custom-relays.md`)](custom-relays.md)** — Configuring
  self-hosted `iroh-relay` servers (`relay.toml`), bearer token environment
  variables, QUIC Address Discovery (QAD), and manual relay verification.
- **[Portable Single-Service Tickets v1 (`tickets.md`)](tickets.md)** —
  Specification and CLI/SDK usage for `locho1:` portable bearer tickets and
  peer relay hints.
- **[Security Policy (`SECURITY.md`)](../SECURITY.md)** — Supported versions,
  private vulnerability reporting, and Unix socket filesystem security.

## Client SDKs & Specifications

- **[Browser SDK Guide (`packages/locho-web/README.md`)](../packages/locho-web/README.md)** —
  Application developer guide for installing and using `locho_web` (npm) in
  browser applications.
- **[Browser HTTP Subset Specification (`browser-http.md`)](browser-http.md)** —
  Detailed contract for `locho_web`'s buffered `fetch`, streaming `fetchStream`,
  opt-in upstream response header metadata, framing limits, and error codes.
- **[Flutter SDK Guide (`packages/locho_flutter/README.md`)](../packages/locho_flutter/README.md)** —
  Application developer guide for installing and using `locho_flutter` (pub.dev)
  in foreground Android and iOS apps.
- **[Flutter Source-Build Package (`flutter-package.md`)](flutter-package.md)** —
  Standalone source-build packaging, toolchain requirements, and external
  consumer verification for `locho_flutter`.
- **[Native Rust Client API (`native-client.md`)](native-client.md)** —
  Loopback TCP attachment API, ownership, and lifecycle contract for the
  `locho-client-native` workspace crate.
- **[SDK Status Observation (`status-observation.md`)](status-observation.md)** —
  Status subscription semantics, coalescing, and cancellation bounds across the
  Flutter and Browser SDKs.

## Architecture, Testing & Maintainer Guides

- **[Contributing & Developer Onboarding (`CONTRIBUTING.md`)](../CONTRIBUTING.md)** —
  Workspace architecture map, local development setup for Rust/Browser/Flutter,
  and PR quality gates.
- **[SDK Foundation & Shared Core (`sdk-foundation.md`)](sdk-foundation.md)** —
  Internal architecture of `crates/locho-core`, `PeerSession` supervision, and
  shared protocol extraction.
- **[Stress & Soak Test Harness (`tests/perf/README.md`)](../tests/perf/README.md)** —
  Running native Unix stream smoke/stress/soak tests and Docker HTTP/TCP load
  tests.
- **[Browser Soak Tests (`packages/locho-web/tests/SOAK.md`)](../packages/locho-web/tests/SOAK.md)** —
  Running sustained browser and service-worker metadata soak tests.
- **[Release Checklist (`releasing.md`)](releasing.md)** — Maintainer checklist
  for verifying, tagging, and publishing CLI and SDK releases.
- **[SDK & Prerelease Provenance (`sdk-releases.md`)](sdk-releases.md)** —
  Published SDK registry versions, SHA-256/SRI integrity hashes, source commit
  provenance, and target verification records.
- **[Changelog (`CHANGELOG.md`)](../CHANGELOG.md)** — Version-by-version release
  notes.

# locho Roadmap

## Product Thesis

locho is a developer-oriented private service tunnel. One host process exposes
multiple explicitly configured HTTP and TCP services. An authorized machine
attaches a selected service to a local HTTP endpoint or TCP port.

locho uses iroh for encrypted peer connectivity and NAT traversal. It does not
create a virtual network, provide subnet routing, publish public URLs, or
become a hosted service-management platform.

The core mental model is:

```text
Configure services -> Start host -> Share a service capability -> Attach locally
```

## 1.0 Status: Complete

The first stable release, `1.0.0`, delivers the core service-tunnel workflow:

- Multiple named HTTP and TCP services in one host process.
- Explicit service configuration and service lifecycle behavior.
- An independent attachment capability for every service.
- Local HTTP proxying for HTTP services.
- Local port forwarding for TCP services.
- Multiple concurrent attachments from different machines.
- Shared service capabilities rather than per-user authorization.
- Per-service capability rotation, which invalidates the previous capability
  after the host restarts with the replacement state.
- Persistent host identity and safe credential storage.
- Encrypted iroh connectivity with direct-path and relay fallback behavior.
- Bounded memory, connection, request, and response resources.
- Timeouts, graceful shutdown, diagnostics, and actionable errors.
- Cross-platform build, packaging, and operational documentation.

The stable product contract is:

- Configuration is loaded and validated before startup. Configuration changes
  require stopping and restarting the host; live configuration reload is not
  part of the current contract.
- `rotate-secret SERVICE` is the capability revocation mechanism. It replaces
  one service capability without changing other services. Existing streams are
  not retroactively closed; new attachments must use the replacement after the
  host restarts.
- HTTP WebSocket upgrades are unsupported and documented. TCP services are the
  supported option for non-HTTP protocols.
- Direct and relay transport paths are observable through diagnostics and
  attachment output. Relay availability and performance are external
  infrastructure dependencies.
- The release gate covers formatting, linting, unit tests, release builds,
  process-level HTTP and TCP tests, and packaged artifact smoke tests.

The release gate does not require deterministic end-to-end relay testing. Relay
verification is environment-dependent because it depends on iroh discovery and
relay infrastructure.

The project does not promise individual machine identity, per-user policy,
audit-grade access records, or a centralized locho account system.

## 1.3.0-beta.1: SDK Integration Beta

The beta adds portable single-service tickets, shared client libraries and public
browser/foreground Flutter SDKs, independently versioned at browser `0.1.0-beta.1`
and Flutter `0.1.0-beta.2`.
Both standalone notes examples consume the registry packages and share one API.
JSON, scoped opt-in persistence, explicit lifecycle/recovery and SSE have
desktop-browser/Android-emulator evidence; Android/iOS compilation is covered in CI.
The existing host config and `locho/3` wire protocol are preserved.

This is a limited beta, not completion of the full mobile acceptance matrix.
Physical iOS, full physical Android/mobile-browser journeys, real network/storage
transitions, actual BFCache and controlled-relay coverage remain follow-up gates.
Browser managed WebSockets, QR and background mobile execution are later work.
See [the release evidence record](docs/release-1.3.0-beta.1.md) and
[SDK installation/provenance](docs/sdk-releases.md).

## Non-Goals

The following are outside the product boundary:

- Virtual network interfaces, mesh networking, IP routing, or subnet exposure.
- Arbitrary host access or unrestricted port scanning through a host.
- Public ingress, public URLs, webhook delivery, or provider-managed routing.
- Per-user identity, per-machine authorization, team administration, or audit
  platform features.
- A service registry, hosted account system, or mandatory locho coordinator.
- Kubernetes operators, platform orchestration, or a service-management SaaS.

Iroh discovery and relay infrastructure may still be used for connectivity. No
locho account or application-managed control plane is required for the core
workflow.

## Future Possibilities

Future work may improve the developer utility without changing its service-level
identity. These are deliberately non-committal possibilities, not a schedule or
promise for a particular release:

- Better configuration and local service management.
- Optional human-readable service exchange.
- Additional protocol support where the security boundary remains explicit.
- Performance improvements such as stream multiplexing.
- Docker and CI examples.
- Additional platform targets where packaging and native verification are
  maintainable.

Live configuration reload is not currently planned; the restart-required model
is intentional and should remain the default unless a future product decision
changes it. Team identity, per-user policy, hosted discovery, and platform
integrations require separate product decisions.

## Decision Gates

- Do not add network routing or virtual interfaces.
- Do not add multi-peer policy management unless locho becomes a different
  product with an explicit identity and administration model.
- Do not add discovery before the explicit capability workflow is reliable.
- Do not add hosted coordination without revisiting the no-platform principle.
- Do not add a new service type unless its authorization, isolation, resource,
  and failure semantics can be documented and tested.
- Do not make relay testing a mandatory release gate unless locho controls a
  deterministic relay test environment and can maintain it.

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

## Core Contract

The project must provide:

- Multiple named HTTP and TCP services in one host process.
- Explicit service configuration and service lifecycle behavior.
- An independent attachment capability for every service.
- Local HTTP proxying for HTTP services.
- Local port forwarding for TCP services.
- Multiple concurrent attachments from different machines.
- Shared service capabilities rather than per-user authorization.
- Per-service capability rotation and revocation.
- Persistent host identity and safe credential storage.
- Encrypted iroh connectivity with direct-path and relay fallback behavior.
- Bounded memory, connection, request, and response resources.
- Timeouts, graceful shutdown, diagnostics, and actionable errors.
- Cross-platform build, packaging, and operational documentation.

The project does not promise individual machine identity, per-user policy,
audit-grade access records, or a centralized locho account system.

## Milestones

### 1. Service Configuration

Define a stable configuration format and CLI for multiple services.

Acceptance criteria:

- A host can load several HTTP and TCP services in one process.
- Every service has a stable name and explicit endpoint configuration.
- Invalid, duplicate, ambiguous, or unsafe configurations fail before startup.
- Service configuration does not imply access to other host services.

### 2. HTTP Tunnel

Provide reliable local HTTP access to a selected private HTTP service.

Acceptance criteria:

- Requests and responses stream without an unnecessary whole-body limit.
- Required HTTP methods, headers, keep-alive, and chunked bodies behave correctly.
- Upstream TLS validation is explicit and safe by default.
- WebSocket behavior is either supported and tested or clearly documented as
  unsupported.
- Request size, response size, timeout, and concurrency limits are documented.

### 3. TCP Tunnel

Provide bidirectional forwarding for explicitly selected TCP endpoints.

Acceptance criteria:

- A client can attach a named TCP service to a local listener.
- Multiple concurrent TCP connections are isolated and supported.
- EOF, cancellation, half-close, and remote failure behave predictably.
- The host cannot be used to reach arbitrary ports or addresses.
- Resource limits prevent one service or client from exhausting the host.

### 4. Capabilities and Identity

Make authorization service-scoped and operationally understandable.

Acceptance criteria:

- Each service has an independent attachment capability.
- A capability for one service cannot authorize another service.
- Multiple machines can use the same capability concurrently.
- Capability values are not logged, accidentally persisted in insecure files, or
  unnecessarily exposed in process output.
- Rotation and revocation take effect according to documented semantics.
- Host identity and service capabilities have separate lifecycle controls.

The service model intentionally does not include per-user or per-machine policy.

### 5. Reliability and Safety

Make the tunnel suitable for serious developer workflows.

Acceptance criteria:

- Direct connectivity failure and relay use are observable and documented.
- Connection, idle, upstream, and shutdown timeouts are defined.
- Graceful shutdown stops accepting work and closes active sessions predictably.
- Malformed protocol data, oversized messages, and invalid service requests are
  rejected safely.
- Concurrent request and connection behavior is covered by integration tests.

### 6. Operations and Distribution

Make the utility installable and diagnosable without a platform service.

Acceptance criteria:

- Structured logs and a human-readable diagnostic mode are available.
- Startup, attachment, authentication, upstream, and relay failures have useful
  messages.
- Installation and release instructions cover supported operating systems.
- The README quickstart can be followed using released binaries.
- End-to-end tests cover HTTP, TCP, concurrency, restart, and credential
  rotation behavior.

The current release gate is tracked by CI and includes formatting, Clippy, unit
tests, a release build, and process-level HTTP and TCP integration tests. The
integration tests use a test-only feature for deterministic local peer
connectivity; normal and release builds do not enable it.

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
identity:

- Better configuration and local service management.
- Optional human-readable service exchange.
- Additional protocol support where the security boundary remains explicit.
- Performance improvements such as stream multiplexing.
- LAN-first or self-hosted relay configuration.
- Docker and CI examples.

These are possibilities, not commitments. Team identity, per-user policy, hosted
discovery, and platform integrations require separate product decisions.

## Decision Gates

- Do not add network routing or virtual interfaces.
- Do not add multi-peer policy management unless locho becomes a different
  product with an explicit identity and administration model.
- Do not add discovery before the explicit capability workflow is reliable.
- Do not add hosted coordination without revisiting the no-platform principle.
- Do not add a new service type unless its authorization, isolation, resource,
  and failure semantics can be documented and tested.

# locho

`locho` is a developer-oriented private tunnel for explicitly selected HTTP and
TCP services. A host exposes one or more services, and an authorized machine
attaches each service to a local endpoint. Connections use
[iroh](https://iroh.computer/) for encrypted peer connectivity and NAT
traversal.

locho is deliberately a service tunnel, not a VPN or a hosted service platform:
it does not create a virtual network, route subnets, publish public URLs, or
require a locho account.

## The Service Model

```text
Host process
|-- API service   -> HTTP upstream   -> service capability A
|-- Database      -> TCP endpoint    -> service capability B
`-- Dev server    -> HTTP upstream   -> service capability C
```

Each service is configured explicitly and has an independent attachment
capability. A machine with a capability can attach that service locally. The
same capability may be used by multiple machines concurrently; locho does not
provide per-user or per-machine authorization.

```text
Machine A -- capability A --+
Machine B -- capability A --+--> API service on the host
Machine C -- capability B ------> Database on the host
```

## When to use locho

Use locho when developers need to:

- Reach a specific private HTTP or TCP service from another machine.
- Share selected services without exposing the host network.
- Avoid an SSH login, VPN deployment, or hosted tunnel account.
- Give several machines access to the same explicitly selected service.

Use another tool when you need:

- Remote shell administration: use SSH.
- Machine, IP, or subnet connectivity: use a VPN or network overlay.
- A public URL, webhooks, or public ingress: use a public tunnel provider.
- Team identity, per-user policy, audit, or service management: use a platform
  designed for those requirements.

## How it works

```text
Client machine                              Host machine
+------------------+                    +------------------+
| local HTTP proxy |                    | locho host       |
| or TCP listener  |===================>| selected service |
+------------------+   iroh connection  +------------------+
       ^                    encrypted             ^
       |                                           |
   local app                              HTTP upstream or
                                          TCP endpoint
```

The host owns the service configuration. An attachment capability authorizes
access to one service only. iroh may establish a direct peer connection or use
a relay when direct connectivity is unavailable. Relayed traffic remains
end-to-end encrypted, but public relay infrastructure does not provide an
availability guarantee.

## Usage

The host configuration defines multiple named HTTP and TCP services. The host
prints or otherwise provides an attachment capability for each service:

```toml
# locho.toml
[[services]]
name = "api"
type = "http"
upstream = "https://example.com"

[[services]]
name = "database"
type = "tcp"
endpoint = "127.0.0.1:5432"
```

Start the host with the validated configuration:

```sh
locho host --config locho.toml
```

The host prints one independent capability per service. Attach one selected
service locally:

```sh
locho attach <host-id> api <service-capability> --listen 127.0.0.1:8765
```

TCP service configuration is accepted and reserved for the TCP forwarding
milestone; attaching a TCP service currently returns an explicit unsupported
response rather than forwarding it as HTTP.

Rotate one service capability without affecting other services:

```sh
locho rotate-secret api
```

The host holds its state lock while running, so stop the host before rotating a
capability, then start it again to load the replacement.

An HTTP service is used through its local HTTP endpoint:

```sh
curl http://127.0.0.1:8765/path
```

A TCP service is attached to a local port and used by its native client:

```sh
locho attach <host-id> database <service-capability> --listen 127.0.0.1:5432
psql --host 127.0.0.1 --port 5432
```

Configuration is loaded and fully validated before the host starts. Service
names are unique, limited to letters, numbers, `-`, and `_`, HTTP upstreams
must use HTTPS, and each service must define only the endpoint field matching
its type.

## Security model

- iroh provides encrypted transport and cryptographic peer identities.
- locho capabilities provide application-level authorization for one service.
- Possession of a capability grants access to its service; it does not identify
  a human or independently identify a machine.
- Capabilities can be rotated or revoked per service.
- Multiple attached machines can use the same capability concurrently.
- Upstream TLS verification and local listener binding remain security-critical
  configuration choices.
- Credentials and host private keys must be protected like passwords and keys.

No locho account or application-managed coordinator is required. iroh discovery
and relay infrastructure may still participate in establishing connectivity.

## Scope

Included:

- Multiple explicitly configured HTTP and TCP services per host process.
- Independent service-scoped attachment capabilities.
- Concurrent attachments from multiple machines.
- Persistent host identity, capability rotation, and service revocation.
- Bounded resources, timeouts, diagnostics, and graceful lifecycle behavior.

Not included:

- Virtual network interfaces, subnet routing, or arbitrary host access.
- Public ingress or provider-managed application endpoints.
- Per-user identity, per-machine policy, team administration, or audit platform.
- Service discovery, hosted accounts, or a centralized locho control plane.

See [COMPARISON.md](COMPARISON.md), [FAQ.md](FAQ.md), and
[ROADMAP.md](ROADMAP.md) for product decisions and future direction.

## Development

Requirements:

- Rust stable with Cargo
- Network access for iroh discovery and relay connectivity

```sh
cargo build --release
cargo test
```

## License

Licensed under the MIT License. See [LICENSE](LICENSE).

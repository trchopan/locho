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
# Optional PEM CA certificate for a private HTTPS upstream.
# ca_cert = "/path/to/upstream-ca.pem"

[[services]]
name = "database"
type = "tcp"
endpoint = "127.0.0.1:5432"
```

Start the host with the validated configuration:

```sh
locho host --config locho.toml
```

For a host that must provide an explicit reachable address to an attachment,
bind it to that address and pass the same address to `locho attach` with
`--direct-address`. This is useful for local testing or networks where peer
discovery cannot advertise the host address:

```sh
locho host --config locho.toml --bind-address 192.0.2.10:12345
locho attach <host-id> api <service-capability> \
  --direct-address 192.0.2.10:12345 --listen 127.0.0.1:8765
```

Use a fixed reachable port instead of `0` when sharing the address with an
attachment.

The host prints one independent capability per service. Attach one selected
service locally:

```sh
locho attach <host-id> api <service-capability> --listen 127.0.0.1:8765
```

Check local state and configuration without printing capabilities:

```sh
locho diagnose --config locho.toml
locho diagnose --host-id <host-id>
locho diagnose --host-id <host-id> --direct-address 192.0.2.10:12345
```

The optional host probe reports the concrete iroh transport path. `direct(...)`
means a direct UDP path, `relay(...)` means traffic is currently using a relay,
and `mixed(...)` means iroh has both a direct and relay path available. An
attachment prints its initial path and continues reporting path changes while
running. Use `--direct-address` when discovery cannot advertise a reachable
address to the probing machine. A relay path is encrypted end-to-end, but relay
availability and performance remain external infrastructure dependencies.

TCP services are attached to a local TCP listener and forward bidirectionally:

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

HTTP request and response bodies are streamed through the tunnel. Known-length
bodies use length framing; chunked bodies use bounded chunk framing. Individual
bodies are limited to 32 MiB, and HTTP requests have a 30-second upstream
timeout. Hop-by-hop headers are not forwarded. WebSocket upgrades are not
supported. HTTPS certificates are validated against the normal system roots by
default; `ca_cert` may explicitly add a PEM CA certificate for a private
upstream.

A TCP service is attached to a local port and used by its native client:

```sh
locho attach <host-id> database <service-capability> --tcp --listen 127.0.0.1:5432
psql --host 127.0.0.1 --port 5432
```

## Installation

Released binaries are published on the
[GitHub Releases page](https://github.com/trchopan/locho/releases). The
supported `1.0.0` targets are:

- Linux x86_64 and ARM64
- macOS x86_64 and Apple silicon
- Windows x86_64

On Unix, download and inspect the installer before running it:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/trchopan/locho/releases/latest/download/locho-installer.sh \
  -o locho-installer.sh
less locho-installer.sh
sh locho-installer.sh
```

On Windows PowerShell:

```powershell
Invoke-WebRequest `
  https://github.com/trchopan/locho/releases/latest/download/locho-installer.ps1 `
  -OutFile locho-installer.ps1
Get-Content .\locho-installer.ps1
.\locho-installer.ps1
```

Manual installation is also supported: download the archive for the operating
system and architecture, extract `locho` or `locho.exe` into a directory on
your `PATH`, and verify the matching `.sha256` entry or the unified `sha256.sum`
file. The release archive contains the binary, `README.md`, `CHANGELOG.md`, and
`LICENSE`.

Upgrading replaces only the executable. Host identity and service capabilities
remain in the application state directory and are preserved across upgrades.
Back up that directory before upgrading, protect capabilities like passwords,
and rotate a capability if it may have been exposed. Release binaries need
outbound network access for iroh discovery and relay connectivity; no locho
account or hosted coordinator is required.

TCP attachments use a 10-second handshake and upstream connection timeout and
close idle connections after 5 minutes. At most 128 TCP connections are active
per host and per attachment process. If the configured endpoint is unavailable,
the attachment reports a gateway failure; it never connects to an arbitrary
address.

Host and attachment processes handle Ctrl-C gracefully: they stop accepting new
connections, close active tunnel connections, and wait up to 10 seconds for
active tasks to finish before terminating remaining tasks. Tunnel handshakes
also have a 10-second timeout, and HTTP clients enforce a 30-second upstream
request/response timeout.

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

Build requirements:

- Rust stable with Cargo
- Network access for iroh discovery and relay connectivity

```sh
cargo build --release
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contributor workflow, complete
quality gate, integration tests, and release verification commands. Maintainers
preparing a release should follow [RELEASE.md](RELEASE.md).

## License

Licensed under the MIT License. See [LICENSE](LICENSE).

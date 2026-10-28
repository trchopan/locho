# locho: Private HTTP and TCP Service Tunnel

> local + echo = locho

`locho` is an open-source, developer-oriented private service tunnel for
connecting to selected HTTP APIs, web services, databases, and TCP or Unix
stream endpoints from another machine. It works as an HTTP reverse proxy or
local TCP/Unix stream forwarder, using [iroh](https://iroh.computer/) for
encrypted peer-to-peer connectivity and NAT traversal.

`locho` gives an authorized machine a local endpoint for one explicitly selected
service without exposing the host network. It is _a focused alternative_ to SSH
port forwarding, a VPN, or a hosted tunnel when you need private access to a
small set of development or infrastructure services.

## Why locho?

- Tunnel private HTTP and TCP/Unix services to local endpoints.
- Authorize each service independently with a service-scoped capability or
  portable ticket.
- Connect directly between peers or use encrypted relay connectivity when
  direct networking is unavailable.
- Share selected services without exposing the host network or other ports.
- Use `locho` without an account or application-managed control plane.
- Run on Linux, macOS, and Windows with a single Rust binary, or connect from
  Browser and Flutter apps using the beta SDKs.

`locho` is deliberately a service tunnel, not a VPN or a hosted service platform:
it does not create a virtual network, route subnets, publish public URLs, or
provide remote shell access.

Start with the [installation instructions](#installation) and
[5-minute quickstart](#quickstart), browse the [documentation hub](docs/README.md),
see the [FAQ](docs/faq.md), or read the [tool comparison](docs/comparison.md).

## Installation

Released binaries are published on the
[GitHub Releases page](https://github.com/trchopan/locho/releases) for:

- Linux `x86_64` and `ARM64`
- macOS `x86_64` and Apple silicon (`ARM64`)
- Windows `x86_64`

### Stable Release (`1.3.0`)

On Linux and macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/trchopan/locho/releases/latest/download/locho-installer.sh | sh
```

On Windows PowerShell:

```powershell
Invoke-WebRequest `
  https://github.com/trchopan/locho/releases/latest/download/locho-installer.ps1 `
  -OutFile locho-installer.ps1
Get-Content .\locho-installer.ps1
.\locho-installer.ps1
```

### Client SDKs (Beta)

The CLI `latest` installers follow the stable CLI release (`1.3.0`). Browser
(`locho_web`) and Flutter (`locho_flutter`) SDKs are independently versioned and
published in beta at `0.1.0-beta.3`; see
[SDK and prerelease provenance](docs/sdk-releases.md) for installation
instructions, registry checksums, and target verification boundaries.

### Manual Installation and Upgrades

You can also download the release archive for your OS and architecture, extract
`locho` or `locho.exe` onto your `PATH`, and verify the matching `.sha256` entry
or `sha256.sum`. Upgrading replaces only the executable; host identity and
service capabilities remain in the application state directory and are preserved
across upgrades.

## Quickstart

### 1. Configure Services on the Host

Define one or more named HTTP or TCP services in `locho.toml`:

```toml
# locho.toml
[[services]]
name = "api"
type = "http"
upstream = "https://example.com"
# Optional total upstream request/response timeout in seconds (1-300, default 60):
# upstream_timeout_secs = 90
# Optional PEM CA certificate for a private HTTPS upstream:
# ca_cert = "/path/to/upstream-ca.pem"

[[services]]
name = "database"
type = "tcp"
endpoint = "127.0.0.1:5432"
# Or, on macOS/Linux, replace endpoint with a filesystem Unix stream socket:
# unix_socket = "/home/alice/.config/example/api.sock"
# tcp_idle_timeout_secs = 0 # omit for 300s default; 0 disables byte-idle expiry
```

### 2. Start the Host

```sh
locho host --config locho.toml
```

The host validates `locho.toml` before starting, initializes its persistent
identity and per-service secrets, and prints its Host ID and configured service
names (it never prints capability secrets automatically).

### 3. Share a Service Capability

Generate a ready-to-run `locho attach` command for one service and share it
through a trusted channel:

```sh
locho share api --config locho.toml
```

- For scripts that need only the `<service>:<type>:<secret>` token:
  `locho secret api --config locho.toml`
- For Browser and Flutter SDKs that consume a `locho1:` portable ticket:
  `locho share database --config locho.toml --ticket` (see
  [portable tickets v1](docs/tickets.md)).

### 4. Attach on the Client Machine

Attach an HTTP service to a local HTTP endpoint:

```sh
locho attach <host-id> api:http:<secret> --listen 127.0.0.1:8765
curl http://127.0.0.1:8765/path
```

Or attach a TCP service to a local port and connect with its native client:

```sh
locho attach <host-id> database:tcp:<secret> --listen 127.0.0.1:5432
psql --host 127.0.0.1 --port 5432
```

## The Service Model & How It Works

```text
Host process
|-- API service   -> HTTP upstream   -> service capability A
|-- Database      -> TCP endpoint    -> service capability B
`-- Dev server    -> HTTP upstream   -> service capability C
```

Each configured service has an independent attachment capability. A machine with
capability A can attach the API service locally, and multiple machines may use
the same capability concurrently:

```text
Machine A -- capability A --+
Machine B -- capability A --+--> API service on the host
Machine C -- capability B ------> Database on the host
```

```text
Client machine                              Host machine
+------------------+                    +------------------+
| local HTTP proxy |                    | locho host       |
| or TCP listener  |===================>| selected service |
+------------------+   iroh connection  +------------------+
       ^                    encrypted             ^
       |                                          |
   local app                              HTTP upstream or
                                          TCP/Unix endpoint
```

- **When to use `locho`**: Reach a specific private HTTP or TCP/Unix service
  from another machine without exposing the host network, creating a VPN, or
  requiring an SSH login.
- **When to use another tool**: Use SSH for remote shell administration, a VPN
  or network overlay for subnet/IP routing, a public tunnel provider for public
  URLs/webhooks, or an enterprise access platform for per-user identity and
  audit logging. See [COMPARISON.md](docs/comparison.md).

## Common CLI Operations

### Multi-Service Attachments (`attachments.toml`)

Attach multiple services in one client process over a single shared `iroh`
connection:

```toml
# attachments.toml
host_id = "<host-id>"
direct_address = "192.0.2.10:12345" # optional
listen_host = "127.0.0.1"           # optional; defaults to localhost

[[services]]
capability = "api:http:<secret>"
listen_port = 8765
# http_timeout_secs = 90 # optional (1-300, default 60)

[[services]]
capability = "database:tcp:<secret>"
listen_port = 5432
# Or on macOS/Linux, replace listen_port with an absolute Unix socket path:
# listen_unix = "/home/bob/.local/run/locho/database.sock"
# tcp_idle_timeout_secs = 0
```

```sh
locho attach --config attachments.toml
```

### Rotating a Service Capability

Stop the running host (which holds the state lock), rotate one service's secret
without affecting other services, and restart the host:

```sh
locho rotate-secret api --config locho.toml
```

### Diagnostics and Explicit Direct Addressing

Check local configuration or probe the concrete transport path (`direct(...)`,
`relay(...)`, or `mixed(...)`) to a host without printing capabilities:

```sh
locho diagnose --config locho.toml
locho diagnose --host-id <host-id>
```

When peer discovery cannot advertise a reachable host address (such as in
isolated local test networks), bind the host to a fixed address and pass
`--direct-address` to clients:

```sh
locho host --config locho.toml --bind-address 192.0.2.10:12345
locho attach <host-id> api:http:<secret> --direct-address 192.0.2.10:12345 --listen 127.0.0.1:8765
```

### Protocol Behavior & Operational Limits

- **HTTP services**: Request and response bodies stream up to 32 MiB per body.
  Upstream and attachment timeouts default to 60 seconds (`1`–`300` seconds).
  Hop-by-hop headers and WebSocket upgrades are unsupported (use a TCP service
  for non-HTTP protocols). HTTP upstreams must use HTTPS (`ca_cert` may add a
  custom PEM CA).
- **TCP & Unix stream services**: 10-second handshake and upstream connection
  timeout, 5-minute default application-byte idle timeout (configurable via
  `tcp_idle_timeout_secs`), and up to 128 active connections per host and per
  attachment process.
- **Reconnection & shutdown**: Local listeners stay bound while the tunnel
  reconnects; in-flight requests and streams are never automatically replayed.
  `Ctrl-C` (and `SIGTERM` on Unix attachments) stops accepting new connections
  and drains active tasks for up to 10 seconds.

### Advanced Operational Guides

- **[Unix Stream Sockets (macOS/Linux)](docs/unix-sockets.md)** — Exposing Unix
  socket upstreams and attaching local Unix socket listeners (`--listen-unix`),
  path length limits, idle policies, and directory security.
- **[Custom iroh Relays](docs/custom-relays.md)** — Configuring self-hosted
  `iroh-relay` servers (`relay.toml`), bearer token environment variables, QUIC
  Address Discovery, and manual relay verification.
- **[Portable Tickets v1](docs/tickets.md)** — Generating and consuming
  `locho1:` single-service tickets with optional peer relay hints.

## Client SDKs (Browser & Flutter Beta)

Browser and foreground Flutter applications can connect to TCP-configured
`locho` services using portable `locho1:` tickets without installing the CLI on
client devices:

| Platform | Package | Documentation |
| --- | --- | --- |
| **Browser (TypeScript / WASM)** | [`locho_web` on npm](https://www.npmjs.com/package/locho_web) | [Browser SDK Guide](packages/locho-web/README.md) · [Browser HTTP Spec](docs/browser-http.md) |
| **Flutter (Android / iOS)** | [`locho_flutter` on pub.dev](https://pub.dev/packages/locho_flutter) | [Flutter SDK Guide](packages/locho_flutter/README.md) · [Source-Build Packaging](docs/flutter-package.md) |
| **Rust (Workspace Crate)** | `locho-client-native` (internal `0.1.0`) | [Native Client API](docs/native-client.md) · [SDK Foundation](docs/sdk-foundation.md) |

See [SDK and prerelease provenance](docs/sdk-releases.md) for registry
checksums, version compatibility, and current mobile/browser beta limitations.

## Security Model & Scope

- **Encrypted peer transport**: `iroh` provides end-to-end encrypted connections
  and cryptographic peer identities.
- **Service-scoped capabilities**: Possession of a capability or ticket grants
  access to that single service; it does not identify an individual user or
  expose other services on the host.
- **Safe operation**: Protect host state and capability tokens like passwords,
  rotate leaked capabilities with `locho rotate-secret`, and review
  [SECURITY.md](SECURITY.md) for vulnerability reporting and Unix socket
  security requirements.
- **Product scope**: See [docs/roadmap.md](docs/roadmap.md) for completed
  milestones, explicit non-goals, and architectural decision gates.

## Development & Contributing

Build and run the test suite with Rust `1.96+` and Cargo:

```sh
cargo build --release
cargo test
```

We welcome bug reports, documentation improvements, and pull requests! See:

- **[CONTRIBUTING.md](CONTRIBUTING.md)** for the repository architecture map,
  local Rust/Browser/Flutter development setup, and CI quality gates.
- **[Documentation Hub (`docs/README.md`)](docs/README.md)** for the complete
  index of guides and specifications.
- **[Release Checklist (`docs/releasing.md`)](docs/releasing.md)** for
  maintainer release verification steps.

## License

Licensed under the MIT License. See [LICENSE](LICENSE).

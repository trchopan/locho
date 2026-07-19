# locho FAQ

## What is locho?

locho is a developer-oriented private tunnel for explicitly selected HTTP and
TCP services. One host process can expose multiple named services. An
authorized machine attaches a selected service to a local HTTP endpoint or TCP
port.

locho is not a VPN, public ingress provider, remote administration tool, or
service-management platform.

## What problem does locho solve?

locho lets a developer use a private service on another machine as a local
endpoint without exposing the host network, creating a virtual network, or
requiring an SSH login.

Typical examples include:

- Accessing a private HTTP API from a laptop.
- Connecting a database client to a selected remote database port.
- Sharing several development services with other machines.
- Giving access to one service without exposing every service on the host.

## How does the service model work?

The workflow is:

```text
Configure services -> Start host -> Share a service capability -> Attach locally
```

Each configured service has its own attachment capability:

```text
API service      -> capability A
Database service -> capability B
```

Capability A cannot be used to attach the database service.

The host loads this model from a TOML file using `locho host --config`. Service
names must be unique and safe, HTTP services require HTTPS `upstream` URLs, and
TCP services require an explicit `endpoint`.

Use `locho rotate-secret <service>` to revoke the previous capability for one
service and issue a replacement. Existing tunnel streams are not retroactively
closed; new requests must use the replacement capability. Stop the host before
running the command because the host holds the state lock while running, then
restart it to load the replacement.

## Can one host expose multiple services?

Yes. A single host process can expose multiple explicitly configured HTTP and
TCP services. Each service has its own name, endpoint, lifecycle, and
attachment capability.

## Can multiple machines attach simultaneously?

Yes. Multiple machines can use the same service capability concurrently. The
capability authorizes access to the service; it does not identify an individual
machine or user.

This is intentionally simpler than a team access-management system. There is
no per-user or per-machine policy.

## What happens if a capability is shared?

Anyone who possesses a service capability can use that service. Capabilities
must therefore be treated as credentials, shared only through an appropriate
secure channel, and rotated when exposed.

Sharing a capability does not grant access to other configured services or to
the host machine as a whole.

## How are HTTP services used?

An HTTP service is attached to a local HTTP endpoint. Applications send their
requests to that endpoint, for example:

```bash
curl http://127.0.0.1:8765/path
```

The host forwards requests to the explicitly configured HTTP upstream.

Request and response bodies are streamed and limited to 32 MiB per body. The
upstream request timeout is 30 seconds. WebSocket upgrades are not supported;
use a TCP service when a non-HTTP protocol is required.

## How are TCP services used?

A TCP service is attached to a local TCP listener. Native clients then connect
to that local port:

```bash
psql --host 127.0.0.1 --port 5432
```

Start the TCP attachment with `locho attach ... --tcp --listen 127.0.0.1:5432`.

locho forwards the selected TCP service only. It does not provide arbitrary IP
connectivity or a way to reach other ports on the host.

TCP connections have a 10-second upstream connection timeout, a 5-minute idle
timeout, and a limit of 128 active connections per host and attachment process.
EOF and half-close behavior is propagated between the local listener and the
configured endpoint.

## What happens when a process is stopped?

Ctrl-C stops accepting new local or remote connections and closes active tunnel
connections. The process waits up to 10 seconds for active tasks to finish
before forcing termination. Tunnel handshakes have a 10-second timeout, HTTP
upstream request/response operations have a 30-second timeout, and TCP sessions
have a 5-minute idle timeout.

## Does locho create a VPN?

No. locho does not:

- Create virtual network interfaces.
- Assign machine or subnet IP addresses.
- Route arbitrary traffic between networks.
- Expose an entire machine or subnet by default.

It creates local endpoints for explicitly configured and authorized services.

## How does connectivity work?

locho uses iroh for encrypted peer connectivity and NAT traversal. It attempts
to establish a direct peer connection and can use a relay when direct
connectivity is unavailable.

No manual inbound port forwarding is required, but both machines still need
network access for discovery and relay connectivity.

Run `locho diagnose --host-id <host-id>` to test connectivity and report the
current iroh path. The result identifies a direct, relay, or mixed path without
printing service capabilities. An attachment also logs and prints its selected
transport path when it connects and when iroh changes paths.

## Can a relay read the application traffic?

Relays carry encrypted iroh traffic and are not intended to read the tunneled
application data. Relays are still infrastructure dependencies: public relays
may be rate-limited and do not provide an uptime or performance guarantee.

## Does locho require a central service?

The core workflow does not require a locho account or application-managed
coordinator. iroh discovery and relay infrastructure may participate in
connecting peers.

This means "no locho control plane," not "no network infrastructure is ever
used."

## How are services authorized?

Authorization has two layers:

- iroh provides encrypted transport and cryptographic peer identity primitives.
- locho uses a service attachment capability to authorize application access.

A capability is scoped to one service. Possession of it grants access to that
service, but does not identify a human or independently identify a machine.

## Can access be revoked?

Yes. Capabilities can be rotated or revoked independently per service. The
exact behavior for existing connections and new attachments is defined by the
CLI and protocol contract, and must be documented clearly.

Host identity and service capabilities have separate lifecycle controls. Reset
the host identity only when the host key itself must be replaced.

## Is locho secure?

locho is designed with encrypted peer transport, explicit service selection,
service-scoped capabilities, bounded resources, and safe local defaults.

Security still depends on correct operation:

- Protect host private keys and service capabilities.
- Do not expose local listeners beyond the intended machine or network.
- Rotate a capability if it leaks.
- Validate upstream TLS certificates and names.
- Treat relay availability and operational logging as separate concerns from
  transport encryption.

locho does not provide per-user identity, per-machine authorization, centralized
policy, or audit-grade access records.

## Why use locho instead of SSH forwarding?

SSH remains an excellent choice for remote administration and forwarding
through an existing account or bastion. It provides mature key management,
configuration, auditing, and ecosystem integration.

locho is useful when:

- A service should be shared without granting a shell account.
- An SSH daemon or reachable bastion is not available.
- One host should expose several named HTTP and TCP services.
- Capabilities should be rotated or revoked independently per service.
- iroh peer connectivity and relay fallback fit the environment better.

locho is not intended to replace SSH for general administration.

## How is locho different from Tailscale or ZeroTier?

Network overlays connect machines, addresses, and subnets through a virtual
network. They are the right choice when broad network access is needed.

locho starts and ends at explicitly configured HTTP and TCP services. It does
not enroll machines into a network or make arbitrary addresses reachable.

Some network-overlay products also support application-oriented access. The
difference is locho's narrower, self-contained developer workflow rather than
an absolute claim that other tools cannot reach a single service.

## How is locho different from ngrok or Cloudflare Tunnel?

Public tunnel and access platforms commonly provide hosted routing, public
URLs, webhooks, identity providers, and operational guarantees. Cloudflare
Tunnel also supports private-network use cases.

locho is a private peer-to-peer service tunnel. A client attaches to a selected
service on another machine; the service is not published as a public URL, and
no locho account is required.

## How is locho different from bore or zrok?

`bore` is a simple focused TCP tunnel. `zrok` provides a broader sharing
workflow with open-source and hosted deployment options.

locho combines private peer connectivity, multiple explicitly configured HTTP
and TCP services, and independent service capabilities. It is narrower than a
sharing platform and more structured than a single-purpose TCP forwarder.

## Is locho a service platform?

No. locho is a developer utility. It does not aim to manage services, users,
teams, deployments, policies, or hosted infrastructure.

The host owns its service configuration and lifecycle. locho provides the
tunnel between an authorized machine and a selected service.

## Is locho suitable for production workloads?

locho is intended primarily for developer workflows, internal tools,
debugging, and controlled service sharing. It should not be treated as a
replacement for production ingress, a VPN, an identity platform, or a managed
availability service.

Production use requires independently evaluating relay availability, service
capacity, credential handling, observability, failure behavior, and operational
support.

## What are locho's limitations?

locho intentionally does not provide:

- Full network connectivity or subnet routing.
- Public ingress or provider-hosted application URLs.
- Unscoped access to a host or its network.
- Per-user or per-machine authorization.
- A service registry, hosted account system, or team administration platform.

Its security and usability depend on careful service selection, capability
management, host-key protection, and appropriate operational limits.

## What is the long-term direction?

The project may improve configuration, diagnostics, protocol performance,
optional service exchange, relay deployment, and developer integrations while
preserving its service-level identity.

Features that turn locho into a network overlay, hosted platform, or team
identity system require a separate product decision and are not part of the
project contract.

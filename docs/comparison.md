# locho Compared with Other Tools

## Positioning

locho is a developer utility for private access to explicitly selected HTTP and
TCP services. One host process can expose multiple services, each with its own
attachment capability. Several machines may use the same capability, but locho
does not attempt to identify or manage individual users.

```text
Network overlay          locho                  Public tunnel
----------------         ----------------      ----------------
Machines and networks    Selected services     Public or hosted ingress
```

The useful choice is the scope of access:

| Need | Best fit |
| --- | --- |
| Machines, IPs, and subnets | VPN or network overlay |
| Public URL, webhooks, or external users | Public tunnel provider |
| Remote shell and administration | SSH |
| Explicit private HTTP/TCP services | locho |

No tool is universally better. locho is optimized for a small, explicit set of
services without an SSH login, virtual network, or locho-managed account.

## Comparison Summary

| Capability | Network overlay | Public tunnel | SSH forwarding | locho |
| --- | --- | --- | --- | --- |
| Access scope | Machines, IPs, subnets | Public/provider endpoint | Forwarded ports via SSH host | Explicit HTTP/TCP services |
| Typical direction | Network to network | Local service to public endpoint | Client through login host | Client to private remote service |
| Local abstraction | Virtual network and routes | Public URL or endpoint | SSH session and local port | Local HTTP proxy or port |
| Authentication | Network identity and policy | Provider/account policy | SSH credentials | Per-service attachment capability |
| Multiple services | Broad network access | Provider configuration | Multiple forwards | Explicit services in one host |
| User management | Often available | Usually available | Account/key based | Deliberately out of scope |
| locho account required | Depends | Usually | No | No |

## locho and VPNs

Tailscale, ZeroTier, and WireGuard-based overlays are appropriate when a client
needs broad access to machines or networks:

```text
Client -> virtual network -> host, machines, and subnets
```

They commonly provide network interfaces, routes, device identity, and policy
across many machines. Tailscale also supports application-oriented workflows,
so the distinction is not that it cannot reach one service. The distinction is
that locho starts and ends at explicitly configured services rather than
creating a network for the machines.

Choose locho when:

- Only selected HTTP or TCP services should be reachable.
- A local proxy or forwarded port is sufficient.
- No virtual network or device enrollment is wanted.
- Several machines may share a service capability without per-user policy.

Choose a network overlay when clients need to discover or reach arbitrary
machines, addresses, or subnets.

## locho and Public Tunnel Providers

ngrok and Cloudflare Tunnel are strong choices for public ingress, webhooks,
provider-managed routing, identity-aware access, and operational features:

```text
Local service -> provider network -> public or managed endpoint
```

Cloudflare Tunnel also supports private-network and Zero Trust use cases, so it
is not limited to public URLs. locho differs in its intended operating model:

```text
Client -> iroh connection -> private selected service
```

locho does not publish a public URL or require a hosted account. iroh may use
discovery and relay infrastructure, but application traffic remains encrypted
between the peers. Use a public tunnel or access platform when hosted routing,
public reachability, identity providers, or availability guarantees are needed.

## locho and SSH

SSH remains the mature choice for remote administration and forwarding through
an existing account or bastion:

```bash
ssh -L 4096:localhost:4096 user@host
```

SSH provides strong ecosystem integration, key management, configuration,
bastions, and operational familiarity. locho is not intended to replace it.

locho is useful when:

- The user should access a service without receiving a shell account.
- An SSH daemon or reachable SSH bastion is not available.
- A host should expose several named HTTP and TCP services.
- Service capabilities should be rotated or revoked independently.
- iroh peer connectivity and relay fallback are preferable to SSH networking.

Use SSH when administration, auditing, or an existing SSH workflow is the main
requirement.

## locho and bore or zrok

`bore` is a focused TCP tunnel with a simple client/server model. It is a good
choice when the goal is uncomplicated TCP forwarding, especially public or
self-hosted forwarding. `zrok` provides a broader sharing workflow with open
source and hosted deployment options.

locho differs by combining explicit multi-service configuration, private
peer-to-peer access, HTTP and TCP service types, and independent service
capabilities in one developer workflow. This is a narrower product than a
sharing platform and less minimal than a single-purpose TCP forwarder.

## Connectivity and Trust

locho does not require manual inbound port forwarding. iroh attempts direct
peer connectivity and can use a relay when direct connectivity is unavailable.
Relays can carry encrypted traffic but do not provide a blanket availability
or performance guarantee.

The trust model has separate layers:

- iroh supplies encrypted transport and peer identity primitives.
- locho capabilities authorize access to one configured service.
- Capability possession is not a human identity or per-machine identity.
- A capability can be shared by multiple machines and revoked for that service.

No locho account or application-managed control plane is required for the core
workflow.

## What locho Is Not

locho is not:

- A VPN, mesh network, or subnet router.
- A public ingress service or URL provider.
- A general remote administration tool.
- A per-user access-management or audit platform.
- A way to expose an entire machine by default.

Its purpose is focused: let developers share the private services a host
explicitly chooses to expose.

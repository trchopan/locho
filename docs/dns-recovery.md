# CLI DNS Recovery

## Default behavior

Locho normally uses iroh's system DNS configuration and deferred public fallback
resolvers. An unreachable system tier can consume the outer lookup deadline
before deferred fallback runs. A successful cached system lookup does not prove
the configured DNS servers are reachable.

Compare direct queries to configured servers and public fallback servers before
changing policy. A system DNS configuration or network repair may be preferable
to changing application resolution behavior.

## Opt-in independent-tier racing

Current source supports this per-process option:

```sh
LOCHO_DNS_EAGER_FALLBACK=1 locho host --config locho.toml
LOCHO_DNS_EAGER_FALLBACK=1 locho attach --config attachments.toml
LOCHO_DNS_EAGER_FALLBACK=1 locho diagnose --host-id <host-id>
```

It affects DNS used by CLI endpoints in host, attachment, and diagnostic commands,
with or without custom relays. It does not configure the native, Flutter, or
browser SDKs, change upstream HTTP-client DNS, or modify system DNS settings.
Only the value `1` enables it; unset the variable to retain the default behavior.
The official `1.3.0` release artifact does not include this source change.

The system and iroh's existing public fallback resolvers run as independent tiers.
This avoids unreachable system servers occupying all query slots in a merged
nameserver list. The option keeps existing outer request deadlines and TLS trust
policy; it does not replay requests or resume interrupted streams.

### Answer policy

- The first non-empty answer wins, including when both tiers return different
  records. Simultaneous answers have no guaranteed system-first preference.
- An empty answer (NODATA) waits for the other tier rather than suppressing its
  usable records.
- Two empty answers, or an empty answer plus an error, produce an empty answer.
- If both tiers fail, the system-tier error is returned consistently.
- If the outer deadline expires while waiting, the lookup times out.

This policy applies to A, AAAA, and TXT records. Network resets recreate the tiers
lazily so system configuration and caches can be refreshed on the next lookup.

### Privacy and split-horizon tradeoffs

**This is not system-first fallback.** It can send lookup names to public DNS
providers even when system DNS is healthy, and a faster public answer can override
a valid private split-horizon answer. Private relay names and discovery queries
may be disclosed; public and private TXT records can also differ.

Do not enable it where public DNS queries are prohibited or private DNS answer
selection is required. Custom-only relay transport does not imply private-only
DNS. Unchanged TLS verification does not eliminate these resolution-policy risks.

## Diagnostics and regression tests

The optional example queries a fixed public name using Cloudflare UDP, TCP, and
HTTPS, then the dependency's merged eager mode:

```sh
cargo run --locked --example dns-probe
```

It does not load identities or capabilities, but it does make external DNS
queries. Results depend on local network state. It does not exercise the CLI's
independent-tier resolver or deterministically reproduce query-slot starvation.
Debug output may include configured DNS server addresses; sanitize it before
sharing diagnostics.

For deterministic regressions using local UDP fixtures only:

```sh
cargo test --locked --bin locho relay::tests::eager_dns_answers_before_unreachable_primary_deadline
cargo test --locked --bin locho dns::tests
```

These tests cover unreachable primary servers, empty versus usable A/AAAA/TXT
answers, failure selection, conflicts, and cancellation. DNS recovery does not fix
descriptor leaks or macOS network-interface enumeration crashes; see the separate
[macOS SSH operational guide](macos-ssh-tunnel.md).

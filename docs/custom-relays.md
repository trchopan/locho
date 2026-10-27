# Custom iroh Relays

By default, `locho` uses [iroh](https://iroh.computer/)'s public N0 relays for
NAT traversal and encrypted fallback connectivity when a direct peer-to-peer UDP
path is unavailable.

Host, attachment, and diagnostic commands can optionally load a shared relay
configuration file (`--relay-config`) to use self-hosted `iroh-relay` servers
either exclusively or alongside the default N0 relays.

## Relay Configuration (`relay.toml`)

```toml
# relay.toml
include_n0_relays = false

[[relays]]
url = "https://relay.example.org"
token_env = "LOCHO_RELAY_TOKEN"
quic_address_discovery = true
```

### Configuration Fields

- **`include_n0_relays`** (`bool`, required):
  - `false`: Use only the custom relays listed in `[[relays]]` for relay
    transport.
  - `true`: Include both the custom relays and the default N0 relays as
    connectivity candidates.
- **`url`** (`string`, required): HTTPS URL of the `iroh-relay` server. Plain
  `http://` relay URLs are rejected; the relay must present a TLS certificate
  trusted by the normal system root store.
- **`token_env`** (`string`, optional): Name of an environment variable
  containing the relay bearer token. The token value is read from the
  environment at runtime and is never printed by `locho` or included in normal
  diagnostics.
- **`quic_address_discovery`** (`bool`, optional, defaults to `true`): Enables
  QUIC Address Discovery (QAD) on the relay's default QAD UDP port (`7842`). Set
  this to `false` when the relay is behind an HTTPS-only reverse proxy that does
  not forward UDP port `7842`.

## Running with a Custom Relay

Supply the same relay configuration to the host, attachment, and diagnostic
commands:

```sh
export LOCHO_RELAY_TOKEN="replace-with-a-secret"
locho host --config locho.toml --relay-config relay.toml
locho share api --config locho.toml --relay-config relay.toml
locho attach <host-id> api:http:<secret> --relay-config relay.toml
locho diagnose --host-id <host-id> --relay-config relay.toml
```

When `--relay-config` is passed to `locho share` or `locho rotate-secret`, the
generated `locho attach` command automatically includes `--relay-config`. Ensure
the file path exists on the client machine (or adjust the path if the two
machines use different directory layouts).

## Path Selection and Discovery Behavior

- **Candidate set, not a priority list**: Relay entries form a candidate set.
  iroh always prefers a working direct UDP path over relayed paths and selects
  among reachable relays based on measured latency.
- **Discovery vs. relay transport**: Setting `include_n0_relays = false`
  restricts relay transport to your custom relays, but the current iroh endpoint
  builder still uses N0 discovery services to publish and resolve bare host IDs.
  When N0 discovery access is not acceptable, pair `--relay-config` with an
  explicit `--bind-address` on the host and `--direct-address` on the client:
  ```sh
  locho host --config locho.toml --relay-config relay.toml --bind-address 192.0.2.10:12345
  locho attach <host-id> api:http:<secret> --relay-config relay.toml --direct-address 192.0.2.10:12345
  ```
- **QUIC Address Discovery (QAD) warnings**: Some networks (CGNAT, VPNs,
  cellular) use destination-dependent NAT mappings. iroh detects this via QAD
  probes across relays. This can make direct UDP hole-punching less reliable,
  causing `locho` to fall back to an encrypted relay path as designed.

## Manual Relay Verification

`locho` configures relay clients; it does not run a relay server. To deploy a
self-hosted relay, follow the version-matched
[`iroh-relay` server documentation](https://github.com/n0-computer/iroh/tree/v1.4.0/iroh-relay),
which covers building the server, TLS certificates, QUIC Address Discovery,
access control, and metrics.

To verify custom-only and fallback relay operation:

1. Deploy a version-matched `iroh-relay` server with a trusted HTTPS certificate
   and configure a shared server access token:
   ```toml
   access.shared_token = ["replace-with-a-secret"]
   ```
2. Create a `relay.toml` on both the host and attachment machines (keeping the
   secret in `LOCHO_RELAY_TOKEN` rather than in the file):
   ```toml
   # relay.toml
   include_n0_relays = false

   [[relays]]
   url = "https://relay.example.org"
   token_env = "LOCHO_RELAY_TOKEN"
   quic_address_discovery = false
   ```
3. Start the host, generate the attach command, attach, and run `diagnose`:
   ```sh
   export LOCHO_RELAY_TOKEN="replace-with-a-secret"
   locho host --config locho.toml --relay-config relay.toml
   locho share api --config locho.toml --relay-config relay.toml
   locho attach <host-id> api:http:<secret> --relay-config relay.toml
   locho diagnose --host-id <host-id> --relay-config relay.toml
   ```
4. Confirm that `locho diagnose` reports the configured relay and a `relay(...)`
   or `mixed(...)` transport path when direct UDP connectivity is unavailable.
5. To verify N0 fallback, set `include_n0_relays = true` and repeat with an
   unreachable custom relay URL.

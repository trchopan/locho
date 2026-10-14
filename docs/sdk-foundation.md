# SDK Foundation

The workspace currently contains the existing `locho` CLI and an unpublished
`locho-core` 0.1.0 library. This is the first shared extraction for future Flutter
and browser SDKs, not a complete attachment SDK.

## Implemented Boundary

`crates/locho-core` owns:

- Existing `locho/3` protocol constants, request/response serialization, bounded
  framing/body helpers, and forwarded-TCP idle behavior.
- Existing authorization proof derivation and constant-time verification.
- Service types/name validation and typed, sanitized capability parsing.
- Optional `transport` feature: validated in-memory relay settings and bearer
  credentials, preserving custom/default relay-map behavior.

The CLI/host use these same implementations. Native secret generation remains in
the CLI, avoiding an OS RNG requirement in the protocol-only library. Filesystem,
TOML/environment adaptation, process signals, logging initialization, endpoint
creation, listener ownership and supervision remain in native CLI modules.
Debug representations redact capability secrets, protocol proofs and application
headers; serialization retains the existing wire format.

Example in-memory native relay policy:

```rust,no_run
use locho_core::relay::{RelayError, RelayOptions, RelaySettings};

fn relay_policy(token_from_application: String) -> Result<RelaySettings, RelayError> {
    RelaySettings::new(false, vec![RelayOptions {
        url: "https://relay.example.org".into(),
        auth_token: Some(token_from_application),
        quic_address_discovery: true,
    }])
}
// The caller applies the returned settings.mode() to its iroh endpoint builder.
```

Relay policy configures the client's endpoint. It is not a host peer-address hint.
Passing a token to native iroh does not establish browser relay-auth support.

## Build and Verification

```sh
cargo test -p locho-core --no-default-features
cargo test -p locho-core --features transport
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features -- --test-threads=1
```

Default core features do not include iroh or native host dependencies. Async helpers
still use Tokio I/O/time; callers own runtime/clock integration. Cross-target
compilation and browser runtime execution are separate evidence. Root CLI builds
require neither Flutter nor WASM tools. Native regression and release gates are
documented in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Remaining Integration Gates

1. Browser `wasm32-unknown-unknown` crypto/runtime/discovery/relay proof against a
   real host; record custom relay authentication and stale peer-hint behavior.
2. Native owned-session/loopback adapter with typed status, cancellation and bounded
   cleanup, followed by FRB runtime and physical iOS/Android attach/GET/close proof.
3. Freeze ticket addressing schema after the connectivity proof; implement parser,
   shared vectors and explicit CLI ticket output.
4. Deliver distributable Dart/npm packages and exercise external consuming apps
   against one versioned health/login/notes backend.

The core API is an internal prerelease boundary and may evolve during these gates.
No wire version change or new host configuration is introduced by this extraction.

# Controlled browser service-setup investigation

This is an isolated developer harness, **not release acceptance evidence** and not
a replacement for public-N0 tests. It does not contain a transport fix. Both failed
mitigations in `docs/browser-first-service-investigation.md` remain excluded.

## Boundaries

- Native fixture: iroh-relay **1.4.0**, plain HTTP/WebSocket bound only to
  `127.0.0.1` on an ephemeral port. QUIC remains end-to-end encrypted. Relay TLS,
  WAN conditions, signed Pkarr verification and real DNS are intentionally absent
  from this control.
- The native fixture directly includes the production host/config/state/auth source
  modules. Its only endpoint-construction substitute uses `presets::Minimal`, one
  loopback relay, no public publisher/discovery, and no IP transports. Relay server
  dependencies/features are isolated in this standalone Cargo workspace. The lock
  is seeded from the workspace lock; the runner checks matching transport versions.
- A `browser-repro` feature adds private WASM-only investigation entry points and
  an explicit trace initializer. Normal SDK builds do not export them. Controlled
  clients use the same connect/session/service-authorization path with a Minimal
  endpoint and a deterministic local address lookup. Explicit/stale hints and
  0/750ms lookup delays are tested with two concurrent clients per case.
- The browser wrapper and private bindings must use the **same WASM instance**.
  The Vite harness alias enforces that. Mixing handles between separate modules is
  invalid. Consumers must never import these private bindings.
- N0 mode uses the normal browser connect path and an actual CLI host. Its delayed
  lookup intercept targets only the peer's discovery record, not client publication.
- Original SDK deadlines and one-request/no-replay assertions remain. The launcher
  allows 65 seconds to report existing sequential startup/request/cleanup budgets;
  this is a diagnostic reporting window, not a longer SDK request deadline.

## Build

From the repository root:

```sh
CARGO_TARGET_DIR="$PWD/target/browser-repro" \
  cargo build --locked --manifest-path tests/browser-repro/Cargo.toml
cargo build --locked --bin locho
```

From `packages/locho-web`:

```sh
node scripts/build-repro.mjs
node --test scripts/release-guards.test.mjs
node --test scripts/repro-runner.test.mjs
```

On this macOS machine, use Homebrew LLVM for `ring`'s WASM C compilation:

```sh
CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang \
AR_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/llvm-ar \
  node scripts/build-repro.mjs
```

Build products use separate `target/browser-repro-wasm` and `dist/repro` directories;
the normal generated/package WASM files are not overwritten. Package building
rejects investigation exports as an additional publication guard.

## Runs

```sh
# Healthy controls, then reviewed transport tracing. Run comparisons serially.
node scripts/reproduce-service-setup.mjs --mode controlled --browser all --cycles 10 --trace none
node scripts/reproduce-service-setup.mjs --mode controlled --browser all --cycles 10 --trace milestones

# Four serial matched N0 configurations, maximum 30 cases each, no failed-case
# retries. Rebuild the instrumented SDK first; stale source/artifacts are rejected.
LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" node scripts/compare-service-setup.mjs

# Deterministic TCP-chunk delay (not a precise packet-loss/RTT simulator), and
# separate host/browser home relays to exercise registration/path timing.
node scripts/reproduce-service-setup.mjs --mode controlled --browser chromium --cycles 10 \
  --relay-layout split --relay-delay-ms 0 --home-delay-ms 250 --trace milestones

# Positive fault control: two TCP sockets open; authorization replies deliberately
# delayed beyond the unchanged 15-second service budget. NOT a healthy run or bug fix.
node scripts/reproduce-service-setup.mjs --mode controlled --browser chromium --cycles 1 \
  --trace milestones --fault-delay-ms 20000

LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" \
  node scripts/reproduce-service-setup.mjs --mode n0 --browser chromium --cycles 10 --trace transport

# Exact previously packed candidate, no new browser instrumentation. The runner
# verifies its retained pack-manifest integrity and clean build provenance.
LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" \
  node scripts/reproduce-service-setup.mjs --mode n0 --sdk package --browser chromium --cycles 10 --trace milestones
```

The fault delay is a capped `integration-test` host hook, disabled in normal release
builds. The fixture package enables that feature; ordinary runs explicitly set its
delay to zero. An expected-fault result never sets `healthy:true` and is not counted
as a successful health/session test.

`--sdk package` is N0-only and serves the retained
`packages/locho-web/dist/artifacts/locho_web-0.1.0-beta.3.tgz` unchanged. Browser
tracing is unavailable for that baseline; `--trace` controls host tracing only.
The instrumented SDK is not byte-identical to this artifact. Keep that distinction
in comparisons. Both reuse exactly the same JS orchestration.

`--host-trace` independently controls host diagnostics (defaults to `--trace`).
The fixed comparison keeps host milestones enabled when browser tracing is off.
`dist/repro/build.json` records source and generated/bundled file hashes; the
launcher rejects stale builds and checks host/SDK/runner inputs again at cleanup.
An inputs-changed result is not usable comparison evidence. Hashes freeze dirty
investigation inputs; they do not turn them into clean release provenance.

After a run completes, generate allowlisted metadata for review without printing
raw traces:

```sh
node scripts/analyze-service-setup.mjs /path/to/private/run-directory
```

This writes a private `analysis.json` with known milestones, stream offsets/lengths,
packet numbers and pseudonymous endpoint correlation. It never copies raw log text
or payloads. Unknown/missing events remain unknown, not evidence of packet absence.
ACK events are counted, not interpreted as proof the authorization bytes were ACKed.

`--host-trace frames` is a low-volume host-only filter: packet-builder outbound
frame metadata plus Locho milestones/driver spans. It omits inbound packet/ACK
events, so a missing acknowledgment in this mode is unknown, not packet loss.

### Buffered native trace capture

```sh
# From repository root (standalone workspace; no production binary changes):
CARGO_TARGET_DIR="$PWD/target/browser-repro" \
  cargo build --locked --manifest-path tests/browser-repro/Cargo.toml --bin n0-trace

# From packages/locho-web:
LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" \
  node scripts/reproduce-service-setup.mjs --mode n0 --browser chromium --cycles 10 \
  --trace milestones --host-trace transport --host-buffered 1
```

This Unix-only disposable binary directly includes the production N0 endpoint
builder, host/config/auth/state modules. The binary, CLI wrapper and standalone
dependency-feature graph are **not identical to the release CLI**; compare results
accordingly. `LOCHO_HOST_BINARY` remains the real CLI used for read-only diagnosis;
its version is separate from the buffered host's recorded binary/source hashes.

Its tracing sink buffers formatted records in memory (8 MiB total; 8 KiB per record),
with explicit eviction/truncation counters. There is no per-event pipe/file I/O.
After each case has settled, the launcher requests a dump via SIGUSR1 and appends
the snapshot to its private trace. Signal/dump handling does not extend SDK request
deadlines. The private intermediate dump is mode 0600 and deleted during cleanup.
Known-credential scanning applies to the collected trace. An overflow invalidates
complete-capture claims. Formatting and memory allocation can still perturb timing;
this is a lower-I/O observer, not a zero-overhead recorder or a transport fix.

The private WASM feature also exposes `repro_stats()`: allowlisted numeric QUIC
counters, inspected only AFTER a failed operation and before client close. The
normal SDK does not export this method. Zero received STREAM frames rules out an
application read-wake problem for that capture; nonzero counts alone cannot prove
complete authorization delivery. Aggregate loss counts do not identify which
stream bytes were lost, and cancellation counters can include post-timeout cleanup.

With `--host-buffered 1`, a failed browser operation invokes a private harness
callback before close. It requests a signal snapshot of the host's still-active
connections, so corresponding host counters can be reviewed without per-packet
logging. The registry holds connection clones only during their existing host
handler lifetime and removes entries on handler exit. It is compiled under
`integration-test`, disabled unless the disposable binary explicitly enables it,
and never enabled by the normal CLI. Callback/dump waiting happens after the SDK
operation has failed; it does not increase the request deadline. A separate harness
test verifies that host inspection completes before closing the failed client.

### Actual CLI, test-only post-failure snapshots

To avoid the standalone buffered binary as a comparison variable, build the real
CLI with its existing `integration-test` feature and use `--host-snapshots 1`:

```sh
# Repository root; preserve any baseline binary before replacing this build.
cargo build --locked --bin locho --features integration-test

# packages/locho-web:
LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" \
  node scripts/reproduce-service-setup.mjs --mode n0 --browser chromium --cycles 10 \
  --trace none --host-trace milestones --host-snapshots 1
```

The launcher sets a private `LOCHO_TEST_CONNECTION_SNAPSHOT_FILE` only for the
disposable host child. The integration-only hook creates it with mode 0600 and
registers a SIGUSR1 counter dump handler; logging is not replaced. A release build
does not compile the hook. An integration build without that variable leaves it
disabled. The runner checks that the hook created its file before sending signals,
rejects unsupported binaries, hashes the observer sources/binary, and deletes the
snapshot file during cleanup. This option is mutually exclusive with
`--host-buffered 1`. Successful requests do not collect browser counters; snapshots
requested after settled cases cannot retroactively establish delivery for a
failed request. Only a pre-close failure callback yields a paired failure snapshot.

`--scenario` can select `explicit`, `stale`, or `stale_delayed` (default `all`).
Each `--cycles` repetition runs the selected scenario(s); e.g. `--scenario
stale_delayed --cycles 30` has a maximum of 30 cases, while default `all --cycles 10`
also has a maximum of 30. All cases still perform two independent clients and stop
at the first failure; this selection is diagnostic, not release acceptance.

### Relay-alias investigation (unproven hypothesis)

`--relay-aliases 1` is controlled/same-relay only. Host registration uses loopback;
browser home/peer URLs use `localhost.` / `localhost` for the same relay. Counts of
dotted/plain WebSocket URL spellings are retained under hashed group tags, never
the URL text. `--canonical-home 1` privately normalizes the home alias for a paired
control. These private entry points are absent from ordinary SDK builds.

Additional loopback-only controls, not release assertions:

- `--auth-reply-delay-ms 1000`: delay host authorization by 1s, capped at 5s and
  still inside the unchanged 15-second SDK budget. Unlike the 20s expected-fault
  control, a timeout here remains a failing case.
- `--alias-first-delay-ms 1000`: delay the first two browser HTTP upgrades per
  case equally regardless of URL spelling, not native registration.
- `--alias-release-on-upstream 1`: hold dotted-alias upgrades until two upstream
  TCP accepts, then release before the delayed reply. Requires a bounded reply
  delay; mutually exclusive with first-upgrade delay. Partial HTTP headers are
  buffered only in memory to classify disposable loopback upgrades; no header text
  is logged. Close/8s fallback releases are recorded and do not prove the intended
  ordering was exercised.

`--canonical-n0-home 1` is an investigation-only N0 candidate that normalizes the
default client relay policy to match normalized peer hints/discovery. It preserves
the default/staging server selection and relay config, without a readiness wait,
pre-resolution or application replay. The ordinary SDK connect path remains
unchanged until this candidate is verified. It requires an instrumented private
build and is rejected for exact-package baselines. A green diagnostic sample alone
must not be presented as a verified transport fix.

The N0 candidate now uses a synchronous private configuration bit and the public
`LochoClient.connect()` JS path in both variants. The previous raw-constructor
experiment had a cached-initialization-await difference and is retained only as
historical coverage. To run the stricter fixed-budget diagnostic comparison:

```sh
LOCHO_HOST_BINARY="$PWD/../../target/debug/locho" node scripts/compare-relay-policy.mjs
```

It checks matching host/WASM/bundle/runner/browser/peer-relay inputs and requires a
matching baseline failure plus a complete canonical sample to call the contrast
valid. Exit 1 can mean **inconclusive** (both samples passed), not necessarily a
candidate failure. Both variants use exactly the same JS connection path. Do not
repeat an inconclusive comparison until it produces a desired result or interpret
it as publication approval.

Controlled relays are reached through loopback TCP proxies. `--relay-delay-ms` and
`--home-delay-ms` delay each TCP chunk in each direction, preserving stream order
and backpressure without recording its contents. Home delay requires the `split`
relay layout; delays are capped at 1000ms. This can amplify scheduling/path
differences, but is not proof of a WAN packet-loss cause. N0 mode rejects these
local shaping options. No public relay is fault-injected.

## Evidence and privacy

Every run owns its relay, state, upstream and file server; ports are ephemeral.
Private evidence lives in a new mode-0700 temporary directory, trace/summary files
are mode 0600, and disposable host keys/state are removed during cleanup. No raw
traces or fixture credentials are uploaded automatically. No screenshots/videos or
Playwright traces are recorded. Never use real tickets or application credentials.

- Debug milestones include protocol-header prefix/byte reads, authorization request
  write/reply decode, and host authorization-reply write boundaries. They log fixed
  stages, lengths/statuses and process-local stream/connection correlation, not
  capability proofs or HTTP headers/body content.
- Browser and host transport logs are opt-in and retained privately. The browser
  registration snapshot is passive and feature-gated; it does not await readiness.
- `summary.json` contains per-client timelines, success/failure codes, exactly-once
  HTTP counts, upstream TCP counts, socket cleanup, selected dependency versions,
  source/lock provenance, and allowlisted stage totals. Browser trace timestamps
  are Node receipt times, not exact on-wire timestamps.
- Transport mode additionally enables `noq_proto::connection=trace` for QUIC frame
  IDs/offsets/lengths, ACKs and retransmission decisions. Reviewed upstream STREAM
  frame display logs lengths, not payload bytes. This is distinct from the earlier
  debug-only transport runs; record the source/build used when comparing them.
- Raw files are bounded at 16 MiB each; dropped-byte counts mean incomplete trace
  evidence. The known disposable secrets/proofs/private-key representations are
  scanned, and any matching trace file is deleted and the run marked failed. If
  scanner inputs are unavailable, raw logs are deleted rather than retained.
  This scan is a safety net, **not proof of complete redaction**: manually review
  raw transport logs before sharing even when the scan passes.
- Tracing can perturb scheduling. A passing traced run does not establish absence
  of the bug. Use matching untraced/milestone/transport controls, multiple repetitions,
  and explicitly distinguish injected failures from naturally occurring ones.

First healthy/fault-control validation ran concurrent harness processes to verify
isolation, not to draw network/CPU causality conclusions. Subsequent comparisons
must be serial with the same case counts and dependency pins.

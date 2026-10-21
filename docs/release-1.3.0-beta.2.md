# 1.3.0-beta.2 release evidence

## Scope and compatibility

This is a limited CLI integration beta, not the stable channel. Stable remains
`1.2.0`. The candidate adds macOS/Linux Unix stream forwarding, shared-peer HTTP
failure isolation, cancellation/lifecycle hardening, bounded browser discovery
retries and iroh 1.4.0. Native raw streams default to a 300-second application-byte
idle deadline; `tcp_idle_timeout_secs = 0` disables expiry.

| Component | Pin / evidence boundary |
| --- | --- |
| CLI candidate | `1.3.0-beta.2`; final committed candidate verification pending |
| Preparation base | `8e1dcc3c8bd97496a9e13dfccfcea97c86c0aeee` (merged PR #118) |
| Transport source builds | iroh `=1.4.0` across CLI, shared/native/browser crates and Flutter Rust integration |
| Wire / tickets / state | `locho/3`, ticket v1, host-state schema 2; no migration required |
| Published browser SDK | npm `locho_web` `0.1.0-beta.2` |
| Published Flutter SDK | pub.dev `locho_flutter` `0.1.0-beta.2`; FRB 2.12.0 |

SDK versions are independent. This CLI release does not republish the immutable
registry packages or apply current source changes to their bundled implementations.
Package provenance and registry checksums remain in [SDK releases](sdk-releases.md).

## Existing evidence — preparation base, not the final candidate

The merged preparation base has green hosted workflows:

- [Native CI](https://github.com/trchopan/locho/actions/runs/39601167481):
  Linux/macOS/Windows contributor gates, Rust 1.96, dependency policy, native
  packaged archives/checksums/installers and release smoke.
- [Browser](https://github.com/trchopan/locho/actions/runs/39601167473):
  source WASM/package builds and browser/external-consumer acceptance.
- [Flutter](https://github.com/trchopan/locho/actions/runs/39601167282):
  Rust/Dart checks, Android builds/alignment/package checks and iOS builds.

These runs precede the CLI version/notes changes. They do not replace final
candidate verification. Stress and soak jobs are not run on ordinary PR/push CI.
Earlier local native stress/soak results used an older dirty binary and are not
final-candidate evidence.

## Local preparation checks — uncommitted tree

These checks exercise the release-preparation tree on macOS ARM64, not a clean
committed release candidate:

- `cargo metadata --locked --no-deps --format-version 1`: CLI reports
  `1.3.0-beta.2`; shared crate versions remain `0.1.0`.
- `dist plan --tag v1.3.0-beta.2 --output-format=json`: selects the beta.2
  changelog, marks the announcement prerelease and plans all five supported
  archives plus checksums and installers; `force_latest` is false.
- Python performance-harness unit tests: 18 Unix-stream tests and eight loadgen
  tests passed.
- `cargo deny check -W unmaintained`: advisories, bans, licenses and sources
  passed; existing duplicate-version and unmaintained `paste` warnings remain.
- `cargo fmt --all -- --check` and locked workspace/all-target/all-feature
  Clippy with warnings denied passed.
- Locked serial workspace/all-target/all-feature tests: 174 passed; four
  separately executed release smoke tests were intentionally ignored here.
- Locked core tests without default features: 29 passed.
- `cargo build --locked --release` passed.
- `LOCHO_TEST_BINARY=target/release/locho cargo test --locked --test release_smoke
  -- --ignored`: all four passed, covering HTTP/TCP/rotation, active-session
  shutdown, the 90-second HTTP timeout and native Unix stream forwarding. Unix
  smoke artifacts: `artifacts/unix-smoke-1792588225934437000/` (local, not committed).

The local binary reports the preparation base commit with dirty provenance. These
results do not satisfy the clean-checkout archive or final-candidate gates below.

## Known browser stale-addressing intermittency

[PR #118](https://github.com/trchopan/locho/pull/118) records 540 interleaved
comparison cases across iroh 1.2.0, 1.3.0 and 1.4.0: 527 passed, eight Locho
timeouts and five discovery-precheck failures. All 135 explicit-relay controls
passed. The comparison used unchanged application sources, no test retries and
no longer deadlines. It does not establish a version-specific reliability ranking;
iroh 1.4.0 is not a demonstrated fix for intermittent public-N0 stale-hint timeouts.
The failing path remains covered. Application writes are never automatically
replayed to mask a transport failure.

## Final candidate gates — pending

Record the exact committed candidate SHA and results in the release PR. A later
merge commit must pass its own final gate before tagging.

### Verification progress and discovered capacity defect

Candidate `61c8f5a79a74dae4cf8a2b61d60f32f424195e64` passed the complete clean
macOS ARM64 release gate, including cargo-dist archive checks and packaged smoke,
plus executable-only upgrade from checksum-verified published stable `1.2.0`.
Exact results are recorded in [release PR #119](https://github.com/trchopan/locho/pull/119).

The earlier candidate's [native load run](https://github.com/trchopan/locho/actions/runs/39608843380)
passed both 15-minute soaks, but failed both Linux/macOS stress profiles at 32
workers per each of four combinations. Attachment logs showed service handshake
timeouts; the host inherited a 100-bidirectional-stream QUIC window while the
stress profile requested 128 simultaneous streams on one peer.

The host now explicitly admits its existing 128-connection TCP budget at the QUIC
layer. The focused `one_peer_supports_the_full_tcp_connection_budget` regression
fails at stream 101 with the old binary and passes all 128 with this change. The
stress profile, timeouts and failure gates are unchanged. Full clean-candidate
verification and hosted load acceptance must be repeated for the fix; earlier
green results do not verify these later changes.

- [ ] Clean committed candidate with consistent beta.2 metadata and release notes.
- [ ] `scripts/verify-release.sh` on Linux or macOS, including cargo-dist archive smoke.
- [ ] `scripts/Verify-Release.ps1` on Windows.
- [ ] Browser and Flutter workflows against the candidate; dispatch manually if
  metadata-only changes do not match workflow path filters.
- [ ] Native Unix stress (5 minutes) and soak (15 minutes) on Linux and macOS,
  recording exact binary commit/hash and harness results.
- [ ] Executable-only upgrade from checksum-verified stable `1.2.0` to the packaged
  candidate, preserving host identity/capabilities and checking scoped rotation.
- [ ] Review logs/diagnostics for leaked capabilities and private keys.

```sh
scripts/verify-release.sh
python3 tests/perf/unix_streams.py stress --binary target/release/locho --duration 5m
python3 tests/perf/unix_streams.py soak --binary target/release/locho --duration 15m
python3 scripts/verify-upgrade.py \
  --previous-binary /path/to/verified-v1.2.0/locho \
  --candidate-binary /path/to/verified-packaged-beta.2/locho
```

Python 3.11+ and OpenSSL are required for the upgrade rehearsal. Use disposable
state, not a real host's configuration. See [release checklist](../RELEASE.md).

## Target and product boundaries

- Linux x86_64, macOS Apple silicon and Windows x86_64 have native hosted CI.
  Linux ARM64 and macOS x86_64 cross-builds are not native runtime/install
  acceptance; additional native rehearsal remains unrecorded for this candidate.
- Unix listeners/upstreams are macOS/Linux only. Windows rejects Unix options;
  existing TCP forwarding remains supported. Windows ARM64 is not distributed.
- Unix filesystem hardening does not defend against root or same-UID interference.
- Physical iOS, broad physical Android/mobile-browser acceptance, real network
  transitions, actual BFCache and controlled-relay acceptance remain open.
  Flutter is foreground-only; hosted builds are not physical-device runtime proof.
- Browser HTTP remains a bounded subset, without managed WebSockets or authenticated
  browser relays. CLI HTTP WebSocket upgrades remain unsupported.

## Tag and published-artifact acceptance — pending

Do not tag until the final candidate gates are recorded. After publishing:

- [ ] `v1.3.0-beta.2` identifies the verified commit and GitHub marks it prerelease.
- [ ] Stable GitHub `latest` and latest-release installers still select `v1.2.0`.
- [ ] All five supported archives, checksums and shell/PowerShell installers exist;
  internal cargo-dist manifests are absent from public release assets.
- [ ] Downloaded assets pass checksums, archive-content, `--help`, `--version`,
  clean commit provenance, installer and HTTP/TCP quickstart checks.
- [ ] Record target-specific verification limitations in the published notes.

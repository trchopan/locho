# 1.3.0-beta.1 release evidence

## Scope and compatibility

This is a limited CLI/SDK integration beta. The stable channel remains `1.2.0`.
CLI tickets, shared native ownership, browser JSON/streaming HTTP, foreground
Flutter attachments and the shared notes examples are included. Full physical
mobile/network acceptance is not a release claim.

| Component | Pin / evidence boundary |
| --- | --- |
| CLI candidate | `1.3.0-beta.1`; exact tested commits/results recorded in the release PR and CI |
| Wire / tickets / state | `locho/3`, ticket v1, existing host-state schema 2; no migration required |
| Browser SDK | npm `locho_web` `0.1.0-beta.1` |
| Flutter SDK | pub.dev `locho_flutter` `0.1.0-beta.1`; bundled Rust sources, FRB 2.12.0 |
| Shared API | Notes API v1, OpenAPI 1.1.0 |
| Examples baseline | `efa44bbd1528342f8633323b93f274984051216c`; registry beta acceptance |
| Rust / browser tools | Rust 1.96+, Node 24.18.0, Playwright 1.64.1, wasm-bindgen 0.2.126 |
| Flutter tools | Flutter 3.44.5 / Dart 3.12.2, Android NDK 28.2 |

Registry checksums and the already-published SDKs' original dirty-source provenance
are preserved in [SDK releases](sdk-releases.md). A CLI version bump does not change
or republish those immutable packages.

## Candidate verification

Run from a clean committed candidate; record the full commit and command results
in the release PR. A later merge commit must pass its own final gate before tagging.

```sh
scripts/verify-release.sh
python3 scripts/verify-upgrade.py \
  --previous-binary /path/to/verified-v1.2.0/locho \
  --candidate-binary /path/to/verified-candidate/locho
```

The upgrade rehearsal uses disposable state, a local TLS HTTP upstream and TCP
echo server. It verifies both client versions against both host versions, unchanged
identity/capabilities after executable replacement, and service-scoped rotation.
It never resets a real host or prints generated credentials. Verify the downloaded
`1.2.0` archive/checksum before supplying the previous binary. Python 3.11+ and
OpenSSL are required. Native tests remain required separately.

The native CI covers Linux/macOS/Windows contributor gates, MSRV and native packaged
artifact/install smoke checks. Browser and Flutter workflow path filters may not
match a CLI-version-only change: dispatch `browser.yml` and `flutter.yml` against
the candidate branch when necessary. SDK compilation/runtime evidence and CLI
archive evidence must identify their own commits.

### Target boundaries

- Linux x86_64, macOS Apple silicon and Windows x86_64 have native CI gates.
- Linux ARM64 and macOS x86_64 distribution builds are not by themselves native
  runtime/install acceptance. Record any additional native rehearsal separately;
  Linux ARM64 native verification remains pending.
- Android ARM64/x86_64 debug/release source builds and 16 KiB checks exist; Android
  emulator runtime and earlier physical Android feasibility are separate evidence.
- iOS simulator and unsigned-device compilation pass in SDK/examples CI; physical
  iOS runtime remains unaccepted.

## Final examples handoff — pending release PR merge

After the release-preparation PR merges, pin its final full Locho SHA in examples
`.github/workflows/check.yml`, `deploy/Dockerfile.locho` and compatibility/deployment
documentation. Keep both registry SDK versions pinned at `0.1.0-beta.1`.

Run API/contract checks, real CLI JSON/SSE smoke, the three-browser production
matrix, private Compose/HTTPS acceptance and existing Android runtime modes against
that host. Record the examples commit, candidate host version/SHA, SDK integrity,
platform/tool versions and results together. Existing baseline acceptance is not
evidence for an untested final host commit.

## Tag and published-artifact acceptance — pending

After final candidate gates and examples acceptance pass:

1. Tag that exact clean Locho commit `v1.3.0-beta.1` and push the tag.
2. Confirm the release workflow publishes all five configured target archives,
   matching checksums and shell/PowerShell installers, marks the release prerelease,
   and keeps `1.2.0` as latest stable. Internal dist manifests are not public assets.
3. Download and verify the published artifacts, commit provenance, installers,
   `--help`, `--version`, HTTP/TCP quickstarts and SDK ticket/notes journey. Execute
   only native artifacts for the available platform; record other native checks or
   their pending status explicitly.
4. Add final release/CI URLs and post-publication results to the release record.

Use the version-specific beta installer URLs in the [README](../README.md#testing-130-beta1);
`/releases/latest/` follows stable releases.

## Known limitations

- Physical iOS, complete physical Android/mobile-browser journeys, real network
  transitions, selected relay paths and storage lock/backup/uninstall behavior
  remain open. Flutter is foreground-only and requires native source-build tools.
- Synthetic page events/new page owners do not prove actual BFCache entry or full
  browser-process restart persistence.
- Public N0 relay/discovery tests are network-dependent. Controlled-relay coverage,
  authenticated browser relays and live migration remain open. Historical browser
  health/restore timeouts remain unexplained despite later green runs; see the
  [examples acceptance](https://github.com/trchopan/locho_sdk_examples/blob/efa44bbd1528342f8633323b93f274984051216c/docs/registry-acceptance.md).
- Browser HTTP is a bounded documented subset; there is no cookie jar, redirect
  following, browser-managed homelab Fetch, or managed WebSocket API. CLI HTTP
  upgrades remain unsupported; ordinary native TCP forwarding is unchanged.
- QR, background mobile execution and full physical/mobile SSE acceptance are
  later work. Reconnection never implies automatic replay of application writes.

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

## Final examples handoff — complete

Examples PR [#12](https://github.com/trchopan/locho_sdk_examples/pull/12) merged as
`57f95ff94835de5525e77e182d9921245dba773b`. It pins the final Locho merge
`3786da3127338b2530e0cd8cea8ef23bc6d55dfd` in
`.github/workflows/check.yml` and `deploy/Dockerfile.locho`, with compatibility and
deployment records updated. Both registry SDK versions remain pinned at
`0.1.0-beta.1`.

The merged PR checks passed for browser, private Compose/HTTPS deployment and
Flutter Ubuntu/macOS builds. Local candidate rerun also passed API/contract checks,
real CLI JSON/SSE smoke, private Compose/HTTPS acceptance, the focused browser
reruns and Flutter static/unit/Android debug-release builds. No Android emulator was
attached in the final local rerun; earlier emulator and physical-device evidence is
recorded separately in the examples repository.

## Tag and published-artifact acceptance — complete

The final clean candidate was tagged and published:

1. Tag `v1.3.0-beta.1` points to `3786da3127338b2530e0cd8cea8ef23bc6d55dfd`.
2. The [release workflow](https://github.com/trchopan/locho/actions/runs/39038125597)
   passed and published Linux x86_64/ARM64, macOS x86_64/Apple silicon and Windows
   x86_64 archives, checksums, shell/PowerShell installers and source archives.
   Internal dist metadata was removed from the public release assets.
3. [GitHub release](https://github.com/trchopan/locho/releases/tag/v1.3.0-beta.1)
   is marked prerelease. GitHub's stable `latest` API still resolves to `v1.2.0`.
4. Downloaded release assets passed `sha256.sum`; the downloaded macOS ARM64 archive
   passed archive-content, `--help`, `--version` and clean commit-provenance checks.
   The downloaded shell installer was exercised against a local asset server and
   installed the same verified binary. PowerShell execution was not available on the
   development Mac; Windows archive/installer smoke passed in the release workflow.
5. The candidate binary passed the real examples notes API/JSON/SSE smoke and the
   examples' candidate-pinned browser/Compose/HTTPS checks before publication.

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

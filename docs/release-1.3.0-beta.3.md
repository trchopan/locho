# Coordinated beta.3 release candidate

## Products, scope and source

| Product | Candidate | Publication boundary |
| --- | --- | --- |
| Locho CLI | `1.3.0-beta.3` | GitHub prerelease tag `v1.3.0-beta.3` |
| Browser SDK | `locho_web` `0.1.0-beta.3` | Exact tested npm tarball, `beta` dist-tag |
| Flutter SDK | `locho_flutter` `0.1.0-beta.3` | Verified standalone pub.dev staging |

These versions are independent. No product is published merely by merging this
preparation PR. Stable CLI/GitHub latest remains `1.2.0`; npm `latest` remains
`0.1.0-beta.1` and its pre-publication `beta` tag remains beta.2. Do not create SDK
version tags: the existing version-like tag workflow drives CLI releases.

Preparation base: merged browser policy repair PR #124, main
`e6217657b5da3461e5570a96a72991f281ead69b`. SDK version preparation was PR #122,
`c0334940194a01020cfdaaa410280e27f7bbe125`. **Final merged source SHA: pending this
coordinated preparation PR's merge.** Build all publication artifacts afresh from
that exact clean SHA, not from these bases or older candidate archives.

The CLI's runtime/native source is unchanged since CLI beta.2; its resolved ctutils
dependency changes from 0.4.2 to 0.4.4 for RUSTSEC-2026-0385. The independently
bundled SDK changes are described in [SDK beta.3](sdk-0.1.0-beta.3.md): browser
upstream-header metadata and client/peer relay-key consistency; refreshed Flutter
core/native sources. Browser metadata methods are not CLI/Flutter APIs. A CLI
upgrade does not update an installed SDK package.

iroh remains 1.4.0, FRB runtime/codegen 2.12.0, shared Rust crate versions 0.1.0,
wire protocol 3, ticket format 1 and host-state schema 2. There is no state or
capability migration; upgrades replace only the CLI executable.

## Supporting evidence, not final release acceptance

PR #124's branch candidate `da9060d05dc8a79b69ce92fbdb28cdeb5c4c150d` passed a
deterministic relay-policy regression (red before repair), clean workspace gates,
ordinary source browser tests (96 passed / 3 external-only skips), exact packed
consumer tests (99 passed), and six concurrent 900-second page/module-worker soaks
(3,555 cycles, all completed and cleaned up). Its hosted desktop, browser, Android
and iOS checks passed. These checks precede the coordinated version/notes changes
and are not evidence for the final merged commit or a later registry archive.

The relay-key correction fixes a proven configuration mismatch. The stricter
same-public-JS-path A/B control passed both baseline and canonical samples and is
inconclusive; local alias controls did not reproduce the natural timeout. Do not
claim every public-N0 stale-hint timeout is solved. Requests are never replayed,
deadlines/assertions are unchanged, and any final-gate failure blocks publishing.

## Candidate and final-merged gates

Record exact SHAs, binary/package hashes and workflow links in the release PR.
Preparation-branch results must be distinguished from final merged-SHA results.

- [ ] CLI Cargo/lock/changelog metadata and SDK versions are consistent.
- [ ] `dist plan --tag v1.3.0-beta.3 --output-format=json` selects a prerelease,
      all five supported archives/installers, and does not force latest.
- [ ] Security/license/source checks and clean workspace CI verification pass.
- [ ] Full `scripts/verify-release.sh` on Linux/macOS and
      `scripts/Verify-Release.ps1` on Windows, including cargo-dist archive smoke.
- [ ] Linux/macOS native Unix stress (5 minutes) and soak (15 minutes), all four
      TCP/Unix combinations, with exact candidate binary provenance.
- [ ] Executable-only upgrade from checksum-verified stable `1.2.0` and current
      CLI beta.2 preserves identity/capabilities and verifies scoped rotation.
      The verifier defaults to stable; the beta.2 run must explicitly pass
      `--previous-version 1.3.0-beta.2` and retain exact clean-provenance checks.
- [ ] Fresh browser WASM/TS/package/production builds; full ordinary source and
      installed-tarball tests on all engines with the pinned reference backend.
- [ ] Fresh 900-second page/module-worker browser soak on all three engines.
- [ ] Flutter formatting/analyze/tests, Rust bridge/Clippy and codegen reproducibility.
- [ ] Fresh external Flutter Android debug/release and 16 KiB alignment; hosted
      iOS simulator/unsigned-release consumers. Record runtime/device evidence
      separately from compilation and report unavailable device checks honestly.
- [ ] Flutter staging outside Git and publish dry run; only the documented exact
      FRB dependency warning may use the existing ignore-warnings procedure.
- [ ] Candidate PR is green and merged; freeze and revalidate the final clean SHA.
- [ ] Review disposable diagnostics for credentials; do not upload raw traces.
- [ ] npm/pub.dev publisher authentication and maintainer approval are available.

## Publish and verify, in order

- [ ] Tag only the verified final source as `v1.3.0-beta.3`; GitHub marks prerelease
      and stable/latest installers still select `1.2.0`.
- [ ] Download CLI archives/checksums/installers; verify contents, provenance,
      help/version, HTTP/TCP/Unix quickstarts and executable-only upgrades.
- [ ] Publish the exact verified browser tarball under npm `beta`, leaving
      `latest` unchanged; complete staged approval if required.
- [ ] Verify npm registry integrity and test actual beta.3 in a fresh Vite consumer.
- [ ] Publish only the verified Flutter staging directory; verify pub.dev checksum
      and bundled `BUILD.json`, then test fresh registry Android/iOS consumers.
- [ ] Record final source SHA, registry integrity, acceptance results and remaining
      limitations; update SDK examples only after publication is confirmed.

Do not describe locally packed/staged packages as publicly installable. If one
publication succeeds and a later gate fails, record the partial state and stop;
immutable published versions cannot be silently replaced with different bytes.

## Beta limitations and supported targets

Five archives remain planned: Linux x86_64/ARM64, macOS x86_64/Apple silicon and
Windows x86_64. Linux ARM64 cross-builds are not native runtime proof; record other
cross-only target limitations explicitly. Windows ARM64 is not distributed; Unix
listeners/upstreams remain macOS/Linux-only. CLI HTTP WebSocket upgrades, browser
managed WebSockets/authenticated relays, physical iOS, broad physical-device/
mobile-browser coverage and real network transitions remain outside the verified
scope. Flutter remains foreground-only. No new stable-channel promotion or broader
compatibility claim is implied by this prerelease.

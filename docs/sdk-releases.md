# SDK and Prerelease Provenance

This document records the installation commands, registry checksums, source
provenance, compatibility boundaries, and verification evidence for published
`locho_web` (npm), `locho_flutter` (pub.dev), and coordinated `locho` CLI
prereleases.

SDK package versions are independent of the `locho` CLI version. A CLI upgrade
replaces only the CLI executable and does not automatically update installed SDK
packages. Do not create Git tags for SDK versions: the repository's version-tag
workflow drives CLI `cargo-dist` releases.

## Compatibility Summary

| Release Series | CLI Prerelease | Browser SDK (`npm`) | Flutter SDK (`pub.dev`) | Wire / Ticket / State | iroh / FRB Pins |
| --- | --- | --- | --- | --- | --- |
| **Beta 3** (2026-10-25) | `v1.3.0-beta.3` (`0c8ad57cc91af30144138bd324aeca86604f48dd`) | `locho_web@0.1.0-beta.3` | `locho_flutter: 0.1.0-beta.3` | `locho/3`, ticket `v1`, state schema `2` | `iroh =1.4.0`, `frb 2.12.0` |
| **Beta 2** (2026-10-17 / 21) | `v1.3.0-beta.2` (`8e1dcc3` / `61c8f5a`) | `locho_web@0.1.0-beta.2` (`e9d09b8`) | `locho_flutter: 0.1.0-beta.2` (`ebd6e10`) | `locho/3`, ticket `v1`, state schema `2` | `iroh =1.4.0` (CLI), `frb 2.12.0` |
| **Beta 1** (2026-10-17) | `v1.3.0-beta.1` (`3786da3127338b2530e0cd8cea8ef23bc6d55dfd`) | `locho_web@0.1.0-beta.1` (`cb1b232`, dirty) | `locho_flutter: 0.1.0-beta.1` (`cb1b232`, dirty) | `locho/3`, ticket `v1`, state schema `2` | `iroh =1.2.0`, `frb 2.12.0` |

Stable CLI (`latest` GitHub Release channel) remains `1.2.0`.

---

## 0.1.0-beta.3 & CLI 1.3.0-beta.3 — 2026-10-25

Coordinated from merged PRs #121, #122, and #124 at final clean source SHA
`0c8ad57cc91af30144138bd324aeca86604f48dd`:

- **Browser (`locho_web` `0.1.0-beta.3`)**: Adds opt-in upstream response
  metadata (`fetchWithMetadata` and `fetchStreamWithMetadata`) for
  application-managed cookie sessions, and aligns client/peer relay URL keys.
  The relay-key configuration mismatch has a deterministic regression test; its
  causal role in intermittent public-N0 stale-hint timeouts remains unproven.
- **Flutter (`locho_flutter` `0.1.0-beta.3`)**: Refreshes its bundled
  `locho-core` and `locho-client-native` sources while keeping its public Dart
  API and FRB `2.12.0` pins unchanged.
- **CLI (`v1.3.0-beta.3`)**: Updates `ctutils` from `0.4.2` to `0.4.4`
  (`RUSTSEC-2026-0385`), keeping runtime behavior, Unix stream forwarding, and
  `iroh =1.4.0` unchanged from `1.3.0-beta.2`.

npm's `beta` dist-tag points to `0.1.0-beta.3` while npm `latest` remains
`0.1.0-beta.1`. CLI `v1.3.0-beta.3` is marked as a GitHub prerelease so stable
installers continue to resolve to `v1.2.0`.

| Product | Registry / Channel | Install |
| --- | --- | --- |
| Browser SDK | [locho_web 0.1.0-beta.3](https://www.npmjs.com/package/locho_web/v/0.1.0-beta.3) | `npm install --save-exact locho_web@0.1.0-beta.3` |
| Flutter SDK | [locho_flutter 0.1.0-beta.3](https://pub.dev/packages/locho_flutter/versions/0.1.0-beta.3) | `locho_flutter: 0.1.0-beta.3` |
| CLI (Unix) | [GitHub `v1.3.0-beta.3`](https://github.com/trchopan/locho/releases/tag/v1.3.0-beta.3) | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/trchopan/locho/releases/download/v1.3.0-beta.3/locho-installer.sh \| sh` |

### Registry Integrity and Provenance

- **Final merged source SHA**: `0c8ad57cc91af30144138bd324aeca86604f48dd` (`dirty: false`, wire protocol `3`, ticket format `1`)
- **npm SRI**: `sha512-D1GzJUBKHwT07/ECa3cqFS3nKxoDss+YHeXYtaYYBV0clpdrM3oDNKzxTjG+TaHv2PlvANDAbYkpg9FrVuOsWg==`
- **npm SHA-256**: `1dcbee155e6c59b8561c3fda3b3e20a15123ef9cb1808fb566ed6ceeb537bc65`
- **pub.dev archive SHA-256**: `fcdad92b61204ed6ef3ead00fb5f46ea780a7263ee3f4465226d5ecc05faa735`

### Verification and Acceptance

- **Browser**: Fresh npm `0.1.0-beta.3` external Vite consumer passed 99/99
  browser tests across Chromium, Firefox, and WebKit, plus a 900-second
  six-context page/module-service-worker soak (3,555 cycles).
- **Flutter**: Fresh pub.dev `0.1.0-beta.3` Android consumer passed `flutter analyze`,
  ARM64/x86_64 debug/release APK builds, and 16 KiB ELF/ZIP alignment checks;
  hosted iOS simulator and unsigned-release consumer builds passed.
- **CLI**: Native Linux/macOS Unix stream stress (5m) and soak (15m) across all
  four TCP/Unix forwarding combinations, cargo-dist archive smoke, and
  executable-only upgrade rehearsals from `1.2.0` and `1.3.0-beta.2` passed.

---

## 0.1.0-beta.2 & CLI 1.3.0-beta.2 — 2026-10-17 / 2026-10-21

### Browser `0.1.0-beta.2` (2026-10-17)

Added the app-developer README covering installation, ticket setup, first
`/health` request, JSON and streaming calls, Vite/WASM deployment,
status/lifecycle handling, and troubleshooting. API and implementation remained
compatible with `0.1.0-beta.1`.

| SDK | Registry | Install |
| --- | --- | --- |
| Browser | [locho_web 0.1.0-beta.2](https://www.npmjs.com/package/locho_web/v/0.1.0-beta.2) | `npm install --save-exact locho_web@0.1.0-beta.2` |

- **npm tarball integrity**:
  `sha512-OEpW+aNcQ80ChJSixIQPiCIcpA8Z9WxoAp0Aq7vI5+m/W149hZhXlzuEcYtuMRg3kdkyGX+93Xc2IOl/ZsNflQ==`
- **Package source provenance**: clean commit
  `e9d09b8020e947a23fda8d5d96adf52122ea1065`, wire protocol `3`, ticket format `1`.

### Flutter `0.1.0-beta.2` (2026-10-17)

Documentation-focused update with app-developer onboarding for Android/iOS
configuration, foreground lifecycle, and troubleshooting.

| SDK | Registry | Install |
| --- | --- | --- |
| Flutter | [locho_flutter 0.1.0-beta.2](https://pub.dev/packages/locho_flutter/versions/0.1.0-beta.2) | `locho_flutter: 0.1.0-beta.2` |

- **pub.dev archive SHA-256**:
  `f835a7d8dd5d8ccba0799da8dd5bd4f3b294d6aced52954bb6bdc553c0c1503b`
- **Package source provenance**: clean commit
  `ebd6e10dfe61dcbcbfcd625eff6f1ee65567b7bf`, wire protocol `3`, ticket format `1`.

### CLI `1.3.0-beta.2` (2026-10-21)

Added macOS/Linux Unix stream socket forwarding, shared-peer HTTP failure
isolation, host QUIC 128-stream window alignment, cancellation/lifecycle
hardening, bounded browser discovery retries, and `iroh` `1.4.0` across source
builds (preparation base `8e1dcc3c8bd97496a9e13dfccfcea97c86c0aeee`, candidate
`61c8f5a79a74dae4cf8a2b61d60f32f424195e64`).

---

## 0.1.0-beta.1 & CLI 1.3.0-beta.1 — 2026-10-17

Initial public SDK beta and CLI ticket integration prerelease (`locho/3` wire
protocol and ticket format `1`).

| Product | Registry / Tag | Install |
| --- | --- | --- |
| Browser | [locho_web 0.1.0-beta.1](https://www.npmjs.com/package/locho_web/v/0.1.0-beta.1) | `npm install --save-exact locho_web@0.1.0-beta.1` |
| Flutter | [locho_flutter 0.1.0-beta.1](https://pub.dev/packages/locho_flutter/versions/0.1.0-beta.1) | `locho_flutter: 0.1.0-beta.1` |
| CLI | [GitHub `v1.3.0-beta.1`](https://github.com/trchopan/locho/releases/tag/v1.3.0-beta.1) | Tag commit `3786da3127338b2530e0cd8cea8ef23bc6d55dfd` |

- **npm tarball integrity**:
  `sha512-q5utHDCCFIegS9yFrW8mVO5rEsAOwteyrqex9xcpGRsny/gMBq+4lwtrESBhPfkbZjaSiAiTNwQ7Oh6R43fI2g==`
- **pub.dev archive SHA-256**:
  `40d69471f0a0fe99bf19788eb260e428d30fc1124c4cfdede6bbef6172eb0234`
- **SDK source provenance**: base revision
  `cb1b232b95500b27a7c9fcd4c7c9264a347b05a5` (`dirty: true` due to initial
  release-preparation changes).
- **Examples baseline**: Verified against
  [`locho_sdk_examples`](https://github.com/trchopan/locho_sdk_examples) commits
  `efa44bbd1528342f8633323b93f274984051216c` and
  `57f95ff94835de5525e77e182d9921245dba773b` (Notes API v1, OpenAPI 1.1.0).

---

## Target Verification Boundaries & Known Beta Limitations

- **CLI native targets**: Linux `x86_64`, macOS Apple silicon, and Windows
  `x86_64` have native hosted CI and release-smoke coverage. Linux `ARM64` and
  macOS `x86_64` distribution archives are cross-compiled unless a separate
  native rehearsal is recorded. Windows `ARM64` is not distributed.
- **Unix stream sockets**: Supported on macOS and Linux only; Windows rejects
  Unix upstreams and listeners while supporting TCP attachments.
- **Mobile & browser scope**: Android `ARM64`/`x86_64` debug/release source
  builds, 16 KiB alignment checks, and Android emulator runtime are verified.
  iOS simulator and unsigned-release compilation run in CI, but physical iOS
  runtime, broad physical-device/mobile-browser journeys, real Wi-Fi/cellular
  transitions, actual BFCache, and controlled/authenticated browser relays
  remain open gates. Flutter is foreground-only.
- **Stale relay hints**: Browser discovery with stale relay hints can still
  intermittently time out over public N0 relays. Application requests are never
  automatically replayed.

## Publishing Subsequent SDK Releases

- **Browser (`locho_web`)**: Build and test the package tarball from a clean
  committed source checkout (`npm run build:wasm && npm run build && npm run build:package`),
  then publish that exact tarball with `--tag beta` (leaving `latest` unchanged
  during beta). Verify registry integrity and run
  `LOCHO_NPM_VERSION=<version> npm run test:external`.
- **Flutter (`locho_flutter`)**: Follow the standalone staging and verification
  procedure in [flutter-package.md](flutter-package.md). Always publish from a
  verified staging directory outside the Git checkout (`scripts/package-flutter.py --output ...`),
  keep FRB runtime and codegen versions pinned together, and verify actual
  `pub.dev` installation afterward with
  `python3 scripts/test-flutter-package.py --platform android --hosted <version>`.

# SDK beta releases

## 0.1.0-beta.2 — 2026-10-17

Flutter beta.2 is a documentation-focused update. The API, wire protocol, ticket
format and native implementation remain compatible with beta.1; browser consumers
continue using `locho_web` `0.1.0-beta.1`.

| SDK | Registry | Install |
| --- | --- | --- |
| Flutter | [locho_flutter](https://pub.dev/packages/locho_flutter/versions/0.1.0-beta.2) | `locho_flutter: 0.1.0-beta.2` in pubspec, then `flutter pub get` |

The package README now starts with the app-developer path: install, create a
ticket, make a first `package:http` request, configure Android/iOS, manage the
foreground lifecycle and troubleshoot common failures. Advanced source-build and
FRB details remain linked for maintainers.

### Registry integrity

- pub.dev archive SHA-256:
  `f835a7d8dd5d8ccba0799da8dd5bd4f3b294d6aced52954bb6bdc553c0c1503b`
- Package source provenance: clean Locho commit
  `ebd6e10dfe61dcbcbfcd625eff6f1ee65567b7bf`, wire protocol 3, ticket format 1.

### Verification

- Pub staging outside the checkout passed package validation with the intentional
  exact FRB dependency warning.
- Flutter formatting, analyze, unit tests, Rust bridge tests and Clippy passed.
- Fresh external Android consumer from the beta.2 archive passed analyze,
  ARM64/x86_64 debug/release APK builds and 16 KiB ELF/ZIP alignment checks.
- Fresh external Android consumer installed from pub.dev beta.2 passed the same
  analyze, debug/release and alignment checks.
- Hosted Flutter CI passed Android debug/release and iOS simulator/unsigned-release
  builds. Physical iOS runtime and broad device/network acceptance remain open.

## 0.1.0-beta.1 — 2026-10-17

Both packages are publicly available. SDK versions are independent of the Locho
CLI version; these packages use wire protocol 3 and attachment ticket format 1.

| SDK | Registry | Install |
| --- | --- | --- |
| Browser | [locho_web](https://www.npmjs.com/package/locho_web/v/0.1.0-beta.1) | `npm install --save-exact locho_web@0.1.0-beta.1` |
| Flutter | [locho_flutter](https://pub.dev/packages/locho_flutter/versions/0.1.0-beta.1) | `locho_flutter: 0.1.0-beta.1` in pubspec, then `flutter pub get` |

Browser imports use `locho_web` and `locho_web/wasm?url`. There is no published
`@locho/web` package. Flutter consumers need Rust and Android/iOS build toolchains;
the package bundles matching Locho core/native sources and generated FRB bindings.
Neither installation requires a vendor bootstrap, GitHub login or SDK checkout.

### Registry integrity

- npm tarball integrity:
  `sha512-q5utHDCCFIegS9yFrW8mVO5rEsAOwteyrqex9xcpGRsny/gMBq+4lwtrESBhPfkbZjaSiAiTNwQ7Oh6R43fI2g==`
- pub.dev archive SHA-256:
  `40d69471f0a0fe99bf19788eb260e428d30fc1124c4cfdede6bbef6172eb0234`

Package provenance records base source revision
`cb1b232b95500b27a7c9fcd4c7c9264a347b05a5` and `dirty: true`, because this initial
publication includes local release-preparation changes. Native sources are bundled
with the Flutter distribution rather than fetched from a separate Locho Git revision.

### Verification

- Browser TypeScript/WASM build and tarball allowlist validation.
- Fresh external Vite consumer installed from npm with scripts disabled, production
  build under `/sdk/`, explicit WASM assets, and 69 passing Chromium/Firefox/WebKit
  cases through a real Locho host, including JSON notes and incremental SSE.
- Flutter analyze/unit tests: five pass; the optional real-bridge ticket test is
  skipped without `LOCHO_TEST_LIBRARY`.
- Fresh external Android consumers from both local archive and pub.dev: analyze,
  ARM64/x86_64 debug/release APK builds and ELF/ZIP 16 KiB alignment checks pass.
- iOS builds were not rerun for this publication: full Xcode is unavailable locally.
  Physical-device/mobile-browser and broad network acceptance remain separate gates.

The examples repository subsequently verified the published packages at commit
`efa44bbd1528342f8633323b93f274984051216c`: browser/API and private HTTPS deployment
CI pass, and [Flutter CI](https://github.com/trchopan/locho_sdk_examples/actions/runs/39028168937)
passes Android debug/release and iOS simulator/unsigned-release builds. This is
hosted compilation evidence, not physical iOS runtime acceptance. See the examples'
[registry acceptance](https://github.com/trchopan/locho_sdk_examples/blob/efa44bbd1528342f8633323b93f274984051216c/docs/registry-acceptance.md)
for runtime results and historical unexplained public-relay health-check timeouts.

### Subsequent publication

Build/test the browser tarball, then publish that exact file with `--tag beta`.
If npm stages the upload, use its Staged Packages page or `npm stage` commands to
review and approve it; a `0.0.0-stage` placeholder does not mean the SDK is installable.
Verify registry integrity and run `LOCHO_NPM_VERSION=<version> npm run test:external`
with the fixture environment documented in the package README.

For Flutter, follow [the staging/publication instructions](flutter-package.md).
Keep FRB runtime/codegen pinned together; pub's exact-version warning is intentional.
Use a new version for each release and test actual registry consumption afterward.

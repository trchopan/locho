# Contributing to locho

Thank you for your interest in contributing to `locho`! This guide covers the
repository layout, local development setup for the CLI and SDKs, quality gates,
and pull request workflow.

For an index of all technical specifications and operational guides, see
[docs/README.md](docs/README.md).

## Repository & Workspace Architecture

`locho` is organized as a Cargo workspace alongside standalone Browser and
Flutter SDK packages:

```text
locho/
|-- src/                        # CLI binary (`locho` host, attach, share, secret, rotate-secret, diagnose)
|-- crates/
|   |-- locho-core/             # Shared `locho/3` wire protocol, capability proofs, ticket v1 codec, and PeerSession supervisor
|   |-- locho-client-native/    # Native TCP loopback attachment API and low-level CLI TCP/Unix stream forwarding adapter
|   `-- locho-web/              # Rust-to-WASM bindings and bounded HTTP/1.1 client framing for browsers
|-- packages/
|   |-- locho-web/              # TypeScript/ESM Browser SDK (`locho_web` on npm) and Playwright test suites
|   `-- locho_flutter/          # Flutter mobile SDK (`locho_flutter` on pub.dev) using Flutter Rust Bridge & Cargokit
|-- tests/
|   |-- integration.rs          # Process-level HTTP, TCP, concurrency, rotation, and shutdown tests
|   |-- release_smoke.rs        # Packaged release-binary smoke tests (including Unix streams)
|   `-- perf/                   # Native Unix stream smoke/stress/soak harness & Docker HTTP/TCP load harness
|-- scripts/                    # CI verification, release packaging, upgrade rehearsal, and Flutter packaging scripts
`-- docs/                       # Operational guides, protocol/SDK specifications, roadmap, and release records
```

See [docs/sdk-foundation.md](docs/sdk-foundation.md) for the architectural
boundary between the CLI and the shared SDK crates, and
[docs/native-client.md](docs/native-client.md) for the `locho-client-native`
lifecycle contract.

## Development Setup

### 1. Core CLI & Rust Workspace

**Prerequisites:**
- **Rust `1.96` or newer** with Cargo (`rustfmt` and `clippy` components)
- **Python `3.11+`** (on macOS and Linux, used by the Unix stream release smoke
  and performance harness tests)
- Outbound network access for `iroh` discovery and relay connectivity during
  integration tests

Build and run the core test suite from the repository root:

```sh
cargo build --release
cargo test
```

The default Cargo workspace members include the root `locho` CLI,
`crates/locho-core`, `crates/locho-client-native`, and `crates/locho-web`. To
test individual crates or the protocol-only core without `iroh` transport:

```sh
cargo test -p locho-core --no-default-features
cargo test -p locho-core --features transport
cargo test -p locho-client-native -- --test-threads=1
```

### 2. Browser SDK (`crates/locho-web` & `packages/locho-web`)

Root CLI builds do not require WASM or Node tooling. When modifying the browser
SDK (`crates/locho-web` or `packages/locho-web`), install:

- Rust target `wasm32-unknown-unknown` (`rustup target add wasm32-unknown-unknown`)
- A WebAssembly-capable `clang`/`lld` toolchain (on macOS, install Homebrew LLVM
  rather than relying on Apple Clang)
- `wasm-bindgen-cli` `0.2.126` (`cargo install wasm-bindgen-cli --version 0.2.126 --locked`)
- **Node.js `24.18.0+`** and npm

Build and typecheck the browser package:

```sh
cargo build --locked --release -p locho-web --target wasm32-unknown-unknown
cd packages/locho-web
npm ci
npm run build:wasm
npm run build
npm run build:package
```

See [docs/browser-http.md](docs/browser-http.md) and
[packages/locho-web/tests/SOAK.md](packages/locho-web/tests/SOAK.md) for running
Playwright browser and soak tests.

### 3. Flutter SDK (`packages/locho_flutter`)

Root CLI builds do not require Flutter. When modifying `packages/locho_flutter`,
install:

- **Flutter `3.44.5`** / **Dart `3.12.2`**
- **Rust `1.96+`** with Android (`aarch64-linux-android`, `x86_64-linux-android`)
  or iOS (`aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`) targets
- Android SDK (API 36+, NDK `28.2.13676358`, JDK 21) and/or Xcode with CocoaPods

Run Dart and Rust bridge checks from `packages/locho_flutter`:

```sh
cd packages/locho_flutter
flutter pub get
dart format --output=none --set-exit-if-changed lib test example/lib example/integration_test
flutter analyze
flutter test
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path rust/Cargo.toml
```

See [docs/flutter-package.md](docs/flutter-package.md) for standalone source
packaging (`scripts/package-flutter.py`) and external consumer verification
(`scripts/test-flutter-package.py`).

## Quality Gate

Before opening a pull request, run the same source, lint, and process-level
checks used by CI from a clean checkout:

```sh
scripts/verify-ci.sh
```

On Windows PowerShell:

```powershell
.\scripts\Verify-CI.ps1
```

You can also run the Python harness unit tests on macOS or Linux:

```sh
python3 -m unittest discover -s tests/release -p test_verify_upgrade.py -v
python3 -m unittest discover -s tests/perf -p test_unix_streams.py -v
python3 -m unittest discover -s tests/perf/loadgen -p test_loadgen.py -v
```

- **Integration & smoke coverage**: The integration tests cover HTTP and TCP
  forwarding, concurrency, restart, capability rotation, malformed requests,
  timeouts, and graceful shutdown. The release smoke tests exercise the ordinary
  release binary without test-only features.
- **Native Unix stream smoke, stress & soak**: The Unix release smoke gate
  covers native TCP/Unix forwarding, listener security/lifecycle, and recovery
  using generic fixtures. For stress and soak profiles, see
  [tests/perf/README.md](tests/perf/README.md#native-unix-stream-smoke-stress-and-soak-tests).
- **Custom relay verification**: Custom relay checks are environment-dependent
  and are not part of the deterministic CI gate. Follow the
  [manual relay verification guide](docs/custom-relays.md#manual-relay-verification)
  when changing relay configuration, authentication, or endpoint construction.

## Release Verification

The complete release gate builds `cargo-dist` artifacts, verifies checksums,
checks installers, and runs smoke tests against the packaged binary.

On Linux or macOS:

```sh
scripts/verify-release.sh
```

On Windows PowerShell:

```powershell
.\scripts\Verify-Release.ps1
```

Use `--skip-artifacts` or `-SkipArtifacts` when `cargo-dist` packaging is not
installed locally. Maintainers should follow
[docs/releasing.md](docs/releasing.md) for the complete release checklist and
supported-target requirements.

## Pull Request Guidelines

- Keep changes focused and include tests for behavioral changes.
- Ensure `scripts/verify-ci.sh` (or `Verify-CI.ps1`) passes and the working tree
  is clean.
- Update [`README.md`](README.md), [`docs/faq.md`](docs/faq.md),
  [`CHANGELOG.md`](CHANGELOG.md), or relevant guides in [`docs/`](docs/README.md)
  when user-facing behavior or operational expectations change.
- Do not add network routing, arbitrary host access, or hosted coordination
  without first revisiting the product boundary and decision gates in
  [docs/roadmap.md](docs/roadmap.md).

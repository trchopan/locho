# Locho Flutter native feasibility package

Foreground-only Android/iOS Dart wrapper over `locho-client-native`, using pinned
Flutter Rust Bridge **2.12.0** and its Cargokit source-build template. This is an
unpublished prerelease package, not a Flutter web SDK. A standalone source-build
archive is available through CI; see `docs/flutter-package.md` in the Locho
repository for artifact installation and external-consumer verification.

## Toolchain and build

Verified locally: Flutter **3.44.5**, Dart **3.12.2**, Rust **1.96.1**, Android NDK
**28.2.13676358**, JDK **21**. Android minimum API 23; initial native ABI matrix is
ARM64 + x86_64. iOS deployment target 13.0 is configured but unverified locally;
full Xcode/CocoaPods and macOS CI are needed for compilation.

```sh
cargo install flutter_rust_bridge_codegen --version 2.12.0 --locked
cargo install cargo-expand --version 1.0.127 --locked
rustup target add aarch64-linux-android x86_64-linux-android
```

From this package:

```sh
flutter pub get
flutter_rust_bridge_codegen generate
dart format --output=none --set-exit-if-changed lib test example/lib example/integration_test
flutter analyze
flutter test
cargo test --locked --manifest-path rust/Cargo.toml
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
```

From `example/`:

```sh
flutter pub get
flutter build apk --debug --target-platform android-arm64,android-x64
flutter build apk --release --target-platform android-arm64,android-x64
flutter run -d <android-device-id>
```

Cargokit compiles Rust automatically. Bridge has an isolated Cargo workspace/lock
and is excluded from root CLI workspace gates; CLI builds need no Flutter tools.
Local development core/native paths require this checkout. The distribution builder
rewrites the archive dependency to an exact public Git revision with a locked Cargo
graph. Consumers extract the archive and reference that package directory, with no
sibling Rust sources. Internet/native source-build toolchains are required.

Vendored Cargokit comes from the FRB 2.12 plugin template, with two local Gradle
patches: `ExecOperations` replaces removed `Project.exec` for Gradle 9; generated
ABI selection is restricted to ARM64/x86_64. It uses `--locked` for all modes. No
prebuilt binaries are downloaded. Preserve its license/provenance during upgrades.

## Public API

```dart
import 'package:locho_flutter/locho_flutter.dart';

final client = await LochoClient.create();
final tunnel = await client.attach(
  ticket: await LochoTicket.parse(ticketText),
  connectTimeout: const Duration(seconds: 15),
);
try {
  // Ordinary package:http/Dio calls to tunnel.httpBaseUri.resolve('/health').
  // Reuse the same tunnel for the foreground screen/session.
} finally {
  await tunnel.close();
  await client.close();
}
```

- `start` returns after setup/bind, then `waitConnected` or status observations can
  show peer progress. A wait timeout alone leaves the attachment running.
- `attach` covers setup/connect with one 15-second default deadline, closes partial
  or late results before reporting failure, and accepts up to 300 seconds. Native
  setup is itself bounded to 15 seconds; late-start cleanup can extend return time.
- Optional `localPort` defaults zero; optional `directAddress` is development input.
  Always IPv4 loopback, never arbitrary bind addresses. `LochoTicket.parse` uses the
  Rust codec; metadata provides host/service/type/relay hints, `export()` explicitly
  returns the credential, and `toString()` redacts it. Supply either a ticket or
  explicit `hostId`/`capability`, never both. HTTP tickets fail with
  `unsupportedService`. Native Rust relay settings exist but are not exposed in Dart.
- `currentStatus` contains revision/state/path/port, terminal `errorCode` and latest
  sanitized `connectionErrorCode`; `snapshot()` refreshes it. `statuses` starts with
  the current snapshot, then coalesces watch updates. Each subscriber has one native
  long-poll in flight (at most one second); no unbounded event queue. Unsubscribe
  does not close its tunnel. Calls after close retain final status.
- Peer `connected` means host identity connectivity, not service authorization or
  application login. Explicit `/health` is separate. Wrong capability may appear as
  HTTP socket closure, not a typed auth exception. Do not replay failed writes.
- Errors are fixed `LochoException.code` categories. Generated bindings are internal.
  Avoid diagnostics containing host/capability input or request headers/bodies.
- Close is idempotent; client close owns partial starts and all attachments. Dart
  futures cannot be cancelled: applications must retain/close client ownership when
  screens are disposed, and generation-check late results. Abandoning a Future alone
  is not cancellation. Native finalization is best effort, never primary cleanup.

## Runtime and Android initialization

FRB dispatch uses its executor for FFI calls; all native operations are explicitly
spawned onto one process-lifetime, two-worker Tokio runtime initialized once. No
per-call runtime, nested block_on, UI-thread networking, global tracing, or signals.
Closing a client never destroys the runtime, and a new client can be created later.
Native handles are opaque; their Rust owner drops request best-effort cancellation.

An Android FlutterPlugin loads the Rust library and installs process-lifetime
Application/JVM refs before Dart creates a client. It initializes both iroh system
DNS and rustls platform certificate verification. The Java verifier artifact
`org.rustls:rustls-platform-verifier:0.2.0` must match the Android support crate in
`rust/Cargo.lock`; adjust both together. The bridge rejects absent context rather
than relying on DNS fallback or panic unwinding. Never disable TLS verification.

The example main manifest includes INTERNET. Its cleartext exception is scoped to
`127.0.0.1`; relay traffic remains HTTPS. iOS has a local-network purpose description;
ATS/local HTTP behavior still requires real-client/device evidence.

## Physical-device verification

Start/build the sibling `locho_sdk_examples` API first (`npm ci && npm run build`).
From the Locho repository root, with release CLI already built:

```sh
python3 scripts/flutter-device-smoke.py <android-device-id>
python3 scripts/flutter-device-smoke.py <android-device-id> --discovery
python3 scripts/flutter-device-smoke.py <android-device-id> --release
```

Harness starts temporary API/Locho processes, chooses a LAN direct address, writes
credential inputs only to a temporary mode-0600 dart-define file, executes the Dart
integration test and cleans up. Override `LOCHO_HOST_IP`/`LOCHO_EXAMPLES_DIR` if
needed. Device must reach the selected host UDP address. `--discovery` omits explicit
address in the SDK; it is environment-dependent, not deterministic CI evidence.

Flutter test/Driver cannot run non-web release integration tests in this toolchain.
The release harness builds autonomous `lib/release_smoke.dart`, installs it via adb
and waits for a fixed pass marker without logging inputs. Release APK uses a debug
signing key for development, not store distribution.

## Evidence — 2026-10-14 and 2026-10-15

| Check | Result |
| --- | --- |
| Dart analyze/tests; Rust bridge host compile/Clippy | Passed |
| Android ARM64/x86_64 native builds | Passed |
| Android debug APK | Passed |
| Joy 2, Android 10/API 29, ARM64 — direct LAN attach/health/close/recreate | Passed |
| Same device — host-ID discovery without direct override | Passed; selected network/path is environment-dependent |
| Same device — malformed host, wrong-secret HTTP failure, timeout cleanup/port release | Passed in debug integration |
| Same device — release attach/health/close twice | Passed |
| Native 16 KiB ELF alignment | Checked by scripts/check-android-pages.py; physical 16 KiB OS pending |
| emulator-5554, Android 17/API 37, arm64-v8a, 16384-byte pages — CLI-ticket debug integration | Passed 2026-10-15: ticket attach/health, close/reconnect, client recreation, invalid host, wrong secret and timeout/port cleanup |
| Same 16 KiB emulator — CLI-ticket ARM64 release smoke | Passed 2026-10-15: build/install and two ticket attach/health/close cycles |
| Release APK ELF/ZIP alignment | Four native libraries passed ELF LOAD alignment; zipalign -c -P 16 passed |
| iOS simulator/device compilation | Passed hosted simulator/debug and unsigned-device/release CI through PR #91 |
| Physical iOS, physical-device ticket acceptance, Wi-Fi/cellular and authenticated custom relay | Pending |
| Full UI pause/resume/lock/unlock/manual network matrix | Pending; generation-based policy implemented in example |
| External source-build consumer / standalone distribution | Android debug/release builds, alignment and 16 KiB emulator CLI-ticket runtime passed; hosted external iOS builds pending |

This completes the Android attach/GET/close feasibility path, not the full platform
G1 matrix. Tiny example is memory-only; full notes/login/remembered-session apps
belong in `locho_sdk_examples`, sharing its API. No capability/ticket is saved.

Ticket consumption follow-up: shared vectors pass through the real FRB library on
macOS. CI builds a Linux native library and runs the same Dart conformance test with
`LOCHO_TEST_LIBRARY`. The device harness supplies a CLI-generated ticket for
its first debug attachment and both release cycles. This path passes on the 16 KiB
ARM64 emulator; physical-device ticket acceptance remains unverified. These runs use
a direct-address candidate and do not establish a relay path. Ticket hints and optional direct addresses are separate from
client relay configuration. iroh does not guarantee ticket-array dialing order.

Reproduce from the root after building the release CLI and reference API:
`python3 scripts/flutter-device-smoke.py emulator-5554` and the same command with
`--release`. Confirm the target with `adb shell getprop ro.product.cpu.abi` and
`adb shell getconf PAGE_SIZE`. Release uses development signing, not store signing.

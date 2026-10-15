# Flutter standalone source-build package

`scripts/package-flutter.py` creates `target/flutter-package/locho_flutter-0.1.0-dev.1.tar.gz`,
a SHA-256 sidecar and a file manifest. CI uploads them as `locho-flutter-source-package`.
No pub.dev publication or prebuilt native download occurs.

## Consumer setup

Download the archive and checksum from the same CI artifact, verify the checksum,
and extract it outside the SDK checkout:

```sh
shasum -a 256 -c locho_flutter-0.1.0-dev.1.tar.gz.sha256
tar -xzf locho_flutter-0.1.0-dev.1.tar.gz
```

Reference the extracted package from your application's pubspec:

```yaml
dependencies:
  locho_flutter:
    path: ../locho_flutter
```

This path points to the delivered archive, not a sibling source checkout. Pin the
archive checksum and retain the application's pubspec.lock. Only import
`package:locho_flutter/locho_flutter.dart`; generated FRB modules are internal.

The artifact contains curated Dart exports/bindings, Android/iOS integration,
vendored Cargokit/license, bridge Rust source/lock/config, synthetic ticket vectors,
package docs/license and `BUILD.json`. It excludes the in-repository example,
desktop scaffolds, build caches and native binaries. BUILD.json records package
version, source revision/dirty state, native dependency revision and protocol versions.

## Rust sources and reproducibility

Development keeps `../../../crates/locho-client-native` for local changes. Packaging
rewrites that one dependency to the full Git revision
`4f8aa20c3bcbabb667ef2354510bdb101d690fa6` from the public Locho repository, including
its matching core crate. This reviewed revision includes ticket support. The
distribution lockfile updates source identities and is checked against the original
registry version/checksum set. Consumers build with `--locked` in all Cargokit modes.
An upgrade of the pinned native/core revision is a deliberate package change.

Internet access is needed for Git/crates.io, Dart packages and native build tools
on a cold machine. Cargokit compiles the native library during application builds;
consumers do not copy generated bindings or need unpublished sibling Rust crates.
This is a source-build distribution, not a claim of hermetic/offline builds.

## Toolchains and platform configuration

- Flutter 3.44.5 / Dart 3.12.2, FRB runtime/codegen 2.12.0.
- Rust 1.96 or newer with appropriate Android/iOS targets; Cargokit uses stable.
- Android: JDK 21 recommended, SDK/compile API 36+, NDK 28.2.13676358, minimum API 23,
  initial ARM64/x86_64 ABIs. Put INTERNET permission in the main manifest. For plain
  HTTP clients on loopback, scope cleartext permission to `127.0.0.1` rather than
  granting a global exception. The external test demonstrates this configuration.
- iOS: full Xcode/CocoaPods, deployment target 13+. Current integration uses
  CocoaPods; configure Flutter with `flutter config --no-enable-swift-package-manager`.
  A local-network usage description is included in the consuming test app.
  Real-device ATS/permissions/signing behavior requires separate acceptance.

App Store signing, privacy declarations and publication remain consuming-app work.
The release test uses development signing, not store credentials. Package lifecycle
remains foreground-only. See the package README for ticket/HTTP/error semantics.

## External-consumer verification

```sh
python3 scripts/package-flutter.py
python3 scripts/test-flutter-package.py --platform android
# Optional real runtime acceptance (release CLI and reference API must be built):
python3 scripts/test-flutter-package.py --platform android --device emulator-5554
python3 scripts/test-flutter-package.py --platform ios
```

The verifier creates a fresh app in an OS temporary directory, installs the extracted
archive package, analyzes it and builds debug/release artifacts. It copies only
public-export app entry points from the tiny example, not package/native sources.
Android checks ELF LOAD/ZIP 16 KiB alignment. With --device it also runs the real
CLI-ticket health/lifecycle integration and autonomous release smoke. Temporary
apps and extracted files are cleaned up after completion/failure.

Local macOS evidence: fresh external Android ARM64/x86_64 debug/release builds,
alignment checks, and CLI-ticket debug/release runtime on Android 17/API 37 ARM64
16 KiB emulator passed. iOS external builds run in hosted CI. Physical-device ticket
acceptance, physical 16 KiB hardware, full mobile network/lifecycle matrix and
authenticated relays remain open. This is package acceptance, not the full notes app.

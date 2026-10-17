#!/usr/bin/env python3
"""Build a fresh app outside this checkout using only the packaged Flutter SDK."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def run(args, cwd, env=None):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", choices=["android", "ios"], required=True)
    parser.add_argument("--device", help="Android device/emulator for debug and release runtime tests")
    parser.add_argument("--hosted", metavar="VERSION", help="Verify the published pub.dev version instead of the local archive")
    args = parser.parse_args()
    if not args.hosted:
        manifest = json.loads((ROOT / "target/flutter-package/manifest.json").read_text())
        archive = ROOT / "target/flutter-package" / manifest["archive"]
        import hashlib
        if hashlib.sha256(archive.read_bytes()).hexdigest() != manifest["sha256"]:
            raise RuntimeError("Flutter package checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="locho-external-flutter-") as temporary:
        directory = Path(temporary)
        if not args.hosted:
            with tarfile.open(archive) as source:
                source.extractall(directory, filter="data")
            package = directory / "locho_flutter"
            if any((package / name).exists() for name in ["example", "build", "rust/target"]):
                raise RuntimeError("Package includes development artifacts")
            if "../../../crates" in (package / "rust/Cargo.toml").read_text():
                raise RuntimeError("Package still depends on sibling Rust paths")
            for crate in ["locho-core", "locho-client-native"]:
                if not (package / "rust/crates" / crate / "Cargo.toml").is_file():
                    raise RuntimeError(f"Package is missing bundled {crate}")
            subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version=1", "--no-deps", "--manifest-path", str(package / "rust/Cargo.toml")], cwd=directory, check=True, stdout=subprocess.DEVNULL)
        app = directory / "consumer"
        run(["flutter", "create", "--platforms", args.platform, "--org", "dev.chop", "--project-name", "locho_external", str(app)], directory)
        pubspec = '''name: locho_external
publish_to: none
version: 0.1.0+1
environment:
  sdk: '>=3.12.2 <4.0.0'
dependencies:
  flutter:
    sdk: flutter
  locho_flutter:
    path: ../locho_flutter
  http: 1.6.0
dev_dependencies:
  flutter_lints: 6.0.0
  flutter_test:
    sdk: flutter
  integration_test:
    sdk: flutter
flutter:
  uses-material-design: true
'''
        if args.hosted:
            pubspec = pubspec.replace("  locho_flutter:\n    path: ../locho_flutter", f"  locho_flutter: {args.hosted}")
        (app / "pubspec.yaml").write_text(pubspec)
        shutil.rmtree(app / "test")
        example = ROOT / "packages/locho_flutter/example"
        # These app entry points import curated package exports, never generated FRB.
        for name in ["lib/main.dart", "lib/release_smoke.dart", "integration_test/tunnel_test.dart"]:
            destination = app / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(example / name, destination)
        run(["flutter", "pub", "get"], app)
        run(["flutter", "analyze"], app)
        if args.platform == "android":
            gradle = app / "android/app/build.gradle.kts"
            gradle.write_text(gradle.read_text().replace("ndkVersion = flutter.ndkVersion", 'ndkVersion = "28.2.13676358"')
                .replace("minSdk = flutter.minSdkVersion", "minSdk = 23"))
            android_manifest = app / "android/app/src/main/AndroidManifest.xml"
            android_manifest.write_text(android_manifest.read_text().replace("<application", '<uses-permission android:name="android.permission.INTERNET"/><application android:networkSecurityConfig="@xml/network_security_config"'))
            resources = app / "android/app/src/main/res/xml"
            resources.mkdir(parents=True, exist_ok=True)
            (resources / "network_security_config.xml").write_text('<network-security-config><domain-config cleartextTrafficPermitted="true"><domain includeSubdomains="false">127.0.0.1</domain></domain-config></network-security-config>')
            for mode in ["debug", "release"]:
                run(["flutter", "build", "apk", f"--{mode}", "--target-platform=android-arm64,android-x64"], app)
            apk = app / "build/app/outputs/flutter-apk/app-release.apk"
            run(["python3", str(ROOT / "scripts/check-android-pages.py"), str(apk)], ROOT)
            sdk = Path(os.environ.get("ANDROID_HOME", Path.home() / "Library/Android/sdk"))
            candidates = sorted((sdk / "build-tools").glob("*/zipalign"))
            run([str(candidates[-1]), "-c", "-P", "16", "4", str(apk)], app)
            if args.device:
                env = {**os.environ, "LOCHO_FLUTTER_CONSUMER": str(app), "LOCHO_ANDROID_PACKAGE": "dev.chop.locho_external"}
                for flags in [[], ["--release"]]:
                    run(["python3", str(ROOT / "scripts/flutter-device-smoke.py"), args.device, *flags], ROOT, env)
        else:
            import plistlib
            info = app / "ios/Runner/Info.plist"
            values = plistlib.loads(info.read_bytes())
            values["NSLocalNetworkUsageDescription"] = "Connect to your selected Locho host."
            info.write_bytes(plistlib.dumps(values))
            run(["flutter", "build", "ios", "--simulator", "--debug"], app)
            run(["flutter", "build", "ios", "--release", "--no-codesign"], app)
        print(f"External Flutter {'pub.dev ' + args.hosted if args.hosted else 'archive'} consumer passed: {args.platform}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Create a standalone, source-build Flutter archive. Does not publish packages."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = ROOT / "packages/locho_flutter"
# Reviewed native/core implementation, available remotely before package creation.
NATIVE_REVISION = "4f8aa20c3bcbabb667ef2354510bdb101d690fa6"
OUTPUT = ROOT / "target/flutter-package"


def main():
    OUTPUT.mkdir(parents=True, exist_ok=True)
    revision = subprocess.check_output(["git", "rev-parse", NATIVE_REVISION], cwd=ROOT, text=True).strip()
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip())
    version = next(line.split(":", 1)[1].strip() for line in (PACKAGE / "pubspec.yaml").read_text().splitlines() if line.startswith("version:"))
    with tempfile.TemporaryDirectory(prefix="locho-flutter-package-") as temporary:
        stage = Path(temporary) / "locho_flutter"
        stage.mkdir()
        tracked = subprocess.check_output(["git", "ls-files", "packages/locho_flutter"], cwd=ROOT, text=True).splitlines()
        roots = {"lib", "android", "ios", "rust", "cargokit"}
        names = {"pubspec.yaml", "pubspec.lock", "LICENSE", "README.md", "CHANGELOG.md", "analysis_options.yaml"}
        for value in tracked:
            path = Path(value).relative_to("packages/locho_flutter")
            if path.parts[0] not in roots and str(path) not in names:
                continue
            # Never distribute tests/cache/build products from vendored build tooling.
            if any(part in {".dart_tool", "build", "target", "test", "test_data"} for part in path.parts):
                continue
            destination = stage / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(PACKAGE / path, destination)
        manifest = stage / "rust/Cargo.toml"
        contents = manifest.read_text()
        expected = 'locho-client-native = { path = "../../../crates/locho-client-native" }'
        if contents.count(expected) != 1:
            raise RuntimeError("Unexpected native dependency; review package transformation")
        manifest.write_text(contents.replace(expected,
            f'locho-client-native = {{ git = "https://github.com/trchopan/locho.git", rev = "{revision}" }}'))
        vectors = stage / "rust/tests/vectors"
        vectors.mkdir(parents=True)
        shutil.copy2(ROOT / "crates/locho-core/tests/vectors/tickets-v1.json", vectors / "tickets-v1.json")
        api = stage / "rust/src/api/simple.rs"
        api.write_text(api.read_text().replace('../../../../../crates/locho-core/tests/vectors/tickets-v1.json', '../../tests/vectors/tickets-v1.json'))
        # Retain existing versions while changing only native/core source identities.
        subprocess.run(["cargo", "check", "--manifest-path", str(manifest)], check=True, cwd=stage)
        # Source relocation must not silently upgrade registry dependencies.
        import tomllib
        before = tomllib.loads((PACKAGE / "rust/Cargo.lock").read_text())["package"]
        after = tomllib.loads((stage / "rust/Cargo.lock").read_text())["package"]
        registry = lambda packages: sorted((p["name"], p["version"], p.get("checksum")) for p in packages if p.get("source", "").startswith("registry+"))
        if registry(before) != registry(after):
            raise RuntimeError("Distribution lock unexpectedly changed registry versions")
        subprocess.run(["cargo", "metadata", "--locked", "--format-version=1", "--no-deps", "--manifest-path", str(manifest)], check=True, cwd=stage, stdout=subprocess.DEVNULL)
        shutil.rmtree(stage / "rust/target", ignore_errors=True)
        (stage / "BUILD.json").write_text(json.dumps({"version": version, "sourceRevision": source, "dirty": dirty,
            "nativeRevision": revision, "wireProtocol": 3, "ticketFormat": 1, "distribution": "source-build"}, indent=2) + "\n")
        shutil.copy2(ROOT / "docs/flutter-package.md", stage / "INSTALL.md")
        archive = OUTPUT / f"locho_flutter-{version}.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(stage, arcname="locho_flutter")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (OUTPUT / (archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")
        (OUTPUT / "manifest.json").write_text(json.dumps({"archive": archive.name, "sha256": digest,
            "files": sorted(str(p.relative_to(stage)) for p in stage.rglob("*") if p.is_file())}, indent=2) + "\n")
        print(archive)


if __name__ == "__main__":
    main()

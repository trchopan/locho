#!/usr/bin/env python3
"""Create a standalone, source-build Flutter archive. Does not publish packages."""
import argparse
import hashlib
import gzip
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PACKAGE = ROOT / "packages/locho_flutter"
OUTPUT = ROOT / "target/flutter-package"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=OUTPUT,
                        help="Output directory; use a directory outside Git for pub publication")
    output_directory = parser.parse_args().output.resolve()
    output_directory.mkdir(parents=True, exist_ok=True)
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    epoch = int(subprocess.check_output(["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT, text=True).strip())
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip())
    version = next(line.split(":", 1)[1].strip() for line in (PACKAGE / "pubspec.yaml").read_text().splitlines() if line.startswith("version:"))
    temp_root = os.environ.get("RUNNER_TEMP") or None
    with tempfile.TemporaryDirectory(prefix="locho-flutter-package-", dir=temp_root) as temporary:
        stage = Path(temporary) / "locho_flutter"
        stage.mkdir()
        tracked = subprocess.check_output(["git", "ls-files", "packages/locho_flutter"], cwd=ROOT, text=True).splitlines()
        roots = {"lib", "android", "ios", "rust", "cargokit"}
        names = {"pubspec.yaml", ".pubignore", "LICENSE", "README.md", "CHANGELOG.md", "analysis_options.yaml"}
        tracked = sorted(set(tracked) | {f"packages/locho_flutter/{name}" for name in names if (PACKAGE / name).is_file()})
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
            'locho-client-native = { path = "crates/locho-client-native" }'))
        # Ship the matching implementation, not a second Git dependency or sibling
        # checkout. Only source, manifests and test fixtures are copied.
        for crate in ["locho-core", "locho-client-native"]:
            tracked_crate = subprocess.check_output(["git", "ls-files", f"crates/{crate}"], cwd=ROOT, text=True).splitlines()
            for value in tracked_crate:
                relative = Path(value).relative_to("crates")
                destination = stage / "rust/crates" / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / value, destination)
            shutil.copy2(ROOT / "LICENSE", stage / "rust/crates" / crate / "LICENSE")
        vectors = stage / "rust/tests/vectors"
        vectors.mkdir(parents=True)
        shutil.copy2(ROOT / "crates/locho-core/tests/vectors/tickets-v1.json", vectors / "tickets-v1.json")
        api = stage / "rust/src/api/simple.rs"
        api.write_text(api.read_text().replace('../../../../../crates/locho-core/tests/vectors/tickets-v1.json', '../../tests/vectors/tickets-v1.json'))
        # Relocation makes the bundled crates workspace members. Allow Cargo to
        # update workspace bookkeeping, then reject any registry dependency drift.
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
            "nativeRevision": source, "wireProtocol": 3, "ticketFormat": 1, "distribution": "bundled-source-build"}, indent=2) + "\n")
        shutil.copy2(ROOT / "docs/flutter-package.md", stage / "INSTALL.md")
        # Keep the verified staging tree for pub publish; publish this directory,
        # never the development package with its sibling Rust path.
        published = output_directory / "locho_flutter"
        if published.exists():
            shutil.rmtree(published)
        shutil.copytree(stage, published)
        archive = output_directory / f"locho_flutter-{version}.tar.gz"
        def normalize(info):
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = epoch
            return info
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=epoch) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as output:
                    output.add(stage, arcname="locho_flutter", filter=normalize)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (output_directory / (archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")
        (output_directory / "manifest.json").write_text(json.dumps({"archive": archive.name, "sha256": digest,
            "files": sorted(str(p.relative_to(stage)) for p in stage.rglob("*") if p.is_file())}, indent=2) + "\n")
        print(archive)


if __name__ == "__main__":
    main()

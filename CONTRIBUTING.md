# Contributing

## Development Setup

Install Rust stable with Cargo. The integration and release smoke tests also
need network access for iroh discovery and relay connectivity.

Build and run the basic test suite from the repository root:

```sh
cargo build --release
cargo test
```

## Quality Gate

Before opening a pull request, run the same source and process-level checks used
by CI:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release
cargo build --release --features integration-test --target-dir target/integration-release
LOCHO_TEST_BINARY=target/integration-release/release/locho cargo test --test integration --features integration-test
LOCHO_TEST_BINARY=target/release/locho cargo test --test release_smoke -- --ignored
```

On Windows, use `target\integration-release\release\locho.exe` and
`target\release\locho.exe` for the two `LOCHO_TEST_BINARY` values.

The integration tests cover HTTP and TCP forwarding, concurrency, restart,
capability rotation, malformed requests, timeouts, and graceful shutdown. The
release smoke tests exercise the ordinary release binary without the test-only
feature.

## Release Verification

The complete release gate also builds cargo-dist artifacts, verifies checksums,
checks installers, and runs smoke tests against the packaged binary.

On Unix:

```sh
scripts/verify-release.sh
```

On Windows:

```powershell
.\scripts\Verify-Release.ps1
```

Use `--skip-artifacts` or `-SkipArtifacts` when cargo-dist packaging is not
available. Maintainers should follow [RELEASE.md](RELEASE.md) for the complete
1.0 checklist and supported-target requirements.

## Pull Requests

- Keep changes focused and include tests for behavioral changes.
- Update the README, FAQ, changelog, or release documentation when user-facing
  behavior or operational expectations change.
- Do not add network routing, arbitrary host access, or hosted coordination
  without first revisiting the product boundary in `ROADMAP.md`.

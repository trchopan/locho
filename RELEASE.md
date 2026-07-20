# Release Checklist

This document is the release gate for `locho`. Complete it from a clean checkout
before creating a version tag. The verification scripts intentionally run the
same checks locally and in CI.

## Release Candidate

- [ ] Confirm the version in `Cargo.toml`, `Cargo.lock`, and the changelog.
- [ ] Confirm the changelog describes user-visible changes and known limitations.
- [ ] Confirm `README.md`, `FAQ.md`, and installation instructions use the
      release version and supported target matrix.
- [ ] Run `scripts/verify-release.sh` on Linux or macOS.
- [ ] Run `scripts/Verify-Release.ps1` on Windows.
- [ ] Run the artifact smoke test against the archive produced by cargo-dist.
- [ ] Verify the archive contains the binary, `README.md`, `CHANGELOG.md`, and
      `LICENSE`.
- [ ] Verify the archive checksum and generated installers.
- [ ] Verify the documented HTTP and TCP quickstarts with the packaged binary.
- [ ] Verify host identity and service capabilities survive an executable-only
      upgrade.
- [ ] Verify stale capabilities fail after rotation and unaffected services keep
      working.
- [ ] Review logs and diagnostics for leaked capabilities or private keys.

## Supported Targets

| Target              | Distribution                 | Verification status                                         |
| ------------------- | ---------------------------- | ----------------------------------------------------------- |
| Linux x86_64        | tar.xz and shell installer   | Native CI and release smoke                                 |
| Linux ARM64         | tar.xz and shell installer   | Cross-compiled; native rehearsal required before 1.0        |
| macOS x86_64        | tar.xz and shell installer   | Native or cross-compiled CI; release smoke                  |
| macOS Apple silicon | tar.xz and shell installer   | Native or cross-compiled CI; release smoke                  |
| Windows x86_64      | zip and PowerShell installer | Native CI and release smoke                                 |
| Windows ARM64       | Not distributed              | Explicitly unsupported; `ring` cross-compilation is blocked |

Do not mark a target as release-verified based only on a successful cross-build.
The packaged binary and installer must be exercised on that target, or the
release notes must state that verification is pending.

## Publish

- [ ] Merge the release-candidate changes with a clean CI result.
- [ ] Update `Cargo.toml` and `Cargo.lock` to the final version.
- [ ] Move the final notes from `Unreleased` into the versioned changelog entry.
- [ ] Run the complete release gate again from a clean checkout.
- [ ] Create and push the matching tag, for example `v1.0.0`.
- [ ] Confirm the GitHub Release contains every supported archive, checksum, and
      installer.
- [ ] Download each published artifact into a clean temporary directory and
      repeat checksum, installer, `--help`, and quickstart verification.
- [ ] Record any target-specific verification limitations in the release notes.

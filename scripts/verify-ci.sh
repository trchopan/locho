#!/usr/bin/env bash

set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

expected_commit=${LOCHO_EXPECTED_COMMIT:-$(git rev-parse HEAD)}
actual_commit=$(git rev-parse HEAD)
[[ "$actual_commit" == "$expected_commit" ]] || {
    printf 'CI commit mismatch: expected %s, got %s\n' "$expected_commit" "$actual_commit" >&2
    exit 1
}
[[ -z "$(git status --porcelain --untracked-files=normal)" ]] || {
    printf 'CI verification requires a clean checkout\n' >&2
    exit 1
}

run() {
    printf '\n+ %s\n' "$*"
    "$@"
}

run cargo fmt --all -- --check
run cargo clippy --workspace --all-targets --all-features -- -D warnings
# Keep the PR path on the debug profile. Packaged artifact jobs exercise the
# ordinary release binary separately on every supported OS.
run cargo test --workspace --all-targets --all-features -- --test-threads=1
run cargo test -p locho-core --no-default-features
run cargo metadata --locked --format-version=1 --manifest-path packages/locho_flutter/rust/Cargo.toml >/dev/null

printf '\nCI verification passed.\n'

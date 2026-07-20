#!/usr/bin/env bash

set -euo pipefail

usage() {
    printf 'usage: %s [--skip-artifacts]\n' "$0" >&2
}

skip_artifacts=false
if [[ $# -gt 1 ]]; then
    usage
    exit 2
elif [[ $# -eq 1 ]]; then
    if [[ "$1" != "--skip-artifacts" ]]; then
        usage
        exit 2
    fi
    skip_artifacts=true
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

run() {
    printf '\n+ %s\n' "$*"
    "$@"
}

run cargo fmt --all -- --check
run cargo clippy --all-targets --all-features -- -D warnings
run cargo test --all-targets --all-features
run cargo build --release
run cargo build --release --features integration-test --target-dir target/integration-release
env LOCHO_TEST_BINARY=target/integration-release/release/locho \
    cargo test --test integration --features integration-test
env LOCHO_TEST_BINARY=target/release/locho \
    cargo test --test release_smoke -- --ignored

if [[ "$skip_artifacts" == true ]]; then
    printf '\nRelease verification passed (artifact packaging skipped).\n'
    exit 0
fi

command -v dist >/dev/null 2>&1 || {
    printf 'cargo-dist is required; install it or use --skip-artifacts\n' >&2
    exit 1
}

target=$(rustc -vV | sed -n 's/^host: //p')
version=$(cargo metadata --no-deps --format-version 1 \
    | sed -n 's/.*"name":"locho","version":"\([^"]*\)".*/\1/p')
[[ -n "$target" && -n "$version" ]] || {
    printf 'could not determine native target or package version\n' >&2
    exit 1
}

run dist build --artifacts=all --target="$target" --tag="v$version" --allow-dirty
archive="target/distrib/locho-${target}.tar.xz"
[[ -f target/distrib/locho-installer.sh ]] || {
    printf 'cargo-dist did not produce the shell installer\n' >&2
    exit 1
}
sh -n target/distrib/locho-installer.sh
grep -F "locho-${target}.tar.xz" target/distrib/sha256.sum >/dev/null
binary=$(scripts/verify-release-artifact.sh "$archive" target/release-artifact)
env LOCHO_TEST_BINARY="$binary" \
    cargo test --test release_smoke -- --ignored

printf '\nRelease verification passed for %s (%s).\n' "$version" "$target"

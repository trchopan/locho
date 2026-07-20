#!/usr/bin/env bash

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
verifier="$script_dir/verify-release-artifact.sh"
fixture_dir=$(mktemp -d "${TMPDIR:-/tmp}/locho-artifact-test.XXXXXX")
trap 'rm -rf "$fixture_dir"' EXIT

mkdir "$fixture_dir/package"
printf '#!/bin/sh\nexit 0\n' > "$fixture_dir/package/locho"
chmod +x "$fixture_dir/package/locho"
printf 'README\n' > "$fixture_dir/package/README.md"
printf 'CHANGELOG\n' > "$fixture_dir/package/CHANGELOG.md"
printf 'LICENSE\n' > "$fixture_dir/package/LICENSE"

archive="$fixture_dir/package.tar.xz"
tar -cJf "$archive" -C "$fixture_dir" package
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive" | sed 's#  # *#' > "$archive.sha256"
else
    shasum -a 256 "$archive" | sed 's#  # *#' > "$archive.sha256"
fi

"$verifier" "$archive" "$fixture_dir/extracted"

if "$verifier" "$archive" "/" >/dev/null 2>&1; then
    printf 'unsafe extraction directory was accepted\n' >&2
    exit 1
fi

printf '0%.0s' {1..64} > "$archive.sha256"
if "$verifier" "$archive" "$fixture_dir/bad-checksum" >/dev/null 2>&1; then
    printf 'corrupt checksum was accepted\n' >&2
    exit 1
fi

rm "$fixture_dir/package/LICENSE"
incomplete_archive="$fixture_dir/incomplete.tar.xz"
tar -cJf "$incomplete_archive" -C "$fixture_dir" package
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$incomplete_archive" | sed 's#  # *#' > "$incomplete_archive.sha256"
else
    shasum -a 256 "$incomplete_archive" | sed 's#  # *#' > "$incomplete_archive.sha256"
fi
if "$verifier" "$incomplete_archive" "$fixture_dir/incomplete-extracted" >/dev/null 2>&1; then
    printf 'incomplete archive was accepted\n' >&2
    exit 1
fi

printf 'artifact verifier negative tests passed\n'

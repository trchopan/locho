#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    printf 'usage: %s ARCHIVE EXTRACTION_DIRECTORY [EXPECTED_COMMIT]\n' "$0" >&2
    exit 2
fi

archive=$1
extract_dir=$2
checksum="${archive}.sha256"
expected_commit=${3:-${LOCHO_EXPECTED_COMMIT:-}}

case "$extract_dir" in
    ""|"."|".."|/)
        printf 'refusing unsafe extraction directory: %s\n' "$extract_dir" >&2
        exit 2
        ;;
esac

[[ -f "$archive" ]] || { printf 'archive does not exist: %s\n' "$archive" >&2; exit 1; }
[[ -f "$checksum" ]] || { printf 'checksum does not exist: %s\n' "$checksum" >&2; exit 1; }

read -r expected _ < "$checksum"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive" | cut -d ' ' -f 1)
else
    printf 'no SHA-256 utility is available\n' >&2
    exit 1
fi
if [[ "$expected" != "$actual" ]]; then
    printf 'checksum mismatch for %s\n' "$archive" >&2
    exit 1
fi

archive_name=$(basename "$archive" .tar.xz)
rm -rf "$extract_dir"
mkdir -p "$extract_dir"
tar -xJf "$archive" -C "$extract_dir"

artifact_dir="$extract_dir/$archive_name"
binary="$artifact_dir/locho"
for required_file in README.md CHANGELOG.md LICENSE; do
    [[ -f "$artifact_dir/$required_file" ]] || {
        printf 'archive is missing %s\n' "$required_file" >&2
        exit 1
    }
done

[[ -x "$binary" ]] || { printf 'archive binary is not executable: %s\n' "$binary" >&2; exit 1; }
"$binary" --help >/dev/null
version=$("$binary" --version)
if [[ ! "$version" =~ ^locho\ [0-9]+\.[0-9]+\.[0-9]+([^[:space:]]*)\ \(commit\ [0-9a-f]{40},\ clean\)$ ]]; then
    printf 'packaged binary has invalid release provenance: %s\n' "$version" >&2
    exit 1
fi
if [[ -n "$expected_commit" && "$version" != *"commit $expected_commit, clean)" ]]; then
    printf 'packaged binary commit does not match %s\n' "$expected_commit" >&2
    exit 1
fi
printf '%s\n' "$binary"

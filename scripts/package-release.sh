#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: package-release.sh <binary> <version> <target> <source-root> <dist-dir>" >&2
  exit 2
fi

binary_path=$1
version=$2
target=$3
source_root=$4
dist_dir=$5

if [[ ! -f "$binary_path" ]]; then
  echo "missing release input: $binary_path" >&2
  exit 1
fi

for document in README.md LICENSE-MIT LICENSE-APACHE; do
  if [[ ! -f "$source_root/$document" ]]; then
    echo "missing release input: $source_root/$document" >&2
    exit 1
  fi
done

if [[ ! "$version" =~ ^[0-9A-Za-z.+-]+$ ]]; then
  echo "invalid release version: $version" >&2
  exit 1
fi
if [[ ! "$target" =~ ^[0-9A-Za-z._-]+$ ]]; then
  echo "invalid release target: $target" >&2
  exit 1
fi

stem="jira-ops-v${version}-${target}"
archive_name="$stem.tar.gz"
archive_path="$dist_dir/$archive_name"
checksum_path="$archive_path.sha256"
staging_root=$(mktemp -d "${TMPDIR:-/tmp}/jira-ops-package.XXXXXX")

cleanup() {
  status=$?
  rm -rf -- "$staging_root"
  if [[ $status -ne 0 ]]; then
    rm -f -- "$archive_path" "$checksum_path"
  fi
  exit "$status"
}
trap cleanup EXIT

mkdir -p "$staging_root/$stem" "$dist_dir"
install -m 0755 "$binary_path" "$staging_root/$stem/jira-ops"
install -m 0644 \
  "$source_root/README.md" \
  "$source_root/LICENSE-MIT" \
  "$source_root/LICENSE-APACHE" \
  "$staging_root/$stem/"

tar -czf "$archive_path" -C "$staging_root" "$stem"
(
  cd "$dist_dir"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive_name" > "$archive_name.sha256"
  else
    shasum -a 256 "$archive_name" > "$archive_name.sha256"
  fi
)

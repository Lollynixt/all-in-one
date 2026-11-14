#!/usr/bin/env bash
# Build a release binary on THIS machine and package it the way the in-app updater expects.
# Use when CI can't build your platform (e.g. Apple Silicon runners on a private repo).
# Usage: scripts/package-local.sh [vX.Y.Z]   -> dist/all-in-one-<target>.tar.gz (+ .sha256)
# Then, with the GitHub CLI:  gh release upload vX.Y.Z dist/* --clobber
set -euo pipefail
cd "$(dirname "$0")/.."

target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$target" ] || { echo "rustc not found" >&2; exit 1; }
bin=all-in-one; case "$target" in *windows*) bin=all-in-one.exe;; esac

tag="${1:-}"
if [ -n "$tag" ]; then
  v="${tag#v}"
  cp Cargo.toml Cargo.toml.bak
  trap 'mv -f Cargo.toml.bak Cargo.toml' EXIT
  python3 - "$v" <<'PY'
import re, sys
s = open("Cargo.toml").read()
s = re.sub(r'(?m)^version = ".*"$', f'version = "{sys.argv[1]}"', s, count=1)
open("Cargo.toml", "w").write(s)
PY
fi

cargo build --release -p aio-app
name="all-in-one-$target.tar.gz"
mkdir -p dist
tar -czf "dist/$name" -C target/release "$bin"
(cd dist && (shasum -a 256 "$name" 2>/dev/null || sha256sum "$name") | cut -d' ' -f1 > "$name.sha256")
echo "Built dist/$name"; ls -l dist

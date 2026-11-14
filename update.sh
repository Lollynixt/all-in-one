#!/usr/bin/env bash
# Update All In One from the newest all-in-one-prototype*.tar.gz in ~/Downloads (or a path you pass), then build and run.
# Usage: ./update.sh [path/to/tarball] [--no-run]
# Keeps target/ (fast incremental builds) and never touches your data in ~/AllInOne.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN=1
TARBALL=""
for a in "$@"; do
  case "$a" in
    --no-run) RUN=0 ;;
    *) TARBALL="$a" ;;
  esac
done

if [[ -z "$TARBALL" ]]; then
  TARBALL="$(ls -t "$HOME"/Downloads/all-in-one-prototype*.tar.gz 2>/dev/null | head -n1 || true)"
fi
if [[ -z "$TARBALL" || ! -f "$TARBALL" ]]; then
  echo "No all-in-one-prototype*.tar.gz found in ~/Downloads. Pass a path: ./update.sh /path/to/file.tar.gz" >&2
  exit 1
fi
command -v cargo >/dev/null || { echo "cargo not found. Install Rust: https://rustup.rs" >&2; exit 1; }
command -v rsync >/dev/null || { echo "rsync not found." >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "Unpacking $TARBALL"
tar xzf "$TARBALL" -C "$TMP"
SRC="$TMP/all-in-one"
[[ -f "$SRC/Cargo.toml" ]] || { echo "Tarball doesn't look like All In One (no all-in-one/Cargo.toml)." >&2; exit 1; }

# Keep a one-deep backup of the previous source in case something is wrong with the new drop.
BACKUP="$HERE.prev"
rm -rf "$BACKUP"
mkdir -p "$BACKUP"
rsync -a --exclude target "$HERE/" "$BACKUP/"

rsync -a --delete --exclude target "$SRC/" "$HERE/"
echo "Source updated (previous version kept at $BACKUP)"

cd "$HERE"
if [[ "$RUN" -eq 1 ]]; then
  exec cargo run -p aio-app --release
else
  cargo build -p aio-app --release
fi

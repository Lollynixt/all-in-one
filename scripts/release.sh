#!/usr/bin/env bash
# Publish a release: tags the current commit and pushes the tag. GitHub Actions builds the apps
# (macOS Apple Silicon and Intel, Linux, Windows) and attaches them to a GitHub Release.
#
#   scripts/release.sh 0.2.0            stable release
#   scripts/release.sh 0.3.0-beta.1     pre-release (only offered to people on the beta channel)
set -euo pipefail
v="${1:?usage: scripts/release.sh <version>, e.g. 0.2.0}"
v="${v#v}"
[[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "version must look like 1.2.3 or 1.2.3-beta.1" >&2; exit 1; }
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Commit your changes first." >&2
  exit 1
fi
git rev-parse "v$v" >/dev/null 2>&1 && { echo "Tag v$v already exists." >&2; exit 1; }
git tag "v$v"
git push origin HEAD "v$v"
echo "Pushed v$v. Watch the build: https://github.com/$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')/actions"

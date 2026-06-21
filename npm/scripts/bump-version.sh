#!/usr/bin/env bash
# Version management -- single source of truth. Bumps Cargo.toml (the binary
# reads it via env!("CARGO_PKG_VERSION")) AND every npm/ package.json, so the
# installed `umadev --version` always equals the published npm version.
# Usage: npm/scripts/bump-version.sh 1.0.4
set -euo pipefail
[[ $# -eq 1 && "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "usage: $(basename "$0") <x.y.z>   e.g. $(basename "$0") 1.0.4" >&2; exit 1
}
NEW="$1"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
perl -i -pe "s/^version = \"\d+\.\d+\.\d+\"/version = \"$NEW\"/" "$ROOT/Cargo.toml"
find "$ROOT/npm" -name package.json -not -path '*/node_modules/*' \
  -exec perl -i -pe "s/\"\d+\.\d+\.\d+\"/\"$NEW\"/g" {} +
( cd "$ROOT" && cargo check -p umadev-spec >/dev/null 2>&1 || true )
echo "version -> $NEW  (Cargo.toml + npm/*/package.json + Cargo.lock)"
echo "then: git commit -am \"release: $NEW\" && git tag \"v$NEW\" && git push origin HEAD:main && git push origin \"v$NEW\""

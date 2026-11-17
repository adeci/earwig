#!/bin/sh
# Regenerates packaging/THIRD-PARTY-NOTICES.txt. Needs cargo-about on PATH and
# an offline cargo that can resolve the lockfile (the Nix check sets that up).
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
out=${1:-$root/packaging/THIRD-PARTY-NOTICES.txt}
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
cargo-about generate --frozen \
  --config "$root/packaging/notices/about.toml" \
  --output-file "$tmp" \
  "$root/packaging/notices/about.hbs"
# Workspace crates are earwig itself; their licence is LICENSE.
grep -v -E '^- earwig( |$|-|w )' "$tmp" > "$out"

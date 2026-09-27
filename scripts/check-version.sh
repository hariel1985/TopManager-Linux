#!/usr/bin/env bash
# Verify every version string agrees (optionally with a tag): used by
# bump-version.sh and by CI before publishing a release.
set -euo pipefail
cd "$(dirname "$0")/.."

CARGO="$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
EXT="$(sed -n 's/.*"version-name": *"\([^"]*\)".*/\1/p' shell-extension/*/metadata.json)"
fail=0
[ "$CARGO" = "$EXT" ] || { echo "extension metadata.json says $EXT, Cargo.toml says $CARGO" >&2; fail=1; }
if [ $# -gt 0 ]; then
    [ "${1#v}" = "$CARGO" ] || { echo "tag $1 does not match Cargo.toml version $CARGO" >&2; fail=1; }
fi
[ $fail = 0 ] && echo "version $CARGO consistent"
exit $fail

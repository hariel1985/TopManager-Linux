#!/usr/bin/env bash
# Set the release version everywhere, commit, and tag it:
#   scripts/bump-version.sh 0.2.0 [--push]
#
# Cargo.toml's [workspace.package] version is the single source of truth; the
# Shell extension metadata follows it, and the release workflow refuses tags
# that don't match. Pushing the tag publishes the GitHub release.
set -euo pipefail
cd "$(dirname "$0")/.."

NEW="${1:?usage: bump-version.sh X.Y.Z [--push]}"
PUSH="${2:-}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "not a semver: $NEW" >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "working tree not clean" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$NEW" >/dev/null && { echo "tag v$NEW already exists" >&2; exit 1; }

sed -i "/^\[workspace.package\]/,/^\[/ s/^version *= *\".*\"/version = \"$NEW\"/" Cargo.toml
sed -i "s/\"version-name\": *\"[^\"]*\"/\"version-name\": \"$NEW\"/" shell-extension/*/metadata.json
cargo update --workspace --quiet

scripts/check-version.sh "v$NEW"
git add Cargo.toml Cargo.lock shell-extension/*/metadata.json
git commit -q -m "Release v$NEW"
git tag -a "v$NEW" -m "TopManager $NEW"
echo "Tagged v$NEW."
if [ "$PUSH" = "--push" ]; then
    git push origin HEAD "v$NEW"
    echo "Pushed; the release workflow will publish https://github.com/$(git remote get-url origin | sed -E 's#.*github.com[:/](.*)\.git$#\1#; s#.*github.com[:/]##')/releases/tag/v$NEW"
else
    echo "Publish with: git push origin HEAD v$NEW"
fi

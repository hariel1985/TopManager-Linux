#!/usr/bin/env bash
# Build a release tarball for the host architecture:
#   dist/topmanager-<version>-<arch>.tar.gz
#
# The daemon is linked statically against musl, so the tarball runs on any
# Linux distribution regardless of its glibc version.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="$(uname -m)"
case "$ARCH" in amd64) ARCH=x86_64 ;; arm64) ARCH=aarch64 ;; esac
TARGET="$ARCH-unknown-linux-musl"
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
NAME="topmanager-$VERSION-$ARCH"
OUT="dist/$NAME"

cargo build --release --locked --target "$TARGET" -p tm-daemon

rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/share/dbus-1/services" "$OUT/share/systemd" "$OUT/share/gnome-shell/extensions"
install -m755 "target/$TARGET/release/topmanagerd" "$OUT/bin/"
install -m755 install.sh "$OUT/"
install -m644 data/io.github.hariel1985.TopManager.service "$OUT/share/dbus-1/services/"
install -m644 data/topmanagerd.service.in "$OUT/share/systemd/"
cp -r shell-extension/topmanager@hariel1985.github.io "$OUT/share/gnome-shell/extensions/"
cp LICENSE README.md "$OUT/"
echo "$VERSION" > "$OUT/VERSION"

tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
(cd dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "dist/$NAME.tar.gz"

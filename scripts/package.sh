#!/usr/bin/env bash
# Build a release tarball for the host architecture:
#   dist/topmanager-<version>-<arch>.tar.gz
#
# The daemon is linked statically against musl, so it runs on any Linux
# distribution regardless of its glibc version. The window links the system's
# GTK 4 / libadwaita, so it is built on the oldest supported target
# (Ubuntu 24.04: GTK 4.14, libadwaita 1.5) in CI.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="$(uname -m)"
case "$ARCH" in amd64) ARCH=x86_64 ;; arm64) ARCH=aarch64 ;; esac
TARGET="$ARCH-unknown-linux-musl"
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)"
NAME="topmanager-$VERSION-$ARCH"
OUT="dist/$NAME"

cargo build --release --locked --target "$TARGET" -p tm-daemon
cargo build --release --locked -p tm-gui

rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/share/dbus-1/services" "$OUT/share/systemd" "$OUT/share/gnome-shell/extensions" \
    "$OUT/share/applications"
install -m755 "target/$TARGET/release/topmanagerd" "$OUT/bin/"
install -m755 target/release/topmanager "$OUT/bin/"
install -m644 data/io.github.hariel1985.TopManager.desktop.in "$OUT/share/applications/"
cp -r data/icons "$OUT/share/"
install -m755 install.sh "$OUT/"
install -m644 data/io.github.hariel1985.TopManager.Daemon.service "$OUT/share/dbus-1/services/"
install -m644 data/topmanagerd.service.in "$OUT/share/systemd/"
cp -r shell-extension/topmanager@hariel1985.github.io "$OUT/share/gnome-shell/extensions/"
cp LICENSE README.md "$OUT/"
echo "$VERSION" > "$OUT/VERSION"

tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
(cd dist && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "dist/$NAME.tar.gz"

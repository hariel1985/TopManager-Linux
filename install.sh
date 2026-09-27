#!/usr/bin/env bash
# TopManager installer / updater / uninstaller.
#
#   Install the latest release of a repo (per user, no root needed):
#     curl -fsSL https://raw.githubusercontent.com/hariel1985/TopManager-Linux/main/install.sh \
#       | bash -s -- --repo hariel1985/TopManager-Linux
#
#   From an extracted release tarball:   ./install.sh
#   Update to the newest release:         topmanager-install update
#   Remove:                               topmanager-install uninstall [--purge]
#
# Every path is derived from $HOME / XDG variables at run time, so the same
# script works for any user name and any home location.

set -euo pipefail

DEFAULT_REPO="hariel1985/TopManager-Linux"
UUID="topmanager@hariel1985.github.io"
BUS_NAME="io.github.hariel1985.TopManager.Daemon"
# 0.1.0 used the app id as the daemon's bus name; its activation file is removed.
LEGACY_BUS_NAME="io.github.hariel1985.TopManager"
UNIT="topmanagerd.service"
APP_ID="io.github.hariel1985.TopManager"

die() { echo "topmanager-install: $*" >&2; exit 1; }
say() { echo "==> $*"; }

usage() {
    cat <<EOF
Usage: install.sh [install] [--repo OWNER/NAME] [--version TAG|latest] [--user|--system] [--no-enable]
       install.sh update    [--repo OWNER/NAME]
       install.sh uninstall [--user|--system] [--purge]
       install.sh status

  --repo      GitHub repository to install from (default: the one recorded at
              install time, else $DEFAULT_REPO). Private repos work when the
              GitHub CLI (gh) is logged in.
  --version   Release tag, e.g. v0.2.0 (default: latest)
  --user      Install into ~/.local for the current user (default)
  --system    Install into /usr/local for all users (needs root)
  --purge     With uninstall: also delete settings and history
EOF
}

# ---------------------------------------------------------------- arguments
CMD=install
REPO=""
VERSION="latest"
MODE=""
ENABLE=1
PURGE=0
LOCAL=0
while [ $# -gt 0 ]; do
    case "$1" in
        install|update|uninstall|status) CMD="$1" ;;
        --repo) REPO="${2:?--repo needs OWNER/NAME}"; shift ;;
        --repo=*) REPO="${1#*=}" ;;
        --version) VERSION="${2:?--version needs a tag}"; shift ;;
        --version=*) VERSION="${1#*=}" ;;
        --user) MODE=user ;;
        --system) MODE=system ;;
        --no-enable) ENABLE=0 ;;
        --purge) PURGE=1 ;;
        # Internal: install from this script's own directory (a downloaded
        # release) and record --repo as its origin.
        --local) LOCAL=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument '$1'" ;;
    esac
    shift
done

[ -n "${HOME:-}" ] || die "\$HOME is not set"

if [ -z "$MODE" ]; then
    if [ "$(id -u)" -eq 0 ]; then MODE=system; else MODE=user; fi
fi

# ------------------------------------------------------------------ layout
# An XDG base directory from the environment, used only if it is absolute and
# writable by us (walking up to the first existing ancestor). A value that
# points into someone else's home, e.g. from a system-wide /etc/environment,
# falls back to the default under our own $HOME. topmanagerd applies the
# same rule, so both always agree.
xdg_dir() {
    local var="$1" fallback="$HOME/$2" value="${!1:-}" probe
    if [ -n "$value" ] && [ "${value#/}" != "$value" ]; then
        probe="$value"
        while [ ! -e "$probe" ]; do probe="$(dirname "$probe")"; done
        if [ -w "$probe" ]; then
            echo "$value"
            return
        fi
        echo "topmanager-install: ignoring $var=$value (not writable), using $fallback" >&2
    fi
    echo "$fallback"
}

set_layout() {
    if [ "$MODE" = system ]; then
        [ "$(id -u)" -eq 0 ] || die "--system needs root (try: sudo $0 $CMD --system)"
        PREFIX=/usr/local
        BIN_DIR="$PREFIX/bin"
        DATA_DIR="$PREFIX/share"
        UNIT_DIR="$PREFIX/lib/systemd/user"
        UNIT_BIN_DIR="$BIN_DIR"
        RECEIPT_DIR="$DATA_DIR/topmanager"
    else
        BIN_DIR="$HOME/.local/bin"
        DATA_DIR="$(xdg_dir XDG_DATA_HOME .local/share)"
        UNIT_DIR="$(xdg_dir XDG_CONFIG_HOME .config)/systemd/user"
        # systemd expands %h to the home directory of whoever runs the unit.
        if [ "$BIN_DIR" = "$HOME/.local/bin" ]; then UNIT_BIN_DIR="%h/.local/bin"; else UNIT_BIN_DIR="$BIN_DIR"; fi
        RECEIPT_DIR="$DATA_DIR/topmanager"
    fi
    EXT_DIR="$DATA_DIR/gnome-shell/extensions/$UUID"
    APPS_DIR="$DATA_DIR/applications"
    ICON_DIR="$DATA_DIR/icons/hicolor"
    DBUS_DIR="$DATA_DIR/dbus-1/services"
    RECEIPT="$RECEIPT_DIR/install.json"
}
set_layout

receipt_field() {
    [ -f "$RECEIPT" ] || return 0
    sed -n "s/.*\"$1\": *\"\([^\"]*\)\".*/\1/p" "$RECEIPT" | head -n1
}

arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo x86_64 ;;
        aarch64|arm64) echo aarch64 ;;
        *) die "unsupported architecture $(uname -m) (supported: x86_64, aarch64)" ;;
    esac
}

have() { command -v "$1" >/dev/null 2>&1; }

user_systemctl() {
    # Only meaningful for the invoking user's own session.
    [ "$MODE" = user ] && have systemctl && systemctl --user "$@" 2>/dev/null
}

# ------------------------------------------------------------------ download
latest_tag() {
    local repo="$1"
    if have gh && gh auth status >/dev/null 2>&1; then
        gh release view -R "$repo" --json tagName -q .tagName 2>/dev/null && return 0
    fi
    have curl || die "curl is required"
    curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1
}

fetch_release() {
    local repo="$1" tag="$2" dest="$3" a
    a="$(arch)"
    local asset="topmanager-${tag#v}-$a.tar.gz"
    say "Downloading $asset from $repo ($tag)"
    if have gh && gh auth status >/dev/null 2>&1; then
        gh release download "$tag" -R "$repo" -p "$asset" -p SHA256SUMS -D "$dest" --clobber \
            || die "download failed (does release $tag have $asset?)"
    else
        have curl || die "curl is required (or install and log in to the GitHub CLI)"
        local base="https://github.com/$repo/releases/download/$tag"
        curl -fL --progress-bar -o "$dest/$asset" "$base/$asset" || die "download failed: $base/$asset"
        curl -fsSL -o "$dest/SHA256SUMS" "$base/SHA256SUMS" || die "download failed: $base/SHA256SUMS"
    fi
    (cd "$dest" && grep " $asset\$" SHA256SUMS | sha256sum -c --quiet -) || die "checksum mismatch for $asset"
    tar -xzf "$dest/$asset" -C "$dest"
    PAYLOAD="$dest/topmanager-${tag#v}-$a"
    [ -x "$PAYLOAD/bin/topmanagerd" ] || die "unexpected archive layout"
}

# ------------------------------------------------------------------- install
install_payload() {
    local src="$1" repo="$2"
    local version
    version="$(cat "$src/VERSION")"

    say "Installing TopManager $version ($MODE) into $BIN_DIR and $DATA_DIR"
    user_systemctl stop "$UNIT" || true

    install -Dm755 "$src/bin/topmanagerd" "$BIN_DIR/topmanagerd"
    install -Dm755 "$src/install.sh" "$BIN_DIR/topmanager-install"
    install -Dm644 "$src/share/dbus-1/services/$BUS_NAME.service" "$DBUS_DIR/$BUS_NAME.service"
    rm -f "$DBUS_DIR/$LEGACY_BUS_NAME.service"
    mkdir -p "$UNIT_DIR"
    sed "s|@BINDIR@|$UNIT_BIN_DIR|g" "$src/share/systemd/topmanagerd.service.in" > "$UNIT_DIR/$UNIT"
    chmod 644 "$UNIT_DIR/$UNIT"
    rm -rf "$EXT_DIR"
    mkdir -p "$(dirname "$EXT_DIR")"
    cp -r "$src/share/gnome-shell/extensions/$UUID" "$EXT_DIR"

    # The window (dynamically linked against the system's GTK 4/libadwaita).
    if [ -x "$src/bin/topmanager" ]; then
        install -Dm755 "$src/bin/topmanager" "$BIN_DIR/topmanager"
        mkdir -p "$APPS_DIR"
        # Desktop files can't expand ~, so the absolute path is written here,
        # at install time, from this user's own $HOME.
        sed "s|@BINDIR@|$BIN_DIR|g" "$src/share/applications/$APP_ID.desktop.in" > "$APPS_DIR/$APP_ID.desktop"
        chmod 644 "$APPS_DIR/$APP_ID.desktop"
        for icon in "$src"/share/icons/hicolor/*/apps/$APP_ID.png; do
            size="$(basename "$(dirname "$(dirname "$icon")")")"
            install -Dm644 "$icon" "$ICON_DIR/$size/apps/$APP_ID.png"
        done
        install -Dm644 "$src/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" \
            "$ICON_DIR/symbolic/apps/$APP_ID-symbolic.svg"
        have gtk-update-icon-cache && gtk-update-icon-cache -qtf "$ICON_DIR" 2>/dev/null || true
        have update-desktop-database && update-desktop-database -q "$APPS_DIR" 2>/dev/null || true
        if have ldd && ldd "$BIN_DIR/topmanager" 2>/dev/null | grep -q "not found"; then
            echo "    note: the TopManager window needs GTK 4 and libadwaita:"
            echo "          sudo apt install libgtk-4-1 libadwaita-1-0"
        fi
    fi

    mkdir -p "$RECEIPT_DIR"
    cat > "$RECEIPT" <<EOF
{
  "repo": "$repo",
  "version": "$version",
  "mode": "$MODE",
  "arch": "$(arch)",
  "installed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF

    if [ "$ENABLE" = 1 ]; then
        enable_everything
    fi
    say "Done. topmanagerd $("$BIN_DIR/topmanagerd" --version | awk '{print $2}') installed."
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) echo "    note: $BIN_DIR is not in your PATH" ;;
    esac
}

enable_extension() {
    # Ask the running Shell first; a freshly copied extension is only
    # discovered at the next login on Wayland, so also record it in
    # gsettings so it comes up enabled then.
    local enabled_now=0
    if have gnome-extensions && gnome-extensions enable "$UUID" 2>/dev/null; then
        enabled_now=1
    fi
    if have gsettings && gsettings list-schemas 2>/dev/null | grep -qx org.gnome.shell; then
        local cur
        cur="$(gsettings get org.gnome.shell enabled-extensions)"
        case "$cur" in
            *"'$UUID'"*) ;;
            "@as []"|"[]") gsettings set org.gnome.shell enabled-extensions "['$UUID']" ;;
            *) gsettings set org.gnome.shell enabled-extensions "${cur%]}, '$UUID']" ;;
        esac
        gsettings set org.gnome.shell disable-user-extensions false
    fi
    if [ "$enabled_now" = 1 ]; then
        say "Top-bar HUD enabled"
    else
        say "Top-bar HUD installed: log out and back in to see it (GNOME loads new extensions at login)"
    fi
}

enable_everything() {
    if [ "$MODE" = system ]; then
        echo "    System install: each user gets the HUD after running"
        echo "      gnome-extensions enable $UUID"
        echo "    (topmanagerd starts automatically through D-Bus activation)"
        return
    fi
    if have systemctl; then
        systemctl --user daemon-reload 2>/dev/null || true
        systemctl --user enable --now "$UNIT" 2>/dev/null \
            && say "topmanagerd service running" \
            || echo "    note: could not start $UNIT (no user session bus?); it will start on first use"
    fi
    enable_extension
}

cmd_install() {
    local here repo
    here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"
    repo="${REPO:-$(receipt_field repo)}"
    repo="${repo:-$DEFAULT_REPO}"

    # Running from an extracted release tarball, and no remote requested.
    if { [ "$LOCAL" = 1 ] || { [ -z "$REPO" ] && [ "$VERSION" = latest ]; }; } && [ -n "$here" ] && [ -x "$here/bin/topmanagerd" ]; then
        install_payload "$here" "$repo"
        return
    fi

    local tag="$VERSION"
    if [ "$tag" = latest ]; then
        tag="$(latest_tag "$repo")"
        [ -n "$tag" ] || die "no release found in $repo"
    fi
    DOWNLOAD_DIR="$(mktemp -d)"
    trap 'rm -rf "$DOWNLOAD_DIR"' EXIT
    fetch_release "$repo" "$tag" "$DOWNLOAD_DIR"
    # Let the downloaded release install itself: an update must use the new
    # version's file layout, not this (older) script's. Releases before 0.2.1
    # don't know --local, so those are installed by this script.
    if grep -q -- '--local) LOCAL=1' "$PAYLOAD/install.sh"; then
        local args=(install --local --repo "$repo" "--$MODE")
        [ "$ENABLE" = 1 ] || args+=(--no-enable)
        bash "$PAYLOAD/install.sh" "${args[@]}"
        return
    fi
    install_payload "$PAYLOAD" "$repo"
}

cmd_update() {
    local repo installed tag
    repo="${REPO:-$(receipt_field repo)}"
    repo="${repo:-$DEFAULT_REPO}"
    installed="$(receipt_field version)"
    tag="$(latest_tag "$repo")"
    [ -n "$tag" ] || die "no release found in $repo"
    if [ -n "$installed" ] && [ "${tag#v}" = "$installed" ]; then
        say "TopManager $installed is up to date ($repo)"
        return
    fi
    say "Updating ${installed:-unknown} → ${tag#v} from $repo"
    REPO="$repo" VERSION="$tag"
    cmd_install
}

cmd_uninstall() {
    say "Removing TopManager ($MODE)"
    user_systemctl disable --now "$UNIT" || true
    if [ "$MODE" = user ] && have gnome-extensions; then
        gnome-extensions disable "$UUID" 2>/dev/null || true
    fi
    rm -f "$BIN_DIR/topmanagerd" "$BIN_DIR/topmanager" "$UNIT_DIR/$UNIT" "$DBUS_DIR/$BUS_NAME.service" "$DBUS_DIR/$LEGACY_BUS_NAME.service" \
        "$APPS_DIR/$APP_ID.desktop"
    rm -f "$ICON_DIR"/*/apps/"$APP_ID.png" "$ICON_DIR/symbolic/apps/$APP_ID-symbolic.svg"
    rm -rf "$EXT_DIR"
    rm -f "$RECEIPT"
    rmdir "$RECEIPT_DIR" 2>/dev/null || true
    user_systemctl daemon-reload || true
    if [ "$PURGE" = 1 ]; then
        rm -rf "$(xdg_dir XDG_CONFIG_HOME .config)/topmanager" "$(xdg_dir XDG_STATE_HOME .local/state)/topmanager"
        say "Settings and history deleted"
    else
        echo "    Settings and history kept (use --purge to delete them)"
    fi
    rm -f "$BIN_DIR/topmanager-install"
}

cmd_status() {
    local repo installed latest
    repo="$(receipt_field repo)"
    installed="$(receipt_field version)"
    echo "mode:       $MODE"
    echo "installed:  ${installed:-not installed}"
    echo "repo:       ${repo:-—}"
    if [ -n "$repo" ]; then
        latest="$(latest_tag "$repo" 2>/dev/null || true)"
        echo "latest:     ${latest:-unknown}"
    fi
    if [ "$MODE" = user ] && have systemctl; then
        echo "service:    $(systemctl --user is-active "$UNIT" 2>/dev/null || true)"
    fi
    if have gnome-extensions; then
        echo "extension:  $(gnome-extensions info "$UUID" 2>/dev/null | sed -n 's/^ *State: *//p' || true)"
    fi
}

case "$CMD" in
    install) cmd_install ;;
    update) cmd_update ;;
    uninstall) cmd_uninstall ;;
    status) cmd_status ;;
esac

#!/usr/bin/env bash
# Run the TopManager HUD in a completely isolated, headless GNOME Shell:
# throwaway $HOME and XDG dirs, private session bus, virtual monitor. It
# installs the packaged release with install.sh (so it also proves the
# installer works for a different home), opens the HUD menu and screenshots it.
#
#   scripts/test-hud.sh [dist/topmanager-<version>-<arch>] [out-dir]
#
# Your real session, settings and extensions are never touched.
set -euo pipefail
cd "$(dirname "$0")/.."

PAYLOAD="${1:-$(ls -d dist/topmanager-*-"$(uname -m)" | tail -n1)}"
OUT="${2:-$(mktemp -d)}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
ROOT="$(mktemp -d)"

cleanup() {
    # Stop anything that inherited this test environment (only our own
    # processes can be matched, and only those carrying this unique $ROOT).
    local p
    for p in /proc/[0-9]*; do
        if grep -qsF "XDG_RUNTIME_DIR=$ROOT/run" "$p/environ" 2>/dev/null; then
            kill "${p#/proc/}" 2>/dev/null || true
        fi
    done
    sleep 1
    fusermount3 -uz "$ROOT/run/doc" 2>/dev/null || fusermount -uz "$ROOT/run/doc" 2>/dev/null || true
    rm -rf "$ROOT" 2>/dev/null || true
}
trap cleanup EXIT

# A private session bus that can only activate dconf: no portals, no
# evolution, no gvfs; nothing that could outlive the test.
mkdir -p "$ROOT/dbus-services"
cp /usr/share/dbus-1/services/ca.desrt.dconf.service "$ROOT/dbus-services/"
cat > "$ROOT/session.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:dir=$ROOT</listen>
  <auth>EXTERNAL</auth>
  <servicedir>$ROOT/dbus-services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
CONF

export HOME="$ROOT/home-of-tester"
export XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state" XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$ROOT/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS GNOME_SETUP_DISPLAY
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME
export TM_PROBE_OUT="$OUT"

"$PAYLOAD/install.sh" --user --no-enable
cp -r scripts/hud-probe@topmanager.test "$XDG_DATA_HOME/gnome-shell/extensions/"
mkdir -p "$XDG_CONFIG_HOME/topmanager"
printf '[general]\nrefresh_interval = 1.0\n[hud]\nmetric = "%s"\nshow_sparkline = true\n' \
    "${TM_HUD_METRIC:-cpu_memory}" > "$XDG_CONFIG_HOME/topmanager/config.toml"

dbus-run-session --config-file="$ROOT/session.conf" -- bash -c '
    set -u
    gsettings set org.gnome.shell enabled-extensions "[\"topmanager@hariel1985.github.io\", \"hud-probe@topmanager.test\"]"
    gsettings set org.gnome.shell disable-user-extensions false
    gsettings set org.gnome.shell welcome-dialog-last-shown-version "999"
    "$HOME/.local/bin/topmanagerd" run > "$TM_PROBE_OUT/topmanagerd.log" 2>&1 &
    DAEMON=$!
    gnome-shell --headless --no-x11 --virtual-monitor 1280x800 --sm-disable > "$TM_PROBE_OUT/gnome-shell.log" 2>&1 &
    SHELL_PID=$!
    for _ in $(seq 1 90); do
        [ -f "$TM_PROBE_OUT/done" ] && break
        kill -0 $SHELL_PID 2>/dev/null || break
        sleep 1
    done
    # The main window, page by page, inside the same headless shell.
    if [ -f "$TM_PROBE_OUT/done" ] && [ -x "$HOME/.local/bin/topmanager" ]; then
        shoot() {
            echo "$1" > "$TM_PROBE_OUT/request"
            for _ in $(seq 1 60); do [ -f "$TM_PROBE_OUT/request" ] || break; sleep 0.25; done
        }
        export WAYLAND_DISPLAY=wayland-0 GDK_BACKEND=wayland
        "$HOME/.local/bin/topmanager" --page processes > "$TM_PROBE_OUT/topmanager.log" 2>&1 &
        sleep 8
        shoot gui-processes
        for page in apps performance power; do
            "$HOME/.local/bin/topmanager" --page "$page" >> "$TM_PROBE_OUT/topmanager.log" 2>&1
            sleep 4
            shoot "gui-$page"
        done
        "$HOME/.local/bin/topmanager" --page processes --preferences >> "$TM_PROBE_OUT/topmanager.log" 2>&1
        sleep 3
        shoot gui-preferences
    fi
    gdbus call --session --dest io.github.hariel1985.TopManager.Daemon --object-path /io/github/hariel1985/TopManager \
        --method io.github.hariel1985.TopManager1.GetSettings > "$TM_PROBE_OUT/settings-via-dbus.txt" 2>&1 || true
    kill $SHELL_PID $DAEMON 2>/dev/null || true
    wait 2>/dev/null || true
'

grep -iE "topmanager|hud-probe|JS ERROR|Extension .* had error" "$OUT/gnome-shell.log" > "$OUT/extension-errors.log" || true
echo "results in $OUT:"
ls "$OUT"
[ -f "$OUT/done" ] && [ ! -f "$OUT/error.txt" ] || { cat "$OUT/error.txt" 2>/dev/null; cat "$OUT/extension-errors.log"; exit 1; }
cat "$OUT/state.json"

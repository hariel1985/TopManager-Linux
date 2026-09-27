#!/usr/bin/env bash
# Run by CI as a freshly created user with an unusual home directory:
# install the packaged release, exercise the service over D-Bus, export a
# backup, and uninstall. Usage: ci-portable-install.sh PAYLOAD_DIR BACKUP_FILE
set -euxo pipefail

call() {
    gdbus call --session --dest io.github.hariel1985.TopManager.Daemon \
        --object-path /io/github/hariel1985/TopManager --method "io.github.hariel1985.TopManager1.$1" "${@:2}"
}

# Second stage, inside a private session bus.
if [ "${1:-}" = --in-session ]; then
    ~/.local/bin/topmanagerd run &
    for _ in $(seq 1 30); do
        if call GetSummary 2>/dev/null | grep 'ready.:true' >/dev/null; then break; fi
        sleep 0.5
    done
    call GetSummary | grep 'ready.:true' >/dev/null
    # gdbus parses arguments as GVariant text: this is the string "health"
    # including its JSON quotes.
    call SetSetting hud.metric "'\"health\"'" | grep '(true,' >/dev/null
    call GetHistory 5m | grep samples >/dev/null
    kill %1
    wait || true
    exit 0
fi

PAYLOAD="$1"
BACKUP="$2"

"$PAYLOAD/install.sh" --user --no-enable
test -x ~/.local/bin/topmanagerd
test -x ~/.local/bin/topmanager
grep "^Exec=$HOME/.local/bin/topmanager$" ~/.local/share/applications/io.github.hariel1985.TopManager.desktop >/dev/null
test -f ~/.local/share/icons/hicolor/256x256/apps/io.github.hariel1985.TopManager.png
test -f ~/.local/share/gnome-shell/extensions/topmanager@hariel1985.github.io/extension.js
test -f ~/.local/share/dbus-1/services/io.github.hariel1985.TopManager.Daemon.service
grep '%h/.local/bin/topmanagerd' ~/.config/systemd/user/topmanagerd.service >/dev/null
~/.local/bin/topmanagerd summary

dbus-run-session -- bash "$0" --in-session

grep 'metric = "health"' ~/.config/topmanager/config.toml >/dev/null
~/.local/bin/topmanagerd export "$BACKUP" --with-history
~/.local/bin/topmanager-install uninstall --purge
test ! -e ~/.local/bin/topmanagerd
test ! -e ~/.local/bin/topmanager
test ! -e ~/.local/share/applications/io.github.hariel1985.TopManager.desktop
test ! -e ~/.config/topmanager

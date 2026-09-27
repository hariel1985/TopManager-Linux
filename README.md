# TopManager for Linux

A GNOME system monitor with a rich **top-bar HUD**: live CPU, memory, network,
GPU and battery with sparklines, a 0–100 **system health score** with a
plain-language diagnosis, proactive **alerts**, and one-click quit for the
processes eating your machine.

Linux/GNOME rewrite of [TopManager for macOS](https://github.com/hariel1985/TopManager),
in Rust, with a GTK 4 / libadwaita window:

- **Processes** — sortable, searchable table (CPU, memory, disk I/O, energy,
  threads, user, state), right-click menu, suspend/resume/end/force quit, and an
  inspector with path, command line, PSS/USS memory, open files.
- **Apps** — running desktop apps grouped from their systemd scopes, with switch
  to / minimize (through the HUD extension) and quit.
- **Performance** — CPU (per core, P/E cores), memory, network and GPU charts;
  live or 5 min – 24 h from the persistent history.
- **Power & Storage** — health score and diagnosis, alerts, battery, system,
  graphics, volumes, network interfaces.
- **Preferences** — refresh rate, notifications, start at login, HUD metric,
  alert thresholds, backup export/import.

Status: **0.x** — everything above works; packaging (.deb) and translations are next.

## Install

Per user, no root needed, any user name / home directory:

```bash
curl -fsSL https://raw.githubusercontent.com/hariel1985/TopManager-Linux/main/install.sh | bash -s -- --repo hariel1985/TopManager-Linux
```

`--repo OWNER/NAME` installs from any fork. If the [GitHub CLI](https://cli.github.com)
is logged in, it is used for downloads, so private repositories work too.
Other options: `--version v0.1.0`, `--system` (all users, into `/usr/local`, needs root).

Afterwards:

```bash
topmanager-install status      # installed vs. latest release
topmanager-install update      # upgrade from the repo it was installed from
topmanager-install uninstall   # add --purge to also delete settings and history
```

On Wayland, GNOME loads a newly installed extension at the next login, so log
out and back in once to see the HUD.

Requirements: GNOME Shell 46–49, systemd user session, x86_64 or aarch64.
The service binary is statically linked and runs on any distribution; the
window needs GTK ≥ 4.14 and libadwaita ≥ 1.5 (Debian 13, Ubuntu 24.04 and newer).

## Moving to another machine or user

Everything lives in the XDG directories of the current user; nothing refers to
a user name or a fixed path.

| What | Where |
|---|---|
| Settings | `~/.config/topmanager/config.toml` (plain TOML, safe to copy or edit) |
| History, alerts | `~/.local/state/topmanager/` |

```bash
topmanagerd export ~/topmanager-backup.json [--with-history]
topmanagerd import ~/topmanager-backup.json [--with-history]   # on the new machine
```

## Architecture

```
topmanagerd (Rust, systemd user service)  ── D-Bus ──▶  GNOME Shell extension (top-bar HUD)
  collectors: /proc, /sys, PSI, sysfs power      │           renders only; also a small bridge
  health score, alert engine, history,           │           (org.gnome.Shell …/TopManager/Shell)
  notifications                                  └──────▶  topmanager (GTK 4 / libadwaita window)
```

- `crates/tm-core` — platform-neutral logic ported from the macOS app
  (health score, alerts, history math, formatting).
- `crates/tm-collect` — Linux collectors (`/proc`, `/sys`), tested against fixture trees.
- `crates/tm-daemon` — `topmanagerd`: sampler, D-Bus service `io.github.hariel1985.TopManager.Daemon`
  (interface `io.github.hariel1985.TopManager1`), history (JSON lines), notifications, export/import.
- `crates/tm-gui` — `topmanager`, the main window (app id `io.github.hariel1985.TopManager`).
- `shell-extension/` — the top-bar HUD (GJS).

Useful commands: `topmanagerd summary` (human-readable snapshot),
`topmanagerd dump` (full JSON), `topmanagerd paths`.

## Development

```bash
cargo test                  # all unit tests
scripts/package.sh          # dist/topmanager-<version>-<arch>.tar.gz
scripts/test-hud.sh         # run the HUD and the window in an isolated headless GNOME Shell, screenshot both
```

Releases: `scripts/bump-version.sh X.Y.Z --push` sets the version everywhere,
tags `vX.Y.Z` and pushes; the release workflow builds x86_64 and aarch64
tarballs and publishes them with checksums on GitHub.

## License

GPL-3.0-or-later, like the original.

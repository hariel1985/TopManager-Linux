//! Preferences. Settings live in topmanagerd's config.toml and are changed
//! over D-Bus, so the window, the HUD and the service always agree.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};
use tm_core::settings::{HudMetric, Settings};

use crate::client::Client;
use crate::Toaster;

const UNIT: &str = "topmanagerd.service";
const HUD_UUID: &str = "topmanager@hariel1985.github.io";

fn setter(client: &Client, toast: &Toaster, key: &'static str) -> impl Fn(Value) + 'static {
    let (client, toast) = (client.clone(), toast.clone());
    move |value| {
        let (c, t) = (client.clone(), toast.clone());
        glib::spawn_future_local(async move {
            if let Err(e) = c.set_setting(key, value).await {
                t(&e);
            }
        });
    }
}

fn spin(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    step: f64,
    value: f64,
    on: impl Fn(f64) + 'static,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.set_value(value);
    row.connect_value_notify(move |r| on(r.value()));
    row
}

fn switch(title: &str, subtitle: &str, active: bool, on: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build();
    row.connect_active_notify(move |r| on(r.is_active()));
    row
}

async fn systemd(conn: &gio::DBusConnection, method: &str, args: glib::Variant) -> Result<glib::Variant, glib::Error> {
    conn.call_future(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        method,
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
    )
    .await
}

async fn shell_extensions(conn: &gio::DBusConnection, method: &str) -> Result<glib::Variant, glib::Error> {
    conn.call_future(
        Some("org.gnome.Shell.Extensions"),
        "/org/gnome/Shell/Extensions",
        "org.gnome.Shell.Extensions",
        method,
        Some(&(HUD_UUID,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
    )
    .await
}

fn action_row(title: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).activatable(true).build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// topmanagerd sits next to this binary after installation; fall back to PATH.
fn daemon_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("topmanagerd")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("topmanagerd"))
}

fn run_daemon_command(args: Vec<String>, toast: Toaster) {
    let mut argv = vec![daemon_binary().into_os_string()];
    argv.extend(args.into_iter().map(Into::into));
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE) {
        Ok(p) => {
            glib::spawn_future_local(async move {
                match p.communicate_utf8_future(None).await {
                    Ok((out, _)) => {
                        let msg = out.map(|s| s.lines().next().unwrap_or("").to_string()).unwrap_or_default();
                        toast(if msg.is_empty() { "Done" } else { &msg });
                    }
                    Err(e) => toast(&e.to_string()),
                }
            });
        }
        Err(e) => toast(&format!("Could not run topmanagerd: {e}")),
    }
}

pub fn show(win: &adw::ApplicationWindow, client: Client, toast: Toaster) {
    let win = win.clone();
    glib::spawn_future_local(async move {
        let s = match client.settings().await {
            Ok(s) => s,
            Err(e) => {
                toast(&format!("Settings unavailable: {e}"));
                return;
            }
        };
        build(&win, &client, &toast, &s).await.present(Some(&win));
    });
}

async fn build(win: &adw::ApplicationWindow, client: &Client, toast: &Toaster, s: &Settings) -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::new();
    let conn = client.connection().clone();

    // ---------------------------------------------------------------- general
    let general = adw::PreferencesPage::builder().title("General").icon_name("preferences-system-symbolic").build();
    let monitoring = adw::PreferencesGroup::builder().title("Monitoring").build();
    let set = setter(client, toast, "general.refresh_interval");
    monitoring.add(&spin(
        "Refresh interval",
        "Seconds between samples",
        1.0,
        30.0,
        1.0,
        s.general.refresh_interval,
        move |v| set(json!(v)),
    ));
    let set = setter(client, toast, "general.notifications");
    monitoring.add(&switch(
        "Notifications",
        "Desktop notifications when an alert starts",
        s.general.notifications,
        move |v| set(json!(v)),
    ));
    let set = setter(client, toast, "history.max_age_hours");
    monitoring.add(&spin(
        "Keep history",
        "Hours of metric history kept on disk",
        1.0,
        744.0,
        1.0,
        s.history.max_age_hours,
        move |v| set(json!(v)),
    ));
    general.add(&monitoring);

    let startup = adw::PreferencesGroup::builder().title("Startup").build();
    let enabled = match systemd(&conn, "GetUnitFileState", (UNIT,).to_variant()).await {
        Ok(v) => v.get::<(String,)>().map(|(s,)| s == "enabled").unwrap_or(false),
        Err(_) => false,
    };
    let (c2, t2) = (conn.clone(), toast.clone());
    let login = switch(
        "Start at login",
        "Run the TopManager service when you log in, so alerts and history work without the window",
        enabled,
        move |on| {
            let (c, t) = (c2.clone(), t2.clone());
            glib::spawn_future_local(async move {
                let r = if on {
                    systemd(&c, "EnableUnitFiles", (vec![UNIT], false, true).to_variant()).await
                } else {
                    systemd(&c, "DisableUnitFiles", (vec![UNIT], false).to_variant()).await
                };
                match r {
                    Ok(_) => {
                        let _ = systemd(&c, "Reload", ().to_variant()).await;
                    }
                    Err(e) => t(&format!("Could not change the login item: {e}")),
                }
            });
        },
    );
    startup.add(&login);
    general.add(&startup);
    dialog.add(&general);

    // ---------------------------------------------------------------- top bar
    let hud = adw::PreferencesPage::builder().title("Top Bar").icon_name("view-reveal-symbolic").build();
    let ext = adw::PreferencesGroup::builder().title("HUD").build();
    let state = shell_extensions(&conn, "GetExtensionInfo")
        .await
        .ok()
        .and_then(|v| glib::VariantDict::new(Some(&v.child_value(0))).lookup_value("state", None))
        .and_then(|v| v.get::<f64>());
    // Extension states: 1 = enabled/active, 2 = disabled (see gnome-shell).
    let hud_row = match state {
        Some(st) => {
            let (c3, t3) = (conn.clone(), toast.clone());
            switch("Show the HUD in the top bar", "The TopManager GNOME Shell extension", st == 1.0, move |on| {
                let (c, t) = (c3.clone(), t3.clone());
                glib::spawn_future_local(async move {
                    let m = if on { "EnableExtension" } else { "DisableExtension" };
                    if let Err(e) = shell_extensions(&c, m).await {
                        t(&e.to_string());
                    }
                });
            })
        }
        None => adw::SwitchRow::builder()
            .title("Show the HUD in the top bar")
            .subtitle("The extension is installed but GNOME Shell loads it at the next login")
            .sensitive(false)
            .build(),
    };
    ext.add(&hud_row);

    let metrics = [
        (HudMetric::Cpu, "cpu", "CPU %"),
        (HudMetric::Memory, "memory", "Memory %"),
        (HudMetric::CpuMemory, "cpu_memory", "CPU and memory"),
        (HudMetric::Health, "health", "Health score"),
        (HudMetric::Download, "download", "Download speed"),
    ];
    let names: Vec<&str> = metrics.iter().map(|m| m.2).collect();
    let combo = adw::ComboRow::builder().title("Show in the top bar").model(&gtk::StringList::new(&names)).build();
    combo.set_selected(metrics.iter().position(|m| m.0 == s.hud.metric).unwrap_or(0) as u32);
    let set = setter(client, toast, "hud.metric");
    combo.connect_selected_notify(move |c| {
        if let Some(m) = metrics.get(c.selected() as usize) {
            set(json!(m.1));
        }
    });
    ext.add(&combo);
    let set = setter(client, toast, "hud.show_sparkline");
    ext.add(&switch("Sparkline", "A small chart next to the value", s.hud.show_sparkline, move |v| set(json!(v))));
    let set = setter(client, toast, "hud.top_count");
    ext.add(&spin("Processes in the menu", "", 1.0, 10.0, 1.0, s.hud.top_count as f64, move |v| set(json!(v as u8))));
    hud.add(&ext);
    dialog.add(&hud);

    // ---------------------------------------------------------------- alerts
    let alerts = adw::PreferencesPage::builder().title("Alerts").icon_name("dialog-warning-symbolic").build();
    let th = adw::PreferencesGroup::builder()
        .title("Thresholds")
        .description("An alert starts when a condition is reached (sustained ones must last three samples) and fires once until it recovers.")
        .build();
    let set = setter(client, toast, "alerts.cpu_percent");
    th.add(&spin("High CPU", "Whole-system usage, %", 10.0, 100.0, 5.0, s.alerts.cpu_percent, move |v| set(json!(v))));
    let set = setter(client, toast, "alerts.process_cpu_percent");
    th.add(&spin(
        "Runaway process",
        "One process, % of a core (200 = two cores)",
        50.0,
        1600.0,
        10.0,
        s.alerts.process_cpu_percent,
        move |v| set(json!(v)),
    ));
    let set = setter(client, toast, "alerts.disk_used_fraction");
    th.add(&spin("Disk almost full", "%", 50.0, 100.0, 1.0, (s.alerts.disk_used_fraction * 100.0).round(), move |v| {
        set(json!(v / 100.0))
    }));
    let set = setter(client, toast, "alerts.low_battery_percent");
    th.add(&spin("Low battery", "%", 0.0, 100.0, 5.0, s.alerts.low_battery_percent as f64, move |v| {
        set(json!(v as u8))
    }));
    let set = setter(client, toast, "alerts.memory_pressure_critical");
    th.add(&switch("Critical memory pressure", "", s.alerts.memory_pressure_critical, move |v| set(json!(v))));
    let set = setter(client, toast, "alerts.thermal_serious");
    th.add(&switch("Overheating", "", s.alerts.thermal_serious, move |v| set(json!(v))));
    alerts.add(&th);
    dialog.add(&alerts);

    // ---------------------------------------------------------------- data
    let data = adw::PreferencesPage::builder().title("Backup").icon_name("document-save-symbolic").build();
    let group = adw::PreferencesGroup::builder()
        .title("Move to Another Computer")
        .description("A backup holds your settings and alerts (and optionally the metric history). It contains no user names or paths, so it can be restored on another machine or account.")
        .build();
    let history = adw::SwitchRow::builder().title("Include metric history").build();
    group.add(&history);

    let export = action_row("Export…", "document-save-symbolic");
    let (w, t, h) = (win.clone(), toast.clone(), history.clone());
    export.connect_activated(move |_| {
        let fd =
            gtk::FileDialog::builder().title("Export TopManager Backup").initial_name("topmanager-backup.json").build();
        let (t, with) = (t.clone(), h.is_active());
        fd.save(Some(&w), gio::Cancellable::NONE, move |r| {
            if let Ok(Some(path)) = r.map(|f| f.path()) {
                let mut args = vec!["export".into(), path.to_string_lossy().into_owned()];
                if with {
                    args.push("--with-history".into());
                }
                run_daemon_command(args, t.clone());
            }
        });
    });
    group.add(&export);

    let import = action_row("Import…", "document-open-symbolic");
    let (w, t, h) = (win.clone(), toast.clone(), history.clone());
    import.connect_activated(move |_| {
        let fd = gtk::FileDialog::builder().title("Import TopManager Backup").build();
        let (t, with) = (t.clone(), h.is_active());
        fd.open(Some(&w), gio::Cancellable::NONE, move |r| {
            if let Ok(Some(path)) = r.map(|f| f.path()) {
                let mut args = vec!["import".into(), path.to_string_lossy().into_owned()];
                if with {
                    args.push("--with-history".into());
                }
                run_daemon_command(args, t.clone());
            }
        });
    });
    group.add(&import);
    data.add(&group);
    dialog.add(&data);

    dialog
}

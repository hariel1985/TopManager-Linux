//! Power & Storage: health score and diagnosis, alerts, battery, system,
//! graphics, storage volumes and network interfaces.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use serde_json::Value;
use tm_core::alert::{AlertSeverity, SystemAlert};
use tm_core::battery::BatteryMath;
use tm_core::format::{format_bytes, format_uptime};
use tm_core::health::{self, ThermalLevel};
use tm_core::model::Snapshot;

use crate::client::Client;
use crate::Toaster;

/// A preferences group whose rows are updated in place by key, so the page
/// doesn't flicker or jump while it refreshes every few seconds.
struct KeyedGroup {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<(String, adw::ActionRow, gtk::Label)>>,
}

impl KeyedGroup {
    fn new(title: &str) -> Self {
        KeyedGroup { group: adw::PreferencesGroup::builder().title(title).build(), rows: RefCell::default() }
    }

    /// `(key, title, subtitle, value)`
    fn set(&self, items: Vec<(String, String, String, String)>) {
        let mut rows = self.rows.borrow_mut();
        rows.retain(|(k, row, _)| {
            let keep = items.iter().any(|i| &i.0 == k);
            if !keep {
                self.group.remove(row);
            }
            keep
        });
        for (key, title, subtitle, value) in items {
            match rows.iter().find(|(k, _, _)| *k == key) {
                Some((_, row, label)) => {
                    if row.title() != title {
                        row.set_title(&glib::markup_escape_text(&title));
                    }
                    if row.subtitle().as_deref().unwrap_or("") != subtitle {
                        row.set_subtitle(&glib::markup_escape_text(&subtitle));
                    }
                    if label.label() != value {
                        label.set_label(&value);
                    }
                }
                None => {
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&title))
                        .subtitle(glib::markup_escape_text(&subtitle))
                        .build();
                    let label = gtk::Label::builder()
                        .label(&value)
                        .selectable(true)
                        .xalign(1.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .css_classes(["numeric", "dim-label"])
                        .build();
                    row.add_suffix(&label);
                    self.group.add(&row);
                    rows.push((key, row, label));
                }
            }
        }
    }
}

fn kv(key: &str, value: impl Into<String>) -> (String, String, String, String) {
    (key.to_string(), key.to_string(), String::new(), value.into())
}

struct VolumeRow {
    row: adw::ActionRow,
    bar: gtk::LevelBar,
    label: gtk::Label,
}

pub struct PowerPage {
    pub root: adw::PreferencesPage,
    client: Client,
    toast: Toaster,
    score: gtk::Label,
    rating: gtk::Label,
    diagnosis: gtk::Label,
    alerts_group: adw::PreferencesGroup,
    alert_rows: RefCell<Vec<adw::ActionRow>>,
    alerts_at: Cell<f64>,
    battery: KeyedGroup,
    system: KeyedGroup,
    graphics: KeyedGroup,
    storage: adw::PreferencesGroup,
    volumes: RefCell<HashMap<String, VolumeRow>>,
    network: KeyedGroup,
}

impl PowerPage {
    pub fn new(client: Client, toast: Toaster) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        let score = gtk::Label::builder().label("—").css_classes(["health-score"]).build();
        let rating = gtk::Label::builder().xalign(0.0).css_classes(["title-2"]).build();
        let diagnosis = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        let text =
            gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).valign(gtk::Align::Center).build();
        text.append(&rating);
        text.append(&diagnosis);
        let hb =
            gtk::Box::builder().spacing(24).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
        hb.append(&score);
        hb.append(&text);
        let health_card = gtk::Box::builder().css_classes(["card"]).build();
        health_card.append(&hb);
        let health = adw::PreferencesGroup::builder().title("System Health").build();
        health.add(&health_card);
        root.add(&health);

        let clear = gtk::Button::builder().label("Clear").valign(gtk::Align::Center).css_classes(["flat"]).build();
        let alerts_group = adw::PreferencesGroup::builder().title("Recent Alerts").header_suffix(&clear).build();
        root.add(&alerts_group);

        let battery = KeyedGroup::new("Battery &amp; Power");
        let system = KeyedGroup::new("System");
        let graphics = KeyedGroup::new("Graphics");
        let storage = adw::PreferencesGroup::builder().title("Storage").build();
        let network = KeyedGroup::new("Network Interfaces");
        root.add(&battery.group);
        root.add(&system.group);
        root.add(&graphics.group);
        root.add(&storage);
        root.add(&network.group);

        let page = Rc::new(PowerPage {
            root,
            client,
            toast,
            score,
            rating,
            diagnosis,
            alerts_group,
            alert_rows: RefCell::default(),
            alerts_at: Cell::new(0.0),
            battery,
            system,
            graphics,
            storage,
            volumes: RefCell::default(),
            network,
        });
        let w = Rc::downgrade(&page);
        clear.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                let (c, t, w2) = (p.client.clone(), p.toast.clone(), Rc::downgrade(&p));
                glib::spawn_future_local(async move {
                    match c.clear_alerts().await {
                        Ok(()) => {
                            if let Some(p) = w2.upgrade() {
                                p.show_alerts(&[]);
                            }
                        }
                        Err(e) => t(&e),
                    }
                });
            }
        });
        page
    }

    pub fn update(self: &Rc<Self>, s: &Snapshot, summary: Option<&Value>) {
        // Health: computed by the daemon (the same numbers as the HUD).
        if let Some(h) = summary.and_then(|v| v.get("health")) {
            let score = h["score"].as_u64().unwrap_or(0) as u8;
            self.score.set_label(&score.to_string());
            for c in ["health-excellent", "health-good", "health-fair", "health-poor"] {
                self.score.remove_css_class(c);
            }
            self.score.add_css_class(match score {
                85.. => "health-excellent",
                70..=84 => "health-good",
                50..=69 => "health-fair",
                _ => "health-poor",
            });
            self.rating.set_label(health::rating(score));
            let diag: Vec<&str> =
                h["diagnosis"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            self.diagnosis.set_label(&if diag.is_empty() {
                "Everything looks good.".to_string()
            } else {
                diag.join("\n")
            });
        }

        if s.timestamp - self.alerts_at.get() > 10.0 {
            self.alerts_at.set(s.timestamp);
            let (c, w) = (self.client.clone(), Rc::downgrade(self));
            glib::spawn_future_local(async move {
                if let (Ok(a), Some(p)) = (c.alerts().await, w.upgrade()) {
                    p.show_alerts(&a);
                }
            });
        }

        let p = &s.power;
        if p.has_battery {
            let state = if p.fully_charged {
                "Fully charged"
            } else if p.is_charging {
                "Charging"
            } else if p.is_plugged_in {
                "Plugged in, not charging"
            } else {
                "On battery"
            };
            let remaining = match (p.time_to_empty_min, p.time_to_full_min) {
                (Some(m), _) => format!("{} until empty", BatteryMath::format_minutes(m as i64)),
                (_, Some(m)) => format!("{} until full", BatteryMath::format_minutes(m as i64)),
                _ => "—".into(),
            };
            let mut items = vec![
                kv("Charge", format!("{}%", p.charge_percent)),
                kv("State", state),
                kv("Time remaining", remaining),
                kv(
                    "Health",
                    p.health_percent()
                        .map(|h| format!("{h:.0}% of design capacity · {}", p.condition().label()))
                        .unwrap_or_else(|| "—".into()),
                ),
                kv("Charge cycles", p.cycle_count.map(|c| c.to_string()).unwrap_or_else(|| "—".into())),
                kv("Power", p.power_watts.map(|w| format!("{w:+.1} W")).unwrap_or_else(|| "—".into())),
            ];
            if let Some(t) = p.temperature {
                items.push(kv("Temperature", format!("{t:.1} °C")));
            }
            if let Some(m) = &p.model {
                items.push(kv("Model", m.clone()));
            }
            self.battery.set(items);
        } else {
            self.battery.set(vec![kv("Power source", "AC power (no battery)")]);
        }

        let sys = &s.system;
        let thermal = match s.thermal.level {
            ThermalLevel::Nominal => "Normal",
            ThermalLevel::Fair => "Warm",
            ThermalLevel::Serious => "Hot — may be throttling",
            ThermalLevel::Critical => "Critical",
        };
        let temp = match (&s.thermal.max_temp, &s.thermal.sensor) {
            (Some(t), Some(sensor)) => format!("{thermal} · {t:.0} °C ({sensor})"),
            (Some(t), None) => format!("{thermal} · {t:.0} °C"),
            _ => format!("{thermal} (no sensors)"),
        };
        self.system.set(vec![
            kv("Operating system", sys.os_name.clone()),
            kv("Kernel", sys.kernel.clone()),
            kv("Computer name", sys.hostname.clone()),
            kv("Processor", format!("{} · {} cores · {}", sys.cpu_model, sys.cpu_count, sys.architecture)),
            kv("Uptime", format_uptime(sys.uptime_secs)),
            kv("Thermal state", temp),
            kv("Desktop", sys.desktop.clone().unwrap_or_else(|| "—".into())),
        ]);

        self.graphics.set(if s.gpus.is_empty() {
            vec![kv("GPU", "none detected")]
        } else {
            s.gpus
                .iter()
                .enumerate()
                .map(|(i, g)| {
                    let mem = match (g.vram_used, g.vram_total) {
                        (Some(u), Some(t)) => format!("{} / {} VRAM", format_bytes(u), format_bytes(t)),
                        _ if g.is_unified_memory => "shared system memory".into(),
                        _ => String::new(),
                    };
                    let util = g.utilization.map(|u| format!("{u:.0}% busy")).unwrap_or_default();
                    let value = [util, mem].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                    (
                        format!("gpu{i}"),
                        g.name.clone(),
                        g.driver.clone(),
                        if value.is_empty() { "—".into() } else { value },
                    )
                })
                .collect()
        });

        self.update_volumes(s);

        self.network.set(
            s.network
                .interfaces
                .iter()
                .map(|i| {
                    let state = if i.is_up { "up" } else { "down" };
                    let kind = if i.is_virtual { " · virtual" } else { "" };
                    (
                        i.name.clone(),
                        i.name.clone(),
                        format!("{state}{kind}"),
                        format!("↓ {}  ↑ {}", format_bytes(i.rx_bytes), format_bytes(i.tx_bytes)),
                    )
                })
                .collect(),
        );
    }

    fn update_volumes(&self, s: &Snapshot) {
        let mut vols = self.volumes.borrow_mut();
        vols.retain(|k, v| {
            let keep = s.disk.volumes.iter().any(|x| &x.mount_point == k);
            if !keep {
                self.storage.remove(&v.row);
            }
            keep
        });
        for v in &s.disk.volumes {
            let r = vols.entry(v.mount_point.clone()).or_insert_with(|| {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&v.name))
                    .subtitle(glib::markup_escape_text(&format!(
                        "{} · {} · {}",
                        v.mount_point, v.file_system, v.device
                    )))
                    .build();
                let icon = if v.is_removable { "drive-removable-media-symbolic" } else { "drive-harddisk-symbolic" };
                row.add_prefix(&gtk::Image::from_icon_name(icon));
                let bar = gtk::LevelBar::builder().width_request(160).valign(gtk::Align::Center).build();
                // The default offsets color *low* values as a warning, which
                // is backwards for disks; fullness is shown on the label.
                for o in [gtk::LEVEL_BAR_OFFSET_LOW, gtk::LEVEL_BAR_OFFSET_HIGH, gtk::LEVEL_BAR_OFFSET_FULL] {
                    bar.remove_offset_value(Some(o));
                }
                let label =
                    gtk::Label::builder().width_chars(18).xalign(1.0).css_classes(["numeric", "dim-label"]).build();
                row.add_suffix(&label);
                row.add_suffix(&bar);
                self.storage.add(&row);
                VolumeRow { row, bar, label }
            });
            let frac = v.usage_percentage() / 100.0;
            r.bar.set_value(frac);
            for c in ["warning", "error"] {
                r.label.remove_css_class(c);
            }
            if frac >= 0.90 {
                r.label.add_css_class(if frac >= 0.95 { "error" } else { "warning" });
            }
            r.label.set_label(&format!("{} free of {}", format_bytes(v.free), format_bytes(v.total)));
        }
    }

    fn show_alerts(&self, alerts: &[SystemAlert]) {
        for r in self.alert_rows.borrow_mut().drain(..) {
            self.alerts_group.remove(&r);
        }
        let mut rows = self.alert_rows.borrow_mut();
        if alerts.is_empty() {
            let r = adw::ActionRow::builder()
                .title("No alerts")
                .subtitle("TopManager warns you here and with a notification when something needs attention.")
                .build();
            self.alerts_group.add(&r);
            rows.push(r);
            return;
        }
        for a in alerts.iter().take(10) {
            let when = glib::DateTime::from_unix_local(a.timestamp as i64)
                .and_then(|d| d.format("%b %e, %H:%M"))
                .map(|s| s.to_string())
                .unwrap_or_default();
            let r = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&a.title))
                .subtitle(glib::markup_escape_text(&a.message))
                .build();
            let icon = match a.severity {
                AlertSeverity::Critical => "dialog-error-symbolic",
                AlertSeverity::Warning => "dialog-warning-symbolic",
                AlertSeverity::Info => "dialog-information-symbolic",
            };
            let img = gtk::Image::from_icon_name(icon);
            img.add_css_class(match a.severity {
                AlertSeverity::Critical => "error",
                AlertSeverity::Warning => "warning",
                AlertSeverity::Info => "accent",
            });
            r.add_prefix(&img);
            r.add_suffix(&gtk::Label::builder().label(&when).css_classes(["dim-label", "caption"]).build());
            self.alerts_group.add(&r);
            rows.push(r);
        }
    }
}

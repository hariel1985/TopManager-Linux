//! Portable export/import of settings, alert inbox and (optionally) history.
//!
//! One self-describing JSON file. It holds no absolute paths and no user
//! names, so it can be restored on another machine under another account;
//! on import everything lands in *that* user's XDG directories.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tm_core::alert::SystemAlert;
use tm_core::metrics::MetricsSample;
use tm_core::settings::Settings;

use crate::paths::{write_atomic, Paths};
use crate::store::{self, HistoryStore, SettingsStore};

const FORMAT: &str = "topmanager-backup";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub format_version: u32,
    pub app_version: String,
    pub created: f64,
    /// Informational only: where the backup came from.
    pub source_host: String,
    /// The config file verbatim, so comments and ordering survive.
    pub config_toml: String,
    #[serde(default)]
    pub alerts: Vec<SystemAlert>,
    #[serde(default)]
    pub history: Option<Vec<MetricsSample>>,
}

pub fn export(paths: &Paths, out: &Path, with_history: bool) -> Result<String, String> {
    let config_toml = fs::read_to_string(paths.config_file()).unwrap_or_else(|_| Settings::default().to_toml());
    let now = tm_collect::host::now_unix();
    let history = with_history.then(|| HistoryStore::open(paths, f64::MAX, now).all());
    let backup = Backup {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created: now,
        source_host: tm_collect::host::read_trimmed("/proc/sys/kernel/hostname").unwrap_or_default(),
        config_toml,
        alerts: store::load_alerts(paths),
        history,
    };
    let json = serde_json::to_vec_pretty(&backup).map_err(|e| e.to_string())?;
    write_atomic(out, &json).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    Ok(format!(
        "exported settings, {} alerts{} to {}",
        backup.alerts.len(),
        backup.history.as_ref().map(|h| format!(", {} history samples", h.len())).unwrap_or_default(),
        out.display()
    ))
}

pub fn import(paths: &Paths, file: &Path, with_history: bool) -> Result<String, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let b: Backup = serde_json::from_str(&text).map_err(|e| format!("not a TopManager backup: {e}"))?;
    if b.format != FORMAT {
        return Err("not a TopManager backup".into());
    }
    if b.format_version > FORMAT_VERSION {
        return Err(format!("backup format {} is newer than this TopManager supports", b.format_version));
    }
    // Validate before touching anything.
    Settings::from_toml(&b.config_toml).map_err(|e| format!("backup contains invalid settings: {e}"))?;

    write_atomic(&paths.config_file(), b.config_toml.as_bytes()).map_err(|e| e.to_string())?;
    store::save_alerts(paths, &b.alerts);
    let mut msg = format!("imported settings and {} alerts from '{}'", b.alerts.len(), b.source_host);
    if with_history {
        if let Some(h) = b.history {
            let settings = SettingsStore::new(paths).load();
            let mut store =
                HistoryStore::open(paths, settings.history.max_age_hours * 3600.0, tm_collect::host::now_unix());
            msg.push_str(&format!(", {} history samples", h.len()));
            store.replace(h);
        }
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_between_two_homes() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (pa, pb) = (Paths::under(a.path()), Paths::under(b.path()));
        write_atomic(&pa.config_file(), b"# mine\n[hud]\nmetric = \"health\"\n").unwrap();

        let file = a.path().join("backup.json");
        export(&pa, &file, true).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&a.path().to_string_lossy().to_string()), "no absolute paths in the backup");

        import(&pb, &file, true).unwrap();
        let restored = fs::read_to_string(pb.config_file()).unwrap();
        assert!(restored.starts_with("# mine"), "config kept verbatim");
        assert_eq!(SettingsStore::new(&pb).load().hud.metric, tm_core::settings::HudMetric::Health);
    }

    #[test]
    fn rejects_foreign_files() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.json");
        fs::write(&f, "{\"hello\":1}").unwrap();
        assert!(import(&Paths::under(d.path()), &f, false).is_err());
    }
}

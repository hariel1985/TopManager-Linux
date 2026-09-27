//! On-disk persistence: settings (TOML), alert inbox (JSON) and metric
//! history (JSON lines, appended each tick and compacted occasionally).
//!
//! JSON lines instead of the macOS app's single JSON document: appending one
//! ~120-byte line per tick replaces re-encoding the whole history on save.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use tm_core::alert::SystemAlert;
use tm_core::metrics::{self, HistoryRange, MetricsSample};
use tm_core::settings::Settings;

use crate::paths::{write_atomic, Paths};

pub struct SettingsStore {
    path: PathBuf,
    mtime: Option<SystemTime>,
}

impl SettingsStore {
    pub fn new(paths: &Paths) -> Self {
        Self { path: paths.config_file(), mtime: None }
    }

    fn current_mtime(&self) -> Option<SystemTime> {
        fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    /// Load, creating the file with defaults on first run so it is easy to
    /// find and edit by hand.
    pub fn load(&mut self) -> Settings {
        let settings = match fs::read_to_string(&self.path) {
            Ok(text) => Settings::from_toml(&text).unwrap_or_else(|e| {
                eprintln!("topmanagerd: {} is invalid, using defaults: {e}", self.path.display());
                Settings::default()
            }),
            Err(_) => {
                let s = Settings::default();
                let _ = self.save(&s);
                s
            }
        };
        self.mtime = self.current_mtime();
        settings
    }

    pub fn save(&mut self, settings: &Settings) -> std::io::Result<()> {
        write_atomic(&self.path, settings.to_toml().as_bytes())?;
        self.mtime = self.current_mtime();
        Ok(())
    }

    /// True when someone edited the file since we last loaded or saved it.
    pub fn changed_on_disk(&self) -> bool {
        self.current_mtime() != self.mtime
    }
}

pub fn load_alerts(paths: &Paths) -> Vec<SystemAlert> {
    fs::read_to_string(paths.alerts_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_alerts(paths: &Paths, alerts: &[SystemAlert]) {
    if let Ok(json) = serde_json::to_vec(alerts) {
        let _ = write_atomic(&paths.alerts_file(), &json);
    }
}

pub struct HistoryStore {
    path: PathBuf,
    samples: VecDeque<MetricsSample>,
    max_age: f64,
    lines_on_disk: usize,
}

impl HistoryStore {
    pub fn open(paths: &Paths, max_age: f64, now: f64) -> Self {
        let path = paths.history_file();
        let text = fs::read_to_string(&path).unwrap_or_default();
        let parsed: Vec<MetricsSample> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let lines = text.lines().count();
        let kept = metrics::trim(&parsed, now, max_age, usize::MAX);
        let mut store = Self { path, samples: kept.into(), max_age, lines_on_disk: lines };
        if store.lines_on_disk > store.samples.len() {
            store.compact();
        }
        store
    }

    pub fn set_max_age(&mut self, max_age: f64) {
        self.max_age = max_age;
    }

    pub fn record(&mut self, sample: MetricsSample) {
        self.samples.push_back(sample);
        let cutoff = sample.t - self.max_age;
        while self.samples.front().is_some_and(|s| s.t < cutoff) {
            self.samples.pop_front();
        }
        if let Ok(line) = serde_json::to_string(&sample) {
            let appended = fs::create_dir_all(self.path.parent().unwrap_or(&self.path))
                .and_then(|_| OpenOptions::new().create(true).append(true).open(&self.path))
                .and_then(|mut f| writeln!(f, "{line}"));
            if appended.is_ok() {
                self.lines_on_disk += 1;
            }
        }
        // Rewrite once a quarter of the file is expired samples.
        if self.lines_on_disk > self.samples.len() + self.samples.len() / 4 + 100 {
            self.compact();
        }
    }

    fn compact(&mut self) {
        let mut out = String::with_capacity(self.samples.len() * 128);
        for s in &self.samples {
            if let Ok(line) = serde_json::to_string(s) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if write_atomic(&self.path, out.as_bytes()).is_ok() {
            self.lines_on_disk = self.samples.len();
        }
    }

    pub fn query(&self, range: HistoryRange, now: f64) -> Vec<MetricsSample> {
        let Some(secs) = range.seconds() else { return Vec::new() };
        let since = now - secs;
        let window: Vec<MetricsSample> = self.samples.iter().filter(|s| s.t >= since).copied().collect();
        metrics::downsample(&window, range.bucket())
    }

    pub fn all(&self) -> Vec<MetricsSample> {
        self.samples.iter().copied().collect()
    }

    /// Replace everything (import).
    pub fn replace(&mut self, samples: Vec<MetricsSample>) {
        self.samples = samples.into();
        self.compact();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64) -> MetricsSample {
        MetricsSample { t, cpu: t, mem_used: 1, mem_total: 2, net_down: 0.0, net_up: 0.0, health: Some(100) }
    }

    #[test]
    fn history_persists_trims_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        {
            let mut h = HistoryStore::open(&paths, 100.0, 0.0);
            for t in 0..300 {
                h.record(s(t as f64));
            }
            assert_eq!(h.len(), 101, "keeps samples within max_age of the newest");
        }
        let lines = fs::read_to_string(paths.history_file()).unwrap().lines().count();
        assert!(lines < 300, "compaction ran ({lines} lines)");

        let h = HistoryStore::open(&paths, 100.0, 299.0);
        assert_eq!(h.len(), 101);
        let q = h.query(HistoryRange::M5, 299.0);
        assert_eq!(q.len(), 101);
        assert_eq!(fs::read_to_string(paths.history_file()).unwrap().lines().count(), 101);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        fs::create_dir_all(&paths.state_dir).unwrap();
        let good = serde_json::to_string(&s(10.0)).unwrap();
        fs::write(paths.history_file(), format!("{good}\n{{truncated\n")).unwrap();
        assert_eq!(HistoryStore::open(&paths, 100.0, 20.0).len(), 1);
    }

    #[test]
    fn settings_created_and_change_detected() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let mut store = SettingsStore::new(&paths);
        assert_eq!(store.load(), Settings::default());
        assert!(paths.config_file().exists());
        assert!(!store.changed_on_disk());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(paths.config_file(), "[hud]\nmetric = \"health\"\n").unwrap();
        assert!(store.changed_on_disk());
        assert_eq!(store.load().hud.metric, tm_core::settings::HudMetric::Health);
    }
}

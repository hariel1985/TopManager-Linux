//! User settings, persisted as `$XDG_CONFIG_HOME/topmanager/config.toml`.
//!
//! The file is the single, portable source of truth: copy it to another
//! machine or user and it just works. It must never contain absolute paths or
//! user names. Every field has a default, so older or hand-trimmed files load.

use serde::{Deserialize, Serialize};

use crate::alert::AlertThresholds;

/// What the top-bar label shows at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HudMetric {
    #[default]
    Cpu,
    Memory,
    Health,
    Download,
    /// "CPU 12% · RAM 43%"
    CpuMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TopSort {
    #[default]
    Cpu,
    Memory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// Seconds between samples, clamped to 1–30.
    pub refresh_interval: f64,
    pub notifications: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self { refresh_interval: 3.0, notifications: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HudSettings {
    pub metric: HudMetric,
    pub show_sparkline: bool,
    pub top_sort: TopSort,
    pub top_count: u8,
}

impl Default for HudSettings {
    fn default() -> Self {
        Self { metric: HudMetric::Cpu, show_sparkline: false, top_sort: TopSort::Cpu, top_count: 3 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub max_age_hours: f64,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self { max_age_hours: 24.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub general: GeneralSettings,
    pub hud: HudSettings,
    pub alerts: AlertThresholds,
    pub history: HistorySettings,
}

pub fn clamp_interval(value: f64) -> f64 {
    if value.is_nan() {
        return GeneralSettings::default().refresh_interval;
    }
    value.clamp(1.0, 30.0)
}

impl Settings {
    /// Parse TOML leniently: unknown keys are ignored, missing ones defaulted,
    /// out-of-range values clamped.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        let mut s: Settings = toml::from_str(text)?;
        s.sanitize();
        Ok(s)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("settings always serialize")
    }

    pub fn sanitize(&mut self) {
        self.general.refresh_interval = clamp_interval(self.general.refresh_interval);
        self.hud.top_count = self.hud.top_count.clamp(1, 10);
        let a = &mut self.alerts;
        a.cpu_percent = a.cpu_percent.clamp(10.0, 100.0);
        a.disk_used_fraction = a.disk_used_fraction.clamp(0.5, 1.0);
        a.low_battery_percent = a.low_battery_percent.min(100);
        a.cpu_sustained_cycles = a.cpu_sustained_cycles.max(1);
        a.process_sustained_cycles = a.process_sustained_cycles.max(1);
        self.history.max_age_hours = self.history.max_age_hours.clamp(1.0, 24.0 * 31.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_interval_bounds() {
        assert_eq!(clamp_interval(0.2), 1.0);
        assert_eq!(clamp_interval(100.0), 30.0);
        assert_eq!(clamp_interval(5.0), 5.0);
        assert_eq!(clamp_interval(f64::NAN), 3.0);
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(Settings::from_toml("").unwrap(), Settings::default());
    }

    #[test]
    fn round_trip() {
        let mut s = Settings::default();
        s.hud.metric = HudMetric::CpuMemory;
        s.alerts.cpu_percent = 80.0;
        assert_eq!(Settings::from_toml(&s.to_toml()).unwrap(), s);
    }

    #[test]
    fn partial_and_unknown_keys_and_clamping() {
        let s = Settings::from_toml("future_key = 1\n[general]\nrefresh_interval = 0.1\n[hud]\nmetric = \"health\"\n")
            .unwrap();
        assert_eq!(s.general.refresh_interval, 1.0);
        assert_eq!(s.hud.metric, HudMetric::Health);
        assert!(s.general.notifications);
    }

    #[test]
    fn serialized_settings_contain_no_paths() {
        let text = Settings::default().to_toml();
        assert!(!text.contains('/'), "config must stay portable:\n{text}");
    }
}

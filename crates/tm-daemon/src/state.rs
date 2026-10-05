//! Daemon state shared between the sampler loop and the D-Bus interface.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tm_core::alert::{AlertEngine, AlertInput, SystemAlert};
use tm_core::bus::API_VERSION;
use tm_core::health;
use tm_core::metrics::MetricsSample;
use tm_core::model::{ProcessItem, Snapshot};
use tm_core::settings::Settings;

use crate::paths::Paths;
use crate::store::{self, HistoryStore, SettingsStore};

/// Points kept for the HUD sparklines.
pub const SPARK_LEN: usize = 60;
/// A "full" subscription (GUI open) must be renewed within this window.
pub const LEASE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Hud,
    Full,
}

#[derive(Default)]
pub struct Sparks {
    pub cpu: VecDeque<f64>,
    pub mem: VecDeque<f64>,
    pub down: VecDeque<f64>,
    pub up: VecDeque<f64>,
}

impl Sparks {
    fn push(buf: &mut VecDeque<f64>, v: f64) {
        if buf.len() == SPARK_LEN {
            buf.pop_front();
        }
        buf.push_back((v * 10.0).round() / 10.0);
    }

    fn record(&mut self, snap: &Snapshot) {
        Self::push(&mut self.cpu, snap.cpu.global_usage);
        Self::push(&mut self.mem, snap.memory.usage_percentage());
        Self::push(&mut self.down, snap.network.total_rx_rate);
        Self::push(&mut self.up, snap.network.total_tx_rate);
    }
}

pub struct State {
    pub paths: Paths,
    pub settings: Settings,
    pub settings_store: SettingsStore,
    pub engine: AlertEngine,
    pub history: HistoryStore,
    pub snapshot: Option<Snapshot>,
    pub sparks: Sparks,
    pub seq: u64,
    leases: HashMap<u32, (Level, Instant)>,
    next_lease: u32,
}

impl State {
    pub fn load(paths: Paths, now: f64) -> Self {
        let mut settings_store = SettingsStore::new(&paths);
        let settings = settings_store.load();
        let mut engine = AlertEngine::new(settings.alerts.clone());
        engine.restore_inbox(store::load_alerts(&paths));
        let history = HistoryStore::open(&paths, settings.history.max_age_hours * 3600.0, now);
        Self {
            paths,
            settings,
            settings_store,
            engine,
            history,
            snapshot: None,
            sparks: Sparks::default(),
            seq: 0,
            leases: HashMap::new(),
            next_lease: 1,
        }
    }

    /// Re-read everything from disk (after an import or a manual edit).
    pub fn reload(&mut self, now: f64) {
        self.settings = self.settings_store.load();
        self.engine.thresholds = self.settings.alerts.clone();
        self.engine.restore_inbox(store::load_alerts(&self.paths));
        self.history = HistoryStore::open(&self.paths, self.settings.history.max_age_hours * 3600.0, now);
    }

    pub fn apply_settings(&mut self, settings: Settings) -> std::io::Result<()> {
        self.settings_store.save(&settings)?;
        self.engine.thresholds = settings.alerts.clone();
        self.history.set_max_age(settings.history.max_age_hours * 3600.0);
        self.settings = settings;
        Ok(())
    }

    pub fn subscribe(&mut self, level: Level, lease: u32) -> u32 {
        let id = if lease != 0 && self.leases.contains_key(&lease) {
            lease
        } else {
            self.next_lease += 1;
            self.next_lease
        };
        self.leases.insert(id, (level, Instant::now()));
        id
    }

    pub fn unsubscribe(&mut self, lease: u32) {
        self.leases.remove(&lease);
    }

    /// Whether any live subscriber wants full per-process data.
    pub fn wants_full(&mut self) -> bool {
        let now = Instant::now();
        self.leases.retain(|_, (_, at)| now.duration_since(*at) < LEASE);
        self.leases.values().any(|(l, _)| *l == Level::Full)
    }

    /// Fold one snapshot in. Returns the alerts that started on this tick.
    pub fn ingest(&mut self, snap: Snapshot) -> Vec<SystemAlert> {
        let top = snap.top_by_cpu(1).first().copied().cloned();
        let raised = self.engine.evaluate(
            &AlertInput {
                cpu_usage: snap.cpu.global_usage,
                memory: Some(&snap.memory),
                disk: Some(&snap.disk),
                thermal: snap.thermal.level,
                top_process: top.as_ref(),
                power: Some(&snap.power),
            },
            snap.timestamp,
        );
        if !raised.is_empty() {
            store::save_alerts(&self.paths, &self.engine.inbox);
        }
        self.history.record(MetricsSample {
            t: snap.timestamp,
            cpu: snap.cpu.global_usage,
            mem_used: snap.memory.used,
            mem_total: snap.memory.total,
            net_down: snap.network.total_rx_rate,
            net_up: snap.network.total_tx_rate,
            health: Some(self.engine.health_score),
        });
        self.sparks.record(&snap);
        self.seq += 1;
        self.snapshot = Some(snap);
        raised
    }

    pub fn clear_alerts(&mut self) {
        self.engine.clear_inbox();
        store::save_alerts(&self.paths, &self.engine.inbox);
    }

    pub fn find_process(&self, pid: u32, start_ticks: u64) -> Option<&ProcessItem> {
        self.snapshot.as_ref()?.processes.iter().find(|p| p.pid == pid && p.start_ticks == start_ticks)
    }

    /// The compact payload of the `Tick` signal: everything the top-bar HUD
    /// renders, nothing more.
    pub fn summary(&self) -> Value {
        let Some(s) = self.snapshot.as_ref() else {
            return json!({ "api": API_VERSION, "seq": self.seq, "ready": false });
        };
        let top = |list: Vec<&ProcessItem>| -> Vec<Value> {
            list.into_iter()
                .map(|p| {
                    json!({
                        "pid": p.pid, "start": p.start_ticks, "name": p.name,
                        "cpu": (p.cpu * 10.0).round() / 10.0, "mem": p.memory,
                        "protected": p.protected, "app_id": p.app_id,
                    })
                })
                .collect()
        };
        let n = self.settings.hud.top_count as usize;
        let gpu = s.gpus.iter().find(|g| g.utilization.is_some() || g.vram_used.is_some()).or(s.gpus.first());
        let recent: Vec<&SystemAlert> = self.engine.inbox.iter().take(3).collect();
        json!({
            "api": API_VERSION,
            "ready": true,
            "seq": self.seq,
            "t": s.timestamp,
            "interval": self.settings.general.refresh_interval,
            "hostname": s.system.hostname,
            "cpu": (s.cpu.global_usage * 10.0).round() / 10.0,
            "cores": s.cpu.cores.len(),
            "mem": {
                "pct": (s.memory.usage_percentage() * 10.0).round() / 10.0,
                "used": s.memory.used, "total": s.memory.total,
                "swap_used": s.memory.swap_used, "pressure": s.memory.pressure,
            },
            "net": { "down": s.network.total_rx_rate, "up": s.network.total_tx_rate },
            "gpu": gpu.map(|g| json!({
                "name": g.name, "util": g.utilization, "vram_used": g.vram_used,
                "vram_total": g.vram_total, "unified": g.is_unified_memory,
            })),
            "battery": s.power.has_battery.then(|| json!({
                "pct": s.power.charge_percent, "charging": s.power.is_charging,
                "plugged": s.power.is_plugged_in, "full": s.power.fully_charged,
                "tte": s.power.time_to_empty_min, "ttf": s.power.time_to_full_min,
                "watts": s.power.power_watts,
            })),
            "thermal": {
                "level": s.thermal.level, "temp": s.thermal.max_temp,
                "sensors": s.thermal.sensors.iter()
                    .map(|t| json!({ "kind": t.kind, "label": t.label, "temp": t.temp }))
                    .collect::<Vec<_>>(),
            },
            "health": {
                "score": self.engine.health_score,
                "rating": health::rating(self.engine.health_score),
                "diagnosis": self.engine.diagnosis,
            },
            "alerts": { "active": self.engine.active_count(), "recent": recent },
            "top_cpu": top(s.top_by_cpu(n)),
            "top_mem": top(s.top_by_memory(n)),
            "spark": { "cpu": self.sparks.cpu, "mem": self.sparks.mem, "down": self.sparks.down, "up": self.sparks.up },
            "hud": self.settings.hud,
        })
    }
}

/// Set one dotted key (`hud.metric`, `alerts.cpu_percent`, …) from a JSON value.
pub fn with_setting(settings: &Settings, key: &str, value: Value) -> Result<Settings, String> {
    let mut tree = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    let pointer = format!("/{}", key.replace('.', "/"));
    let slot = tree.pointer_mut(&pointer).ok_or_else(|| format!("unknown setting '{key}'"))?;
    *slot = value;
    let mut out: Settings = serde_json::from_value(tree).map_err(|e| format!("invalid value for '{key}': {e}"))?;
    out.sanitize();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tm_core::settings::HudMetric;

    #[test]
    fn set_dotted_setting() {
        let s = with_setting(&Settings::default(), "hud.metric", json!("cpu_memory")).unwrap();
        assert_eq!(s.hud.metric, HudMetric::CpuMemory);
        let s = with_setting(&s, "general.refresh_interval", json!(0.1)).unwrap();
        assert_eq!(s.general.refresh_interval, 1.0, "sanitized");
        assert!(with_setting(&s, "hud.nope", json!(1)).is_err());
        assert!(with_setting(&s, "hud.metric", json!("bogus")).is_err());
        assert!(with_setting(&s, "general", json!(1)).is_err());
    }

    #[test]
    fn leases_expire_and_renew() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::load(Paths::under(dir.path()), 0.0);
        assert!(!st.wants_full());
        let id = st.subscribe(Level::Full, 0);
        assert!(st.wants_full());
        assert_eq!(st.subscribe(Level::Full, id), id);
        st.unsubscribe(id);
        assert!(!st.wants_full());
    }

    #[test]
    fn summary_before_and_after_first_tick() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::load(Paths::under(dir.path()), 0.0);
        assert_eq!(st.summary()["ready"], json!(false));
        let mut snap = Snapshot { timestamp: 1.0, ..Default::default() };
        snap.cpu.global_usage = 42.04;
        snap.processes.push(ProcessItem { pid: 7, name: "a".into(), cpu: 99.0, ..Default::default() });
        st.ingest(snap);
        let s = st.summary();
        assert_eq!(s["ready"], json!(true));
        assert_eq!(s["cpu"], json!(42.0));
        assert_eq!(s["top_cpu"][0]["pid"], json!(7));
        assert_eq!(s["spark"]["cpu"].as_array().unwrap().len(), 1);
        assert_eq!(s["health"]["score"], json!(100));
        assert_eq!(st.history.len(), 1);
    }
}

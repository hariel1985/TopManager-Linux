//! The `io.github.hariel1985.TopManager1` D-Bus interface.
//!
//! Payloads are JSON strings (versioned by `ApiVersion`): trivially consumed
//! by the GJS Shell extension with `JSON.parse`, and by the GUI with serde.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tm_collect::signal::{self, Action};
use tm_collect::Host;
use tm_core::bus::API_VERSION;
use tm_core::metrics::HistoryRange;
use zbus::interface;
use zbus::object_server::SignalEmitter;

use crate::state::{self, Level, State};

pub struct Service {
    pub state: Arc<Mutex<State>>,
    /// Wakes the sampler early (settings changed, new full subscriber).
    pub wake: Arc<tokio::sync::Notify>,
}

impl Service {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn now() -> f64 {
    tm_collect::host::now_unix()
}

#[interface(name = "io.github.hariel1985.TopManager1")]
impl Service {
    /// The same payload as the latest `Tick` signal.
    fn get_summary(&self) -> String {
        self.lock().summary().to_string()
    }

    /// The full latest snapshot (every collector, without the process list).
    fn get_snapshot(&self) -> String {
        let st = self.lock();
        match &st.snapshot {
            Some(s) => {
                let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.remove("processes");
                    o.insert("api".into(), json!(API_VERSION));
                }
                v.to_string()
            }
            None => json!({ "api": API_VERSION, "ready": false }).to_string(),
        }
    }

    fn get_processes(&self) -> String {
        let st = self.lock();
        let procs = st.snapshot.as_ref().map(|s| &s.processes);
        json!({ "api": API_VERSION, "seq": st.seq, "processes": procs }).to_string()
    }

    fn get_process_detail(&self, pid: u32) -> String {
        match tm_collect::process::detail(&Host::default(), pid) {
            Some(d) => serde_json::to_string(&d).unwrap_or_default(),
            None => json!({ "error": "process not found" }).to_string(),
        }
    }

    /// `action` is `term`, `kill`, `stop` or `cont`. `start_ticks` must match
    /// the process the caller saw, which guards against pid reuse.
    #[zbus(out_args("ok", "error"))]
    fn signal_process(&self, pid: u32, start_ticks: u64, action: &str) -> (bool, String) {
        let Some(action) = Action::parse(action) else {
            return (false, format!("unknown action '{action}'"));
        };
        let name = {
            let st = self.lock();
            st.find_process(pid, start_ticks).map(|p| p.name.clone())
        }
        .or_else(|| tm_collect::host::read_trimmed(format!("/proc/{pid}/comm")))
        .unwrap_or_default();
        match signal::send(&Host::default(), pid, start_ticks, &name, action) {
            Ok(()) => {
                self.wake.notify_one();
                (true, String::new())
            }
            Err(e) => (false, e.to_string()),
        }
    }

    /// `range`: `5m`, `30m`, `1h` or `24h`.
    fn get_history(&self, range: &str) -> String {
        let Some(r) = HistoryRange::parse(range) else {
            return json!({ "error": format!("unknown range '{range}'") }).to_string();
        };
        let samples = self.lock().history.query(r, now());
        json!({ "api": API_VERSION, "range": r, "samples": samples }).to_string()
    }

    fn get_alerts(&self) -> String {
        let st = self.lock();
        json!({
            "api": API_VERSION,
            "active": st.engine.active_kinds(),
            "inbox": st.engine.inbox,
        })
        .to_string()
    }

    fn clear_alerts(&self) {
        self.lock().clear_alerts();
        self.wake.notify_one();
    }

    fn get_settings(&self) -> String {
        serde_json::to_string(&self.lock().settings).unwrap_or_default()
    }

    /// Set one dotted key, e.g. `SetSetting("hud.metric", "\"health\"")`.
    #[zbus(out_args("ok", "error"))]
    async fn set_setting(
        &self,
        key: &str,
        json_value: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> (bool, String) {
        let value: Value = match serde_json::from_str(json_value) {
            Ok(v) => v,
            Err(e) => return (false, format!("value is not JSON: {e}")),
        };
        let result = {
            let mut st = self.lock();
            state::with_setting(&st.settings, key, value)
                .and_then(|s| st.apply_settings(s).map_err(|e| e.to_string()))
                .map(|_| serde_json::to_string(&st.settings).unwrap_or_default())
        };
        match result {
            Ok(settings) => {
                let _ = Self::settings_changed(&emitter, &settings).await;
                self.wake.notify_one();
                (true, String::new())
            }
            Err(e) => (false, e),
        }
    }

    /// Re-read settings, alerts and history from disk (after an import).
    async fn reload(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let settings = {
            let mut st = self.lock();
            st.reload(now());
            serde_json::to_string(&st.settings).unwrap_or_default()
        };
        let _ = Self::settings_changed(&emitter, &settings).await;
        self.wake.notify_one();
    }

    /// `level` is `hud` or `full`. Pass 0 to get a new lease, or a previous
    /// lease id to renew it; leases expire after 15 s without renewal.
    fn subscribe(&self, level: &str, lease: u32) -> u32 {
        let level = if level == "full" { Level::Full } else { Level::Hud };
        let id = self.lock().subscribe(level, lease);
        self.wake.notify_one();
        id
    }

    fn unsubscribe(&self, lease: u32) {
        self.lock().unsubscribe(lease);
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    #[zbus(property)]
    fn api_version(&self) -> u32 {
        API_VERSION
    }

    #[zbus(signal)]
    pub async fn tick(emitter: &SignalEmitter<'_>, summary: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn alert_raised(emitter: &SignalEmitter<'_>, alert: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn settings_changed(emitter: &SignalEmitter<'_>, settings: &str) -> zbus::Result<()>;
}

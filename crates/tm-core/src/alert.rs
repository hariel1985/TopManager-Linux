//! Alert engine, ported from `Alert.swift` + `AlertCenter.swift`.
//!
//! The engine is pure: it takes a snapshot and a timestamp and returns the
//! alerts that *started* on this tick. Delivering notifications is the
//! daemon's job. The alert kind is the de-dup key: at most one active alert per
//! kind, so a condition that stays true never storms the user.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::health::{self, HealthInput, ThermalLevel};
use crate::model::{DiskInfo, MemoryInfo, MemoryPressure, PowerInfo, ProcessItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    CpuHigh,
    MemoryPressure,
    DiskFull,
    Thermal,
    RunawayProcess,
    LowBattery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemAlert {
    pub id: u64,
    pub kind: AlertKind,
    pub title: String,
    pub message: String,
    pub severity: AlertSeverity,
    /// Unix seconds.
    pub timestamp: f64,
    /// Set for process alerts so the HUD can offer "quit" safely.
    pub pid: Option<u32>,
    pub start_ticks: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertThresholds {
    pub cpu_percent: f64,
    pub cpu_sustained_cycles: u32,
    pub memory_pressure_critical: bool,
    pub disk_used_fraction: f64,
    pub thermal_serious: bool,
    /// Per-core percent (~1.9 cores busy).
    pub process_cpu_percent: f64,
    pub process_sustained_cycles: u32,
    pub low_battery_percent: u8,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            cpu_percent: 90.0,
            cpu_sustained_cycles: 3,
            memory_pressure_critical: true,
            disk_used_fraction: 0.95,
            thermal_serious: true,
            process_cpu_percent: 190.0,
            process_sustained_cycles: 3,
            low_battery_percent: 15,
        }
    }
}

/// "Sustained" conditions must hold for N consecutive samples before firing,
/// and clear as soon as they recover. The returned count is fed back next tick.
pub fn sustained(breached: bool, previous_count: u32, required_cycles: u32) -> (bool, u32) {
    let count = if breached { previous_count + 1 } else { 0 };
    (count >= required_cycles, count)
}

/// Everything one evaluation needs.
pub struct AlertInput<'a> {
    pub cpu_usage: f64,
    pub memory: Option<&'a MemoryInfo>,
    pub disk: Option<&'a DiskInfo>,
    pub thermal: ThermalLevel,
    pub top_process: Option<&'a ProcessItem>,
    pub power: Option<&'a PowerInfo>,
}

#[derive(Debug, Default)]
pub struct AlertEngine {
    pub thresholds: AlertThresholds,
    pub health_score: u8,
    pub diagnosis: Vec<String>,
    /// Inbox, newest first.
    pub inbox: Vec<SystemAlert>,
    active: BTreeSet<AlertKind>,
    cpu_count: u32,
    proc_count: u32,
    next_id: u64,
}

const INBOX_LIMIT: usize = 50;

impl AlertEngine {
    pub fn new(thresholds: AlertThresholds) -> Self {
        Self { thresholds, health_score: 100, next_id: 1, ..Default::default() }
    }

    pub fn active_kinds(&self) -> &BTreeSet<AlertKind> {
        &self.active
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn clear_inbox(&mut self) {
        self.inbox.clear();
    }

    /// Restore a persisted inbox (newest first) without re-firing anything.
    pub fn restore_inbox(&mut self, inbox: Vec<SystemAlert>) {
        self.next_id = inbox.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        self.inbox = inbox;
        self.inbox.truncate(INBOX_LIMIT);
    }

    /// Evaluate one snapshot. Returns the alerts that started on this tick.
    pub fn evaluate(&mut self, input: &AlertInput, now: f64) -> Vec<SystemAlert> {
        let t = self.thresholds.clone();
        let disk_fraction = input.disk.map(DiskInfo::max_used_fraction).unwrap_or(0.0);
        let pressure = input.memory.map(|m| m.pressure).unwrap_or_default();

        let h = HealthInput {
            cpu_usage: input.cpu_usage,
            memory_pressure: pressure,
            swap_used_bytes: input.memory.map(|m| m.swap_used).unwrap_or(0),
            thermal: input.thermal,
            disk_used_fraction: disk_fraction,
        };
        self.health_score = health::score(&h);
        self.diagnosis = health::diagnosis(&h);

        let mut raised = Vec::new();

        let (firing, count) = sustained(input.cpu_usage >= t.cpu_percent, self.cpu_count, t.cpu_sustained_cycles);
        self.cpu_count = count;
        self.set_active(
            &mut raised,
            now,
            AlertKind::CpuHigh,
            firing,
            "High CPU usage",
            format!("CPU has stayed above {:.0}% (now {:.0}%).", t.cpu_percent, input.cpu_usage),
            AlertSeverity::Warning,
            None,
        );

        self.set_active(
            &mut raised,
            now,
            AlertKind::MemoryPressure,
            t.memory_pressure_critical && pressure == MemoryPressure::Critical,
            "Critical memory pressure",
            "The system is very low on memory. Consider closing apps.".into(),
            AlertSeverity::Critical,
            None,
        );

        self.set_active(
            &mut raised,
            now,
            AlertKind::DiskFull,
            disk_fraction >= t.disk_used_fraction,
            "Disk almost full",
            format!("A volume is {:.0}% full.", disk_fraction * 100.0),
            AlertSeverity::Warning,
            None,
        );

        let hot = matches!(input.thermal, ThermalLevel::Serious | ThermalLevel::Critical);
        self.set_active(
            &mut raised,
            now,
            AlertKind::Thermal,
            t.thermal_serious && hot,
            "System is running hot",
            "Thermal state is elevated; performance may be throttled.".into(),
            if input.thermal == ThermalLevel::Critical { AlertSeverity::Critical } else { AlertSeverity::Warning },
            None,
        );

        let runaway = input.top_process.is_some_and(|p| p.cpu >= t.process_cpu_percent);
        let (firing, count) = sustained(runaway, self.proc_count, t.process_sustained_cycles);
        self.proc_count = count;
        let top = input.top_process;
        self.set_active(
            &mut raised,
            now,
            AlertKind::RunawayProcess,
            firing,
            "Runaway process",
            top.map(|p| format!("{} (PID {}) is using {:.0}% CPU.", p.name, p.pid, p.cpu)).unwrap_or_default(),
            AlertSeverity::Warning,
            top.map(|p| (p.pid, p.start_ticks)),
        );

        let low_battery =
            input.power.is_some_and(|p| p.has_battery && !p.is_plugged_in && p.charge_percent <= t.low_battery_percent);
        self.set_active(
            &mut raised,
            now,
            AlertKind::LowBattery,
            low_battery,
            "Low battery",
            format!("Battery is at {}% and not charging.", input.power.map(|p| p.charge_percent).unwrap_or(0)),
            AlertSeverity::Warning,
            None,
        );

        raised
    }

    #[allow(clippy::too_many_arguments)]
    fn set_active(
        &mut self,
        raised: &mut Vec<SystemAlert>,
        now: f64,
        kind: AlertKind,
        active: bool,
        title: &str,
        message: String,
        severity: AlertSeverity,
        process: Option<(u32, u64)>,
    ) {
        if !active {
            self.active.remove(&kind);
            return;
        }
        if !self.active.insert(kind) {
            return; // already firing: de-dup
        }
        let alert = SystemAlert {
            id: self.next_id,
            kind,
            title: title.into(),
            message,
            severity,
            timestamp: now,
            pid: process.map(|p| p.0),
            start_ticks: process.map(|p| p.1),
        };
        self.next_id += 1;
        self.inbox.insert(0, alert.clone());
        self.inbox.truncate(INBOX_LIMIT);
        raised.push(alert);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(cpu: f64, memory: Option<&'a MemoryInfo>) -> AlertInput<'a> {
        AlertInput {
            cpu_usage: cpu,
            memory,
            disk: None,
            thermal: ThermalLevel::Nominal,
            top_process: None,
            power: None,
        }
    }

    #[test]
    fn sustained_fires_only_after_required_cycles() {
        let (f1, c) = sustained(true, 0, 3);
        let (f2, c) = sustained(true, c, 3);
        let (f3, _) = sustained(true, c, 3);
        assert!(!f1 && !f2 && f3);
    }

    #[test]
    fn sustained_resets_when_condition_clears() {
        assert_eq!(sustained(false, 5, 3), (false, 0));
    }

    #[test]
    fn severity_ordering() {
        assert!(AlertSeverity::Info < AlertSeverity::Warning);
        assert!(AlertSeverity::Warning < AlertSeverity::Critical);
    }

    #[test]
    fn dedupes_and_clears_active_kinds() {
        let mut e = AlertEngine::new(AlertThresholds::default());
        let critical = MemoryInfo { pressure: MemoryPressure::Critical, ..Default::default() };
        let ok = MemoryInfo { pressure: MemoryPressure::Nominal, ..Default::default() };

        assert_eq!(e.evaluate(&input(5.0, Some(&critical)), 1.0).len(), 1);
        assert!(e.evaluate(&input(5.0, Some(&critical)), 2.0).is_empty(), "must not re-fire");
        assert_eq!(e.inbox.len(), 1);
        assert!(e.active_kinds().contains(&AlertKind::MemoryPressure));

        e.evaluate(&input(5.0, Some(&ok)), 3.0);
        assert!(!e.active_kinds().contains(&AlertKind::MemoryPressure));
        assert_eq!(e.inbox.len(), 1);

        // Recurring after recovery is a new alert.
        assert_eq!(e.evaluate(&input(5.0, Some(&critical)), 4.0).len(), 1);
        assert_eq!(e.inbox.len(), 2);
        assert!(e.inbox[0].id > e.inbox[1].id, "inbox is newest first");
    }

    #[test]
    fn cpu_alert_needs_sustained_breach() {
        let mut e = AlertEngine::new(AlertThresholds::default());
        assert!(e.evaluate(&input(95.0, None), 1.0).is_empty());
        assert!(e.evaluate(&input(95.0, None), 2.0).is_empty());
        let raised = e.evaluate(&input(95.0, None), 3.0);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].kind, AlertKind::CpuHigh);
        assert!(e.health_score < 100);
    }

    #[test]
    fn runaway_process_carries_identity() {
        let mut e = AlertEngine::new(AlertThresholds { process_sustained_cycles: 1, ..Default::default() });
        let p = ProcessItem { pid: 42, start_ticks: 777, name: "yes".into(), cpu: 199.0, ..Default::default() };
        let inp = AlertInput { top_process: Some(&p), ..input(10.0, None) };
        let raised = e.evaluate(&inp, 1.0);
        assert_eq!(raised.len(), 1);
        assert_eq!((raised[0].pid, raised[0].start_ticks), (Some(42), Some(777)));
    }

    #[test]
    fn low_battery_only_when_unplugged() {
        let mut e = AlertEngine::new(AlertThresholds::default());
        let plugged = PowerInfo { has_battery: true, charge_percent: 5, is_plugged_in: true, ..Default::default() };
        let unplugged = PowerInfo { is_plugged_in: false, ..plugged.clone() };
        assert!(e.evaluate(&AlertInput { power: Some(&plugged), ..input(1.0, None) }, 1.0).is_empty());
        assert_eq!(e.evaluate(&AlertInput { power: Some(&unplugged), ..input(1.0, None) }, 2.0).len(), 1);
    }

    #[test]
    fn inbox_is_capped_and_restorable() {
        let mut e = AlertEngine::new(AlertThresholds::default());
        let critical = MemoryInfo { pressure: MemoryPressure::Critical, ..Default::default() };
        let ok = MemoryInfo::default();
        for i in 0..60 {
            e.evaluate(&input(1.0, Some(&critical)), i as f64);
            e.evaluate(&input(1.0, Some(&ok)), i as f64 + 0.5);
        }
        assert_eq!(e.inbox.len(), INBOX_LIMIT);

        let saved = e.inbox.clone();
        let mut restored = AlertEngine::new(AlertThresholds::default());
        restored.restore_inbox(saved.clone());
        let new = restored.evaluate(&input(1.0, Some(&critical)), 100.0);
        assert!(new[0].id > saved[0].id, "ids keep increasing after restore");
    }
}

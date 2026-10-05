//! Snapshot data models. Serialized as JSON over D-Bus (see `bus::API_VERSION`),
//! so field names are part of the API: add fields, don't rename them.

use serde::{Deserialize, Serialize};

use crate::battery::{BatteryCondition, BatteryMath};
use crate::health::ThermalLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CoreType {
    /// Performance core (Intel `cpu_core`, or the big cluster on ARM).
    Performance,
    /// Efficiency core (Intel `cpu_atom`, or the LITTLE cluster on ARM).
    Efficiency,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreUsage {
    pub id: usize,
    pub usage: f64,
    pub core_type: CoreType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CpuInfo {
    pub global_usage: f64,
    pub user_usage: f64,
    pub system_usage: f64,
    pub idle_usage: f64,
    pub iowait_usage: f64,
    pub cores: Vec<CoreUsage>,
}

impl CpuInfo {
    pub fn is_hybrid(&self) -> bool {
        self.cores.iter().any(|c| c.core_type == CoreType::Efficiency)
            && self.cores.iter().any(|c| c.core_type == CoreType::Performance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryPressure {
    Nominal,
    Warning,
    Critical,
    #[default]
    Unknown,
}

impl MemoryPressure {
    /// Classify Linux PSI (`/proc/pressure/memory`) 10-second averages.
    ///
    /// `some` = share of time at least one task stalled on memory, `full` = share
    /// of time *all* non-idle tasks stalled. `full` is the stronger signal, so it
    /// escalates at much lower values.
    pub fn from_psi(some_avg10: f64, full_avg10: f64) -> Self {
        if some_avg10 >= 40.0 || full_avg10 >= 10.0 {
            MemoryPressure::Critical
        } else if some_avg10 >= 10.0 || full_avg10 >= 2.0 {
            MemoryPressure::Warning
        } else {
            MemoryPressure::Nominal
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MemoryInfo {
    pub total: u64,
    /// `MemTotal - MemAvailable`: what the kernel can't hand out without reclaim.
    pub used: u64,
    pub free: u64,
    pub available: u64,
    pub active: u64,
    pub inactive: u64,
    pub cached: u64,
    pub buffers: u64,
    pub shmem: u64,
    /// Compressed size held by zram devices (the closest Linux analogue to the
    /// macOS compressor); 0 when zram isn't in use.
    pub compressed: u64,
    /// Original (uncompressed) size of what zram holds.
    pub compressed_original: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub pressure: MemoryPressure,
    pub psi_some_avg10: Option<f64>,
    pub psi_full_avg10: Option<f64>,
}

impl MemoryInfo {
    pub fn usage_percentage(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f64 / self.total as f64 * 100.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub name: String,
    pub mount_point: String,
    pub device: String,
    pub file_system: String,
    pub total: u64,
    pub free: u64,
    pub used: u64,
    pub is_removable: bool,
}

impl VolumeInfo {
    pub fn new(
        name: String,
        mount_point: String,
        device: String,
        file_system: String,
        total: u64,
        free: u64,
        is_removable: bool,
    ) -> Self {
        let used = total.saturating_sub(free);
        Self { name, mount_point, device, file_system, total, free, used, is_removable }
    }

    pub fn usage_percentage(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f64 / self.total as f64 * 100.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DiskInfo {
    pub volumes: Vec<VolumeInfo>,
}

impl DiskInfo {
    /// Fullest volume as a 0…1 fraction (drives the health score and disk alert).
    pub fn max_used_fraction(&self) -> f64 {
        self.volumes.iter().map(|v| v.usage_percentage() / 100.0).fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub is_up: bool,
    /// Bridges, veth, tun, docker… Excluded from totals to avoid double counting.
    pub is_virtual: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NetworkInfo {
    pub interfaces: Vec<NetworkInterface>,
    pub total_rx_rate: f64,
    pub total_tx_rate: f64,
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub utilization: Option<f64>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    /// Integrated GPUs share system RAM.
    pub is_unified_memory: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PowerInfo {
    pub has_battery: bool,
    pub charge_percent: u8,
    pub is_charging: bool,
    pub is_plugged_in: bool,
    pub fully_charged: bool,
    pub cycle_count: Option<u32>,
    /// Design and current full-charge capacity, in whatever unit the battery
    /// reports (µWh or µAh); only their ratio is meaningful.
    pub design_capacity: Option<u64>,
    pub full_capacity: Option<u64>,
    pub temperature: Option<f64>,
    /// Positive while charging, negative while discharging.
    pub power_watts: Option<f64>,
    pub time_to_empty_min: Option<u32>,
    pub time_to_full_min: Option<u32>,
    pub model: Option<String>,
}

impl PowerInfo {
    pub fn health_percent(&self) -> Option<f64> {
        match (self.full_capacity, self.design_capacity) {
            (Some(full), Some(design)) if design > 0 => Some(BatteryMath::health_percent(full, design)),
            _ => None,
        }
    }

    pub fn condition(&self) -> BatteryCondition {
        BatteryMath::condition(self.health_percent(), self.cycle_count)
    }
}

/// What a temperature sensor measures; the order is the display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SensorKind {
    Cpu,
    Gpu,
    Memory,
    Storage,
    Wifi,
    Chipset,
    Battery,
    /// ACPI / motherboard zones.
    System,
    #[default]
    Other,
}

impl SensorKind {
    pub fn label(self) -> &'static str {
        match self {
            SensorKind::Cpu => "CPU",
            SensorKind::Gpu => "GPU",
            SensorKind::Memory => "RAM",
            SensorKind::Storage => "SSD",
            SensorKind::Wifi => "Wi-Fi",
            SensorKind::Chipset => "Chipset",
            SensorKind::Battery => "Battery",
            SensorKind::System => "System",
            SensorKind::Other => "Other",
        }
    }
}

/// The hottest reading of one component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TempSensor {
    pub kind: SensorKind,
    /// Display name: the kind's label, or the chip name for `Other`.
    pub label: String,
    /// The underlying sensor, e.g. "k10temp Tctl".
    pub sensor: String,
    /// °C.
    pub temp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ThermalInfo {
    pub level: ThermalLevel,
    /// Hottest sensor reading, °C.
    pub max_temp: Option<f64>,
    pub sensor: Option<String>,
    /// One entry per component (CPU, GPU, RAM, …), in display order.
    pub sensors: Vec<TempSensor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    Running,
    Sleeping,
    /// Uninterruptible sleep (`D`), usually blocked on I/O.
    DiskSleep,
    Stopped,
    Zombie,
    Idle,
    #[default]
    Unknown,
}

impl ProcessState {
    pub fn from_proc_char(c: char) -> Self {
        match c {
            'R' => ProcessState::Running,
            'S' => ProcessState::Sleeping,
            'D' => ProcessState::DiskSleep,
            'T' | 't' => ProcessState::Stopped,
            'Z' | 'X' => ProcessState::Zombie,
            'I' => ProcessState::Idle,
            _ => ProcessState::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcessItem {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub user: String,
    pub uid: u32,
    /// Per-core: 100 % = one core fully busy.
    pub cpu: f64,
    /// Normalized: 100 % = every core fully busy.
    pub cpu_total: f64,
    /// Private resident memory (RSS minus file-backed/shared pages).
    pub memory: u64,
    /// Full resident set, shared pages included.
    pub resident: u64,
    /// Swapped-out size (`VmSwap`); only filled on full refreshes.
    pub swapped: u64,
    pub threads: u32,
    pub state: ProcessState,
    /// Start time in clock ticks since boot: together with the pid it uniquely
    /// identifies a process, which guards signals against pid reuse.
    pub start_ticks: u64,
    /// Start time as Unix seconds.
    pub start_time: f64,
    pub disk_read_rate: f64,
    pub disk_write_rate: f64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub energy: f64,
    /// Desktop app id when the process runs in a GNOME/Flatpak app scope.
    pub app_id: Option<String>,
    /// Killing it would end the desktop session (gnome-shell, systemd, …):
    /// the HUD hides its quit button and the daemon refuses term/kill.
    pub protected: bool,
}

/// On-demand deep dive for the process inspector (too costly for every row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProcessDetail {
    pub pid: u32,
    pub start_ticks: u64,
    pub exe: Option<String>,
    pub cmdline: Vec<String>,
    pub cwd: Option<String>,
    pub open_files: Option<u32>,
    /// Proportional set size (shared pages split between their users).
    pub pss: Option<u64>,
    /// Unique set size (private pages only).
    pub uss: Option<u64>,
    pub swap: Option<u64>,
    pub voluntary_ctxt_switches: Option<u64>,
    pub nonvoluntary_ctxt_switches: Option<u64>,
    pub nice: Option<i32>,
    pub cgroup: Option<String>,
}

/// Everything one sampling tick produces. `GetSnapshot` sends it without the
/// process list, hence the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Snapshot {
    /// Unix seconds.
    pub timestamp: f64,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub processes: Vec<ProcessItem>,
    pub disk: DiskInfo,
    pub network: NetworkInfo,
    pub gpus: Vec<GpuInfo>,
    pub power: PowerInfo,
    pub thermal: ThermalInfo,
    pub system: SystemInfo,
}

impl Snapshot {
    /// Top processes, kernel threads excluded: they can't be signalled, so a
    /// quit button (HUD) or a runaway-process alert would be useless.
    pub fn top_by_cpu(&self, n: usize) -> Vec<&ProcessItem> {
        let mut v: Vec<&ProcessItem> = self.processes.iter().filter(|p| !p.is_kernel_thread()).collect();
        v.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        v.truncate(n);
        v
    }

    pub fn top_by_memory(&self, n: usize) -> Vec<&ProcessItem> {
        let mut v: Vec<&ProcessItem> = self.processes.iter().filter(|p| !p.is_kernel_thread()).collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.memory));
        v.truncate(n);
        v
    }
}

impl ProcessItem {
    /// kthreadd (pid 2) and its children: kworker, ksoftirqd, …
    pub fn is_kernel_thread(&self) -> bool {
        self.pid == 2 || self.ppid == 2
    }

    pub fn disk_total_rate(&self) -> f64 {
        self.disk_read_rate + self.disk_write_rate
    }
}

/// Heuristic energy-impact proxy, same shape as the macOS `EnergyModel`:
/// sustained CPU dominates, with a small penalty for wakeups (on Linux the
/// voluntary context-switch rate), which correlate with power draw at low CPU.
pub struct EnergyModel;

impl EnergyModel {
    pub fn impact(cpu_percent: f64, wakeups_per_sec: f64) -> f64 {
        cpu_percent.max(0.0) + wakeups_per_sec.max(0.0) * 0.045
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SystemInfo {
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    pub architecture: String,
    pub cpu_model: String,
    pub cpu_count: usize,
    pub uptime_secs: f64,
    pub boot_time: f64,
    pub desktop: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_usage_percentage() {
        let m = MemoryInfo { total: 16_000, used: 8_000, ..Default::default() };
        assert!((m.usage_percentage() - 50.0).abs() < 1e-9);
        assert_eq!(MemoryInfo::default().usage_percentage(), 0.0);
    }

    #[test]
    fn psi_classification() {
        assert_eq!(MemoryPressure::from_psi(0.0, 0.0), MemoryPressure::Nominal);
        assert_eq!(MemoryPressure::from_psi(12.0, 0.0), MemoryPressure::Warning);
        assert_eq!(MemoryPressure::from_psi(0.0, 3.0), MemoryPressure::Warning);
        assert_eq!(MemoryPressure::from_psi(45.0, 0.0), MemoryPressure::Critical);
        assert_eq!(MemoryPressure::from_psi(5.0, 12.0), MemoryPressure::Critical);
    }

    #[test]
    fn volume_used_space_and_percentage() {
        let v = VolumeInfo::new("root".into(), "/".into(), "/dev/sda1".into(), "ext4".into(), 1000, 250, false);
        assert_eq!(v.used, 750);
        assert!((v.usage_percentage() - 75.0).abs() < 1e-9);
    }

    #[test]
    fn volume_free_greater_than_total_clamps() {
        let v = VolumeInfo::new("x".into(), "/x".into(), "d".into(), "ext4".into(), 100, 200, false);
        assert_eq!(v.used, 0);
    }

    #[test]
    fn disk_max_used_fraction() {
        let d = DiskInfo {
            volumes: vec![
                VolumeInfo::new("a".into(), "/".into(), "a".into(), "ext4".into(), 100, 50, false),
                VolumeInfo::new("b".into(), "/b".into(), "b".into(), "ext4".into(), 100, 10, false),
            ],
        };
        assert!((d.max_used_fraction() - 0.9).abs() < 1e-9);
        assert_eq!(DiskInfo::default().max_used_fraction(), 0.0);
    }

    #[test]
    fn process_states() {
        assert_eq!(ProcessState::from_proc_char('R'), ProcessState::Running);
        assert_eq!(ProcessState::from_proc_char('D'), ProcessState::DiskSleep);
        assert_eq!(ProcessState::from_proc_char('t'), ProcessState::Stopped);
        assert_eq!(ProcessState::from_proc_char('I'), ProcessState::Idle);
        assert_eq!(ProcessState::from_proc_char('?'), ProcessState::Unknown);
    }

    #[test]
    fn top_lists_skip_kernel_threads() {
        let mut s = Snapshot::default();
        s.processes.push(ProcessItem {
            pid: 90,
            ppid: 2,
            name: "kworker/0:4".into(),
            cpu: 99.0,
            memory: 9,
            ..Default::default()
        });
        s.processes.push(ProcessItem {
            pid: 2,
            ppid: 0,
            name: "kthreadd".into(),
            cpu: 98.0,
            memory: 8,
            ..Default::default()
        });
        s.processes.push(ProcessItem {
            pid: 500,
            ppid: 1,
            name: "app".into(),
            cpu: 1.0,
            memory: 1,
            ..Default::default()
        });
        assert_eq!(s.top_by_cpu(3).iter().map(|p| p.pid).collect::<Vec<_>>(), vec![500]);
        assert_eq!(s.top_by_memory(3).len(), 1);
    }

    #[test]
    fn disk_total_rate_sums() {
        let p = ProcessItem { disk_read_rate: 100.0, disk_write_rate: 50.0, ..Default::default() };
        assert_eq!(p.disk_total_rate(), 150.0);
    }

    #[test]
    fn energy_monotonic_and_clamped() {
        assert!(EnergyModel::impact(50.0, 0.0) > EnergyModel::impact(10.0, 0.0));
        assert!(EnergyModel::impact(10.0, 500.0) > EnergyModel::impact(10.0, 0.0));
        assert_eq!(EnergyModel::impact(-5.0, -100.0), 0.0);
        assert_eq!(EnergyModel::impact(33.0, 0.0), 33.0);
    }

    #[test]
    fn hybrid_detection() {
        let mut c = CpuInfo::default();
        c.cores.push(CoreUsage { id: 0, usage: 0.0, core_type: CoreType::Performance });
        assert!(!c.is_hybrid());
        c.cores.push(CoreUsage { id: 1, usage: 0.0, core_type: CoreType::Efficiency });
        assert!(c.is_hybrid());
    }

    #[test]
    fn json_field_names_are_snake_case() {
        let json = serde_json::to_string(&ProcessItem::default()).unwrap();
        assert!(json.contains("\"start_ticks\"") && json.contains("\"cpu_total\""));
        let json = serde_json::to_string(&MemoryPressure::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
    }
}

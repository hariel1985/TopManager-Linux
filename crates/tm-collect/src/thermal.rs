//! Thermal level from thermal zones and hwmon sensors.
//!
//! Linux has no single "thermal state" like macOS, so each sensor is rated
//! against its own trip points (or fixed CPU-ish limits when it has none) and
//! the hottest rating wins. Readings are also grouped by component (CPU, GPU,
//! RAM, …), keeping each component's hottest sensor, so the UI can show more
//! than the single hottest chip (often the Wi-Fi card).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tm_core::health::ThermalLevel;
use tm_core::model::{SensorKind, TempSensor, ThermalInfo};

use crate::host::{self, Host};

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Trips {
    pub passive: Option<f64>,
    pub hot: Option<f64>,
    pub critical: Option<f64>,
}

pub fn rate(temp: f64, trips: Trips) -> ThermalLevel {
    if trips == Trips::default() {
        return match temp {
            t if t >= 95.0 => ThermalLevel::Critical,
            t if t >= 85.0 => ThermalLevel::Serious,
            t if t >= 75.0 => ThermalLevel::Fair,
            _ => ThermalLevel::Nominal,
        };
    }
    if trips.critical.is_some_and(|c| temp >= c - 5.0) {
        return ThermalLevel::Critical;
    }
    if trips.hot.is_some_and(|h| temp >= h) || trips.passive.is_some_and(|p| temp >= p) {
        return ThermalLevel::Serious;
    }
    let warn = trips.passive.or(trips.hot).or(trips.critical.map(|c| c - 10.0));
    if warn.is_some_and(|w| temp >= w - 10.0) {
        return ThermalLevel::Fair;
    }
    ThermalLevel::Nominal
}

/// Millidegrees → °C, discarding readings no real sensor produces.
fn millideg(path: &Path) -> Option<f64> {
    let t = host::read_i64(path)? as f64 / 1000.0;
    (t > 0.0 && t < 150.0).then_some(t)
}

fn zone_trips(dir: &Path) -> Trips {
    let mut trips = Trips::default();
    for i in 0..16 {
        let Some(kind) = host::read_trimmed(dir.join(format!("trip_point_{i}_type"))) else { break };
        let Some(t) = millideg(&dir.join(format!("trip_point_{i}_temp"))) else { continue };
        let slot = match kind.as_str() {
            "passive" => &mut trips.passive,
            "hot" => &mut trips.hot,
            "critical" => &mut trips.critical,
            _ => continue,
        };
        *slot = Some(slot.map_or(t, |old: f64| old.min(t)));
    }
    trips
}

/// Component behind an hwmon chip name or thermal zone type.
pub fn classify(source: &str) -> SensorKind {
    let n = source.to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| n.starts_with(p));
    if starts(&["k10temp", "coretemp", "zenpower", "x86_pkg_temp", "tcpu", "b0d4", "soc_dts"]) || n.contains("cpu") {
        SensorKind::Cpu
    } else if starts(&["amdgpu", "radeon", "nouveau", "nvidia", "i915"]) || n == "xe" || n.contains("gpu") {
        SensorKind::Gpu
    } else if starts(&["spd5118", "jc42", "tmem"]) || n.contains("dimm") {
        SensorKind::Memory
    } else if starts(&["nvme", "drivetemp"]) {
        SensorKind::Storage
    } else if starts(&["iwlwifi", "mt76", "mt79", "ath1", "ath9k", "rtw", "brcmf", "wlan"]) || n.contains("_phy") {
        SensorKind::Wifi
    } else if n.contains("pch") {
        SensorKind::Chipset
    } else if starts(&["bat"]) {
        SensorKind::Battery
    } else if starts(&["acpitz", "nct", "it87", "tskn"]) {
        SensorKind::System
    } else {
        SensorKind::Other
    }
}

fn level_rank(l: ThermalLevel) -> u8 {
    match l {
        ThermalLevel::Nominal => 0,
        ThermalLevel::Fair => 1,
        ThermalLevel::Serious => 2,
        ThermalLevel::Critical => 3,
    }
}

pub fn sample(host: &Host) -> ThermalInfo {
    let mut info = ThermalInfo::default();
    let mut parts: BTreeMap<(SensorKind, String), TempSensor> = BTreeMap::new();
    // `source` is the chip / zone type the component is derived from, `name`
    // the full sensor name shown to the user.
    let mut consider = |source: &str, name: String, temp: f64, trips: Trips| {
        let level = rate(temp, trips);
        if level_rank(level) > level_rank(info.level) {
            info.level = level;
        }
        if info.max_temp.is_none_or(|m| temp > m) {
            info.max_temp = Some(temp);
            info.sensor = Some(name.clone());
        }
        let kind = classify(source);
        let label = if kind == SensorKind::Other { source.to_string() } else { kind.label().to_string() };
        let part = parts.entry((kind, label.clone())).or_default();
        if part.label.is_empty() || temp > part.temp {
            *part = TempSensor { kind, label, sensor: name, temp };
        }
    };

    if let Ok(entries) = fs::read_dir(host.sys("class/thermal")) {
        for e in entries.flatten() {
            if !e.file_name().to_string_lossy().starts_with("thermal_zone") {
                continue;
            }
            let dir = e.path();
            let Some(temp) = millideg(&dir.join("temp")) else { continue };
            let name = host::read_trimmed(dir.join("type")).unwrap_or_else(|| "thermal".into());
            consider(&name, name.clone(), temp, zone_trips(&dir));
        }
    }

    if let Ok(entries) = fs::read_dir(host.sys("class/hwmon")) {
        for e in entries.flatten() {
            let dir = e.path();
            let chip = host::read_trimmed(dir.join("name")).unwrap_or_else(|| "hwmon".into());
            // DIMM sensors (spd5118, jc42) report the JEDEC event limit as
            // `max` (55 °C on DDR5), well below where memory gets hot; only
            // their `crit` is a real limit.
            let use_max = classify(&chip) != SensorKind::Memory;
            for i in 1..32 {
                let Some(temp) = millideg(&dir.join(format!("temp{i}_input"))) else { continue };
                let crit = millideg(&dir.join(format!("temp{i}_crit")));
                let max = millideg(&dir.join(format!("temp{i}_max"))).filter(|_| use_max);
                let label = host::read_trimmed(dir.join(format!("temp{i}_label")));
                let name = label.map(|l| format!("{chip} {l}")).unwrap_or_else(|| chip.clone());
                consider(&chip, name, temp, Trips { passive: None, hot: max, critical: crit });
            }
        }
    }
    info.sensors = parts.into_values().collect();
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::write;

    #[test]
    fn rating_with_trips() {
        let trips = Trips { passive: Some(90.0), hot: None, critical: Some(105.0) };
        assert_eq!(rate(50.0, trips), ThermalLevel::Nominal);
        assert_eq!(rate(82.0, trips), ThermalLevel::Fair);
        assert_eq!(rate(91.0, trips), ThermalLevel::Serious);
        assert_eq!(rate(101.0, trips), ThermalLevel::Critical);
    }

    #[test]
    fn rating_without_trips() {
        assert_eq!(rate(60.0, Trips::default()), ThermalLevel::Nominal);
        assert_eq!(rate(80.0, Trips::default()), ThermalLevel::Fair);
        assert_eq!(rate(96.0, Trips::default()), ThermalLevel::Critical);
    }

    #[test]
    fn hottest_sensor_wins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/class/thermal/thermal_zone0/type", "acpitz\n");
        write(root, "sys/class/thermal/thermal_zone0/temp", "45000\n");
        write(root, "sys/class/thermal/thermal_zone0/trip_point_0_type", "critical\n");
        write(root, "sys/class/thermal/thermal_zone0/trip_point_0_temp", "120000\n");
        write(root, "sys/class/hwmon/hwmon1/name", "coretemp\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_label", "Package id 0\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_input", "92000\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_max", "90000\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_crit", "100000\n");
        write(root, "sys/class/hwmon/hwmon1/temp2_input", "-273000\n");
        let info = sample(&Host::at(root));
        assert_eq!(info.level, ThermalLevel::Serious);
        assert_eq!(info.max_temp, Some(92.0));
        assert_eq!(info.sensor.as_deref(), Some("coretemp Package id 0"));
        let parts: Vec<_> = info.sensors.iter().map(|p| (p.label.as_str(), p.temp)).collect();
        assert_eq!(parts, [("CPU", 92.0), ("System", 45.0)]);
    }

    #[test]
    fn every_component_is_listed() {
        // AMD laptop: the Wi-Fi card is the hottest chip, but CPU, GPU and
        // both DIMMs must still be reported.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/class/thermal/thermal_zone0/type", "acpitz\n");
        write(root, "sys/class/thermal/thermal_zone0/temp", "20000\n");
        write(root, "sys/class/hwmon/hwmon0/name", "acpitz\n");
        write(root, "sys/class/hwmon/hwmon0/temp1_input", "20000\n");
        write(root, "sys/class/hwmon/hwmon1/name", "amdgpu\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_label", "edge\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_input", "40000\n");
        write(root, "sys/class/hwmon/hwmon2/name", "k10temp\n");
        write(root, "sys/class/hwmon/hwmon2/temp1_label", "Tctl\n");
        write(root, "sys/class/hwmon/hwmon2/temp1_input", "42000\n");
        write(root, "sys/class/hwmon/hwmon3/name", "spd5118\n");
        write(root, "sys/class/hwmon/hwmon3/temp1_input", "49750\n");
        write(root, "sys/class/hwmon/hwmon4/name", "spd5118\n");
        write(root, "sys/class/hwmon/hwmon4/temp1_input", "48750\n");
        write(root, "sys/class/hwmon/hwmon5/name", "mt7921_phy0\n");
        write(root, "sys/class/hwmon/hwmon5/temp1_input", "60000\n");
        write(root, "sys/class/hwmon/hwmon6/name", "nvme\n");
        write(root, "sys/class/hwmon/hwmon6/temp1_label", "Composite\n");
        write(root, "sys/class/hwmon/hwmon6/temp1_input", "38850\n");
        write(root, "sys/class/hwmon/hwmon7/name", "r8169_0_500:00\n");
        write(root, "sys/class/hwmon/hwmon7/temp1_input", "51000\n");
        let info = sample(&Host::at(root));
        assert_eq!(info.max_temp, Some(60.0));
        assert_eq!(info.sensor.as_deref(), Some("mt7921_phy0"));
        let parts: Vec<_> = info.sensors.iter().map(|p| (p.label.as_str(), p.temp)).collect();
        assert_eq!(
            parts,
            [
                ("CPU", 42.0),
                ("GPU", 40.0),
                ("RAM", 49.75),
                ("SSD", 38.85),
                ("Wi-Fi", 60.0),
                ("System", 20.0),
                ("r8169_0_500:00", 51.0),
            ]
        );
        assert_eq!(info.sensors[0].sensor, "k10temp Tctl");
        assert_eq!(info.sensors[2].sensor, "spd5118");
    }

    #[test]
    fn dimm_event_limit_is_not_overheating() {
        // DDR5 SPD hub: max = JEDEC event limit (55 °C), crit = 85 °C.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/class/hwmon/hwmon3/name", "spd5118\n");
        write(root, "sys/class/hwmon/hwmon3/temp1_input", "58000\n");
        write(root, "sys/class/hwmon/hwmon3/temp1_max", "55000\n");
        write(root, "sys/class/hwmon/hwmon3/temp1_crit", "85000\n");
        let info = sample(&Host::at(root));
        assert_eq!(info.level, ThermalLevel::Nominal);
        write(root, "sys/class/hwmon/hwmon3/temp1_input", "82000\n");
        assert_eq!(sample(&Host::at(root)).level, ThermalLevel::Critical);
    }

    #[test]
    fn classifies_common_chips() {
        assert_eq!(classify("coretemp"), SensorKind::Cpu);
        assert_eq!(classify("x86_pkg_temp"), SensorKind::Cpu);
        assert_eq!(classify("cpu-thermal"), SensorKind::Cpu);
        assert_eq!(classify("nouveau"), SensorKind::Gpu);
        assert_eq!(classify("jc42"), SensorKind::Memory);
        assert_eq!(classify("drivetemp"), SensorKind::Storage);
        assert_eq!(classify("iwlwifi_1"), SensorKind::Wifi);
        assert_eq!(classify("pch_cannonlake"), SensorKind::Chipset);
        assert_eq!(classify("BAT0"), SensorKind::Battery);
        assert_eq!(classify("nct6798"), SensorKind::System);
        assert_eq!(classify("xen_something"), SensorKind::Other);
    }

    #[test]
    fn no_sensors_is_nominal() {
        let dir = tempfile::tempdir().unwrap();
        let info = sample(&Host::at(dir.path()));
        assert_eq!(info.level, ThermalLevel::Nominal);
        assert_eq!(info.max_temp, None);
    }
}

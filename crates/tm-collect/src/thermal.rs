//! Thermal level from thermal zones and hwmon sensors.
//!
//! Linux has no single "thermal state" like macOS, so each sensor is rated
//! against its own trip points (or fixed CPU-ish limits when it has none) and
//! the hottest rating wins.

use std::fs;
use std::path::Path;

use tm_core::health::ThermalLevel;
use tm_core::model::ThermalInfo;

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
    let mut consider = |name: String, temp: f64, trips: Trips| {
        let level = rate(temp, trips);
        if level_rank(level) > level_rank(info.level) {
            info.level = level;
        }
        if info.max_temp.is_none_or(|m| temp > m) {
            info.max_temp = Some(temp);
            info.sensor = Some(name);
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
            consider(name, temp, zone_trips(&dir));
        }
    }

    if let Ok(entries) = fs::read_dir(host.sys("class/hwmon")) {
        for e in entries.flatten() {
            let dir = e.path();
            let chip = host::read_trimmed(dir.join("name")).unwrap_or_else(|| "hwmon".into());
            for i in 1..32 {
                let Some(temp) = millideg(&dir.join(format!("temp{i}_input"))) else { continue };
                let crit = millideg(&dir.join(format!("temp{i}_crit")));
                let max = millideg(&dir.join(format!("temp{i}_max")));
                let label = host::read_trimmed(dir.join(format!("temp{i}_label")));
                let name = label.map(|l| format!("{chip} {l}")).unwrap_or_else(|| chip.clone());
                consider(name, temp, Trips { passive: None, hot: max, critical: crit });
            }
        }
    }
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
    }

    #[test]
    fn no_sensors_is_nominal() {
        let dir = tempfile::tempdir().unwrap();
        let info = sample(&Host::at(dir.path()));
        assert_eq!(info.level, ThermalLevel::Nominal);
        assert_eq!(info.max_temp, None);
    }
}

//! Battery and AC state from `/sys/class/power_supply`.
//!
//! Read directly from sysfs rather than UPower: no D-Bus round trip per tick,
//! and it works identically on every desktop.

use std::fs;
use std::path::Path;

use tm_core::model::PowerInfo;

use crate::host::{self, Host};

#[derive(Debug, Default, Clone)]
struct Battery {
    status: String,
    capacity: Option<u64>,
    cycle_count: Option<u32>,
    // Energy (µWh) or charge (µAh) — never mixed within one battery.
    now: Option<u64>,
    full: Option<u64>,
    design: Option<u64>,
    /// µW, always non-negative.
    power: Option<u64>,
    temp: Option<f64>,
    model: Option<String>,
}

fn read_battery(dir: &Path) -> Battery {
    let s = |f: &str| host::read_trimmed(dir.join(f));
    let u = |f: &str| host::read_u64(dir.join(f));
    let (now, full, design) = if dir.join("energy_now").exists() {
        (u("energy_now"), u("energy_full"), u("energy_full_design"))
    } else {
        (u("charge_now"), u("charge_full"), u("charge_full_design"))
    };
    // power_now, or current × voltage (µA × µV = 1e-6 µW).
    let power = u("power_now").or_else(|| {
        let i = host::read_i64(dir.join("current_now"))?.unsigned_abs();
        let v = u("voltage_now")?;
        Some(i * v / 1_000_000)
    });
    Battery {
        status: s("status").unwrap_or_default(),
        capacity: u("capacity"),
        cycle_count: u("cycle_count").map(|c| c as u32).filter(|c| *c > 0),
        now,
        full,
        design,
        power: power.filter(|p| *p > 0),
        temp: host::read_i64(dir.join("temp")).map(|t| t as f64 / 10.0),
        model: s("model_name").filter(|m| !m.is_empty()),
    }
}

pub fn sample(host: &Host) -> PowerInfo {
    let Ok(entries) = fs::read_dir(host.sys("class/power_supply")) else { return combine(&[], None) };
    let mut batteries = Vec::new();
    let mut ac_online: Option<bool> = None;

    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for dir in paths {
        let kind = host::read_trimmed(dir.join("type")).unwrap_or_default();
        match kind.as_str() {
            // `scope = Device` marks peripheral batteries (mice, headsets).
            "Battery" if host::read_trimmed(dir.join("scope")).as_deref() != Some("Device") => {
                batteries.push(read_battery(&dir));
            }
            "Mains" | "USB" | "USB_C" | "USB_PD" => {
                if host::read_trimmed(dir.join("online")).as_deref() == Some("1") {
                    ac_online = Some(true);
                } else if ac_online.is_none() {
                    ac_online = Some(false);
                }
            }
            _ => {}
        }
    }
    combine(&batteries, ac_online)
}

/// Aggregate batteries (ThinkPads often have two) into one reading.
fn combine(batteries: &[Battery], ac_online: Option<bool>) -> PowerInfo {
    if batteries.is_empty() {
        return PowerInfo { is_plugged_in: ac_online.unwrap_or(true), ..Default::default() };
    }
    let sum = |f: fn(&Battery) -> Option<u64>| -> Option<u64> {
        batteries.iter().map(f).collect::<Option<Vec<u64>>>().map(|v| v.iter().sum())
    };
    let now = sum(|b| b.now);
    let full = sum(|b| b.full);
    let design = sum(|b| b.design);
    let power: u64 = batteries.iter().filter_map(|b| b.power).sum();

    let charging = batteries.iter().any(|b| b.status == "Charging");
    let discharging = batteries.iter().any(|b| b.status == "Discharging");
    let fully = batteries.iter().all(|b| b.status == "Full");
    let plugged = ac_online.unwrap_or(!discharging);

    let charge_percent = match (now, full) {
        (Some(n), Some(f)) if f > 0 => (n as f64 / f as f64 * 100.0).round().min(100.0) as u8,
        _ => batteries[0].capacity.unwrap_or(0).min(100) as u8,
    };

    let (time_to_empty, time_to_full) = match (now, full, power) {
        (Some(n), _, p) if discharging && p > 0 => (Some((n as f64 / p as f64 * 60.0) as u32), None),
        (Some(n), Some(f), p) if charging && p > 0 => {
            (None, Some((f.saturating_sub(n) as f64 / p as f64 * 60.0) as u32))
        }
        _ => (None, None),
    };

    PowerInfo {
        has_battery: true,
        charge_percent,
        is_charging: charging,
        is_plugged_in: plugged,
        fully_charged: fully,
        cycle_count: batteries.iter().filter_map(|b| b.cycle_count).max(),
        design_capacity: design.filter(|d| *d > 0),
        full_capacity: full.filter(|f| *f > 0),
        temperature: batteries.iter().find_map(|b| b.temp),
        power_watts: (power > 0).then(|| {
            let w = power as f64 / 1_000_000.0;
            if discharging {
                -w
            } else {
                w
            }
        }),
        time_to_empty_min: time_to_empty,
        time_to_full_min: time_to_full,
        model: batteries.iter().find_map(|b| b.model.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::write;

    fn bat(root: &Path, name: &str, status: &str, now: u64, full: u64, design: u64, power: u64) {
        let b = format!("sys/class/power_supply/{name}");
        write(root, &format!("{b}/type"), "Battery\n");
        write(root, &format!("{b}/status"), &format!("{status}\n"));
        write(root, &format!("{b}/energy_now"), &format!("{now}\n"));
        write(root, &format!("{b}/energy_full"), &format!("{full}\n"));
        write(root, &format!("{b}/energy_full_design"), &format!("{design}\n"));
        write(root, &format!("{b}/power_now"), &format!("{power}\n"));
        write(root, &format!("{b}/cycle_count"), "150\n");
    }

    #[test]
    fn discharging_laptop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        bat(root, "BAT0", "Discharging", 30_000_000, 46_000_000, 50_000_000, 10_000_000);
        write(root, "sys/class/power_supply/AC/type", "Mains\n");
        write(root, "sys/class/power_supply/AC/online", "0\n");
        let p = sample(&Host::at(root));
        assert!(p.has_battery && !p.is_plugged_in && !p.is_charging);
        assert_eq!(p.charge_percent, 65);
        assert_eq!(p.time_to_empty_min, Some(180));
        assert_eq!(p.power_watts, Some(-10.0));
        assert!((p.health_percent().unwrap() - 92.0).abs() < 1e-9);
        assert_eq!(p.cycle_count, Some(150));
    }

    #[test]
    fn two_batteries_are_summed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        bat(root, "BAT0", "Charging", 10, 20, 20, 5);
        bat(root, "BAT1", "Charging", 20, 20, 40, 5);
        let p = sample(&Host::at(root));
        assert_eq!(p.charge_percent, 75);
        assert_eq!(p.full_capacity, Some(40));
        assert_eq!(p.design_capacity, Some(60));
        assert_eq!(p.time_to_full_min, Some(60));
        assert!(p.is_plugged_in, "charging implies power without an AC node");
    }

    #[test]
    fn peripheral_batteries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        bat(root, "hidpp_battery_0", "Discharging", 1, 2, 2, 1);
        write(root, "sys/class/power_supply/hidpp_battery_0/scope", "Device\n");
        assert!(!sample(&Host::at(root)).has_battery);
    }

    #[test]
    fn desktop_without_battery() {
        let dir = tempfile::tempdir().unwrap();
        let p = sample(&Host::at(dir.path()));
        assert!(!p.has_battery && p.is_plugged_in);
    }
}

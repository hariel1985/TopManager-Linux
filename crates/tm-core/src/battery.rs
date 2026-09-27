//! Pure battery math, ported from `PowerInfo.swift` (`BatteryMath`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryCondition {
    Normal,
    ServiceRecommended,
    Unknown,
}

impl BatteryCondition {
    pub fn label(self) -> &'static str {
        match self {
            BatteryCondition::Normal => "Normal",
            BatteryCondition::ServiceRecommended => "Service Recommended",
            BatteryCondition::Unknown => "Unknown",
        }
    }
}

pub struct BatteryMath;

impl BatteryMath {
    /// Current full-charge capacity ÷ design capacity, in percent.
    pub fn health_percent(full_capacity: u64, design_capacity: u64) -> f64 {
        if design_capacity == 0 {
            return 0.0;
        }
        full_capacity as f64 / design_capacity as f64 * 100.0
    }

    /// Approximates the "Service Recommended" classification from raw numbers.
    pub fn condition(health_percent: Option<f64>, cycle_count: Option<u32>) -> BatteryCondition {
        let Some(health) = health_percent else {
            return BatteryCondition::Unknown;
        };
        if health < 80.0 {
            return BatteryCondition::ServiceRecommended;
        }
        if cycle_count.is_some_and(|c| c > 1000) {
            return BatteryCondition::ServiceRecommended;
        }
        BatteryCondition::Normal
    }

    pub fn watts(volts: f64, milli_amps: f64) -> f64 {
        volts * milli_amps / 1000.0
    }

    /// "2h 15m" / "45m"; "—" for non-positive durations.
    pub fn format_minutes(minutes: i64) -> String {
        if minutes <= 0 {
            return "—".into();
        }
        let (h, m) = (minutes / 60, minutes % 60);
        if h > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{m}m")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PowerInfo;

    #[test]
    fn health_percent() {
        assert!((BatteryMath::health_percent(4000, 5000) - 80.0).abs() < 1e-9);
        assert_eq!(BatteryMath::health_percent(100, 0), 0.0);
    }

    #[test]
    fn condition() {
        assert_eq!(BatteryMath::condition(Some(92.0), Some(120)), BatteryCondition::Normal);
        assert_eq!(BatteryMath::condition(Some(74.0), Some(120)), BatteryCondition::ServiceRecommended);
        assert_eq!(BatteryMath::condition(Some(95.0), Some(1200)), BatteryCondition::ServiceRecommended);
        assert_eq!(BatteryMath::condition(None, Some(100)), BatteryCondition::Unknown);
    }

    #[test]
    fn watts() {
        assert!((BatteryMath::watts(12.0, 2000.0) - 24.0).abs() < 1e-9);
        assert!((BatteryMath::watts(12.0, -1500.0) + 18.0).abs() < 1e-9);
    }

    #[test]
    fn format_minutes() {
        assert_eq!(BatteryMath::format_minutes(135), "2h 15m");
        assert_eq!(BatteryMath::format_minutes(45), "45m");
        assert_eq!(BatteryMath::format_minutes(0), "—");
    }

    #[test]
    fn power_info_derived_values() {
        let p = PowerInfo {
            has_battery: true,
            charge_percent: 66,
            cycle_count: Some(150),
            design_capacity: Some(5000),
            full_capacity: Some(4600),
            ..Default::default()
        };
        assert!((p.health_percent().unwrap() - 92.0).abs() < 1e-9);
        assert_eq!(p.condition(), BatteryCondition::Normal);
        assert_eq!(PowerInfo::default().health_percent(), None);
    }
}

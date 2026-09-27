//! 0–100 system health score and plain-language diagnosis, ported from
//! `HealthModel.swift`. Thresholds are identical so both apps agree.

use serde::{Deserialize, Serialize};

use crate::model::MemoryPressure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThermalLevel {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthInput {
    pub cpu_usage: f64,
    pub memory_pressure: MemoryPressure,
    pub swap_used_bytes: u64,
    pub thermal: ThermalLevel,
    /// 0…1 for the fullest volume.
    pub disk_used_fraction: f64,
}

pub fn score(input: &HealthInput) -> u8 {
    let mut score = 100.0;

    // Sustained high CPU: lose up to ~20 points as usage climbs 60→100 %.
    if input.cpu_usage > 60.0 {
        score -= f64::min(20.0, (input.cpu_usage - 60.0) * 0.5);
    }

    match input.memory_pressure {
        MemoryPressure::Warning => score -= 15.0,
        MemoryPressure::Critical => score -= 30.0,
        _ => {}
    }

    if input.swap_used_bytes > 2_000_000_000 {
        score -= 10.0;
    } else if input.swap_used_bytes > 500_000_000 {
        score -= 5.0;
    }

    match input.thermal {
        ThermalLevel::Fair => score -= 5.0,
        ThermalLevel::Serious => score -= 20.0,
        ThermalLevel::Critical => score -= 35.0,
        ThermalLevel::Nominal => {}
    }

    if input.disk_used_fraction > 0.95 {
        score -= 15.0;
    } else if input.disk_used_fraction > 0.90 {
        score -= 8.0;
    }

    score.round().clamp(0.0, 100.0) as u8
}

pub fn diagnosis(input: &HealthInput) -> Vec<String> {
    let mut issues = Vec::new();
    if input.cpu_usage > 85.0 {
        issues.push(format!("CPU is heavily loaded ({}%)", input.cpu_usage as i64));
    }
    match input.memory_pressure {
        MemoryPressure::Critical => issues.push("Memory pressure is critical — the system is low on RAM".into()),
        MemoryPressure::Warning => issues.push("Memory pressure is elevated".into()),
        _ => {}
    }
    if input.swap_used_bytes > 2_000_000_000 {
        issues.push("High swap usage — closing apps may help".into());
    }
    if matches!(input.thermal, ThermalLevel::Serious | ThermalLevel::Critical) {
        issues.push("System is running hot and may be throttling".into());
    }
    if input.disk_used_fraction > 0.90 {
        issues.push(format!("A disk is nearly full ({}%)", (input.disk_used_fraction * 100.0) as i64));
    }
    issues
}

pub fn rating(score: u8) -> &'static str {
    match score {
        85.. => "Excellent",
        70..=84 => "Good",
        50..=69 => "Fair",
        _ => "Poor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> HealthInput {
        HealthInput {
            cpu_usage: 10.0,
            memory_pressure: MemoryPressure::Nominal,
            swap_used_bytes: 0,
            thermal: ThermalLevel::Nominal,
            disk_used_fraction: 0.4,
        }
    }

    #[test]
    fn healthy_is_excellent() {
        let s = score(&healthy());
        assert_eq!(s, 100);
        assert_eq!(rating(s), "Excellent");
    }

    #[test]
    fn high_cpu_lowers_score() {
        let input = HealthInput { cpu_usage: 100.0, ..healthy() };
        assert!(score(&input) < score(&healthy()));
    }

    #[test]
    fn critical_memory_and_thermal_compound() {
        let input = HealthInput {
            cpu_usage: 95.0,
            memory_pressure: MemoryPressure::Critical,
            swap_used_bytes: 3_000_000_000,
            thermal: ThermalLevel::Critical,
            disk_used_fraction: 0.97,
        };
        let s = score(&input);
        assert!(s < 50);
        assert_eq!(rating(s), "Poor");
    }

    #[test]
    fn score_clamped_to_range() {
        let worst = HealthInput {
            cpu_usage: 100.0,
            memory_pressure: MemoryPressure::Critical,
            swap_used_bytes: u64::MAX,
            thermal: ThermalLevel::Critical,
            disk_used_fraction: 1.0,
        };
        assert!(score(&worst) <= 100);
    }

    #[test]
    fn diagnosis_empty_when_healthy() {
        assert!(diagnosis(&healthy()).is_empty());
    }

    #[test]
    fn diagnosis_reports_problems() {
        let input = HealthInput {
            cpu_usage: 95.0,
            memory_pressure: MemoryPressure::Critical,
            swap_used_bytes: 3_000_000_000,
            thermal: ThermalLevel::Serious,
            disk_used_fraction: 0.93,
        };
        let d = diagnosis(&input).join("\n").to_lowercase();
        assert!(d.contains("cpu") && d.contains("memory") && d.contains("disk"));
    }

    #[test]
    fn rating_bands() {
        assert_eq!(rating(85), "Excellent");
        assert_eq!(rating(84), "Good");
        assert_eq!(rating(69), "Fair");
        assert_eq!(rating(49), "Poor");
    }
}

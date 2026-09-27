//! Persisted history samples and their pure math, ported from `MetricsStore.swift`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One point-in-time snapshot of system-wide metrics. Short field names keep
/// the on-disk history compact (one JSON object per line).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricsSample {
    /// Unix seconds.
    pub t: f64,
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_total: u64,
    pub net_down: f64,
    pub net_up: f64,
    #[serde(default)]
    pub health: Option<u8>,
}

impl MetricsSample {
    pub fn mem_percent(&self) -> f64 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f64 / self.mem_total as f64 * 100.0
        }
    }
}

/// Drop samples older than `max_age` (relative to `now`) and keep at most the
/// newest `max_count`.
pub fn trim(samples: &[MetricsSample], now: f64, max_age: f64, max_count: usize) -> Vec<MetricsSample> {
    let cutoff = now - max_age;
    let kept: Vec<_> = samples.iter().copied().filter(|s| s.t >= cutoff).collect();
    let skip = kept.len().saturating_sub(max_count);
    kept[skip..].to_vec()
}

/// Average samples into fixed-width time buckets so long ranges plot ~100–150
/// points instead of tens of thousands. `bucket <= 0` returns the input.
pub fn downsample(samples: &[MetricsSample], bucket: f64) -> Vec<MetricsSample> {
    if bucket <= 0.0 || samples.len() <= 1 {
        return samples.to_vec();
    }
    let mut groups: BTreeMap<i64, Vec<&MetricsSample>> = BTreeMap::new();
    for s in samples {
        groups.entry((s.t / bucket).floor() as i64).or_default().push(s);
    }
    groups
        .into_iter()
        .map(|(key, g)| {
            let n = g.len() as f64;
            let avg = |f: fn(&MetricsSample) -> f64| g.iter().map(|s| f(s)).sum::<f64>() / n;
            let healths: Vec<f64> = g.iter().filter_map(|s| s.health.map(f64::from)).collect();
            MetricsSample {
                t: (key as f64 + 0.5) * bucket,
                cpu: avg(|s| s.cpu),
                mem_used: avg(|s| s.mem_used as f64) as u64,
                mem_total: g.iter().map(|s| s.mem_total).max().unwrap_or(0),
                net_down: avg(|s| s.net_down),
                net_up: avg(|s| s.net_up),
                health: (!healths.is_empty())
                    .then(|| (healths.iter().sum::<f64>() / healths.len() as f64).round() as u8),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRange {
    #[serde(rename = "live")]
    Live,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "30m")]
    M30,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "24h")]
    H24,
}

impl HistoryRange {
    pub const ALL: [HistoryRange; 5] =
        [HistoryRange::Live, HistoryRange::M5, HistoryRange::M30, HistoryRange::H1, HistoryRange::H24];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "live" => HistoryRange::Live,
            "5m" => HistoryRange::M5,
            "30m" => HistoryRange::M30,
            "1h" => HistoryRange::H1,
            "24h" => HistoryRange::H24,
            _ => return None,
        })
    }

    /// Window length in seconds; `None` for the live (in-RAM) view.
    pub fn seconds(self) -> Option<f64> {
        match self {
            HistoryRange::Live => None,
            HistoryRange::M5 => Some(5.0 * 60.0),
            HistoryRange::M30 => Some(30.0 * 60.0),
            HistoryRange::H1 => Some(3600.0),
            HistoryRange::H24 => Some(86_400.0),
        }
    }

    /// Downsample bucket keeping ~100–150 plotted points.
    pub fn bucket(self) -> f64 {
        match self {
            HistoryRange::Live | HistoryRange::M5 => 0.0,
            HistoryRange::M30 => 15.0,
            HistoryRange::H1 => 30.0,
            HistoryRange::H24 => 600.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, cpu: f64) -> MetricsSample {
        MetricsSample { t, cpu, mem_used: 0, mem_total: 100, net_down: 0.0, net_up: 0.0, health: None }
    }

    #[test]
    fn trim_drops_old_samples() {
        let now = 100_000.0;
        let out = trim(&[s(now - 10_000.0, 0.0), s(now - 100.0, 0.0), s(now - 50.0, 0.0)], now, 3600.0, 100);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|x| x.t >= now - 3600.0));
    }

    #[test]
    fn trim_caps_count_keeping_newest() {
        let now = 1000.0;
        let samples: Vec<_> = (0..10).map(|i| s(now - i as f64, i as f64)).collect();
        let out = trim(&samples, now, 3600.0, 5);
        assert_eq!(out.iter().map(|x| x.cpu).collect::<Vec<_>>(), vec![5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn downsample_zero_bucket_is_passthrough() {
        let samples: Vec<_> = (0..5).map(|i| s(i as f64, 0.0)).collect();
        assert_eq!(downsample(&samples, 0.0).len(), 5);
    }

    #[test]
    fn downsample_averages_within_bucket() {
        let a = MetricsSample {
            t: 1_000_000.0,
            cpu: 20.0,
            mem_used: 40,
            mem_total: 100,
            net_down: 100.0,
            net_up: 10.0,
            health: Some(90),
        };
        let b = MetricsSample {
            t: 1_000_005.0,
            cpu: 40.0,
            mem_used: 60,
            mem_total: 100,
            net_down: 300.0,
            net_up: 30.0,
            health: Some(80),
        };
        let out = downsample(&[a, b], 60.0);
        assert_eq!(out.len(), 1);
        assert!((out[0].cpu - 30.0).abs() < 1e-9);
        assert_eq!(out[0].mem_used, 50);
        assert!((out[0].net_down - 200.0).abs() < 1e-9);
        assert!((out[0].net_up - 20.0).abs() < 1e-9);
        assert_eq!(out[0].health, Some(85));
    }

    #[test]
    fn downsample_reduces_long_ranges_and_sorts() {
        let samples: Vec<_> = (0..600).map(|i| s(2_000_000.0 + i as f64, 1.0)).collect();
        let out = downsample(&samples, 60.0);
        assert!((10..=11).contains(&out.len()));
        assert!(out.windows(2).all(|w| w[0].t < w[1].t));
    }

    #[test]
    fn ranges() {
        assert_eq!(HistoryRange::Live.seconds(), None);
        assert_eq!(HistoryRange::Live.bucket(), 0.0);
        let w: Vec<f64> = HistoryRange::ALL.iter().filter_map(|r| r.seconds()).collect();
        assert!(w.windows(2).all(|p| p[0] < p[1]));
        assert_eq!(HistoryRange::H24.seconds(), Some(86_400.0));
        for r in HistoryRange::ALL {
            let name = serde_json::to_string(&r).unwrap();
            assert_eq!(HistoryRange::parse(name.trim_matches('"')), Some(r));
        }
    }

    #[test]
    fn old_history_lines_without_health_parse() {
        let v: MetricsSample =
            serde_json::from_str(r#"{"t":1,"cpu":2,"mem_used":3,"mem_total":4,"net_down":5,"net_up":6}"#).unwrap();
        assert_eq!(v.health, None);
    }
}

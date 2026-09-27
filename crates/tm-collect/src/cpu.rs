//! CPU usage from `/proc/stat` deltas, core types from sysfs.

use tm_core::model::{CoreType, CoreUsage, CpuInfo};

use crate::host::{self, Host};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    fn parse(fields: &[&str]) -> Self {
        let f = |i: usize| fields.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
        Self { user: f(0), nice: f(1), system: f(2), idle: f(3), iowait: f(4), irq: f(5), softirq: f(6), steal: f(7) }
    }

    pub fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq + self.steal
    }

    fn delta(&self, prev: &CpuTimes) -> CpuTimes {
        CpuTimes {
            user: self.user.saturating_sub(prev.user),
            nice: self.nice.saturating_sub(prev.nice),
            system: self.system.saturating_sub(prev.system),
            idle: self.idle.saturating_sub(prev.idle),
            iowait: self.iowait.saturating_sub(prev.iowait),
            irq: self.irq.saturating_sub(prev.irq),
            softirq: self.softirq.saturating_sub(prev.softirq),
            steal: self.steal.saturating_sub(prev.steal),
        }
    }

    /// Busy share in percent. iowait counts as idle, like `top`.
    fn usage(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        let idle = self.idle + self.iowait;
        (total - idle) as f64 / total as f64 * 100.0
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ProcStat {
    pub total: Option<CpuTimes>,
    pub cores: Vec<(usize, CpuTimes)>,
    /// Boot time, Unix seconds.
    pub btime: Option<u64>,
}

pub fn parse_proc_stat(text: &str) -> ProcStat {
    let mut out = ProcStat::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let rest: Vec<&str> = parts.collect();
        if key == "cpu" {
            out.total = Some(CpuTimes::parse(&rest));
        } else if let Some(n) = key.strip_prefix("cpu") {
            if let Ok(id) = n.parse() {
                out.cores.push((id, CpuTimes::parse(&rest)));
            }
        } else if key == "btime" {
            out.btime = rest.first().and_then(|v| v.parse().ok());
        }
    }
    out
}

/// Classify cores as performance / efficiency.
///
/// Intel hybrid CPUs expose `devices/cpu_core/cpus` and `devices/cpu_atom/cpus`.
/// ARM big.LITTLE exposes a relative `cpu_capacity` per core: the largest
/// value is a performance core. Homogeneous CPUs stay `Unknown`.
pub fn detect_core_types(host: &Host, ids: &[usize]) -> Vec<CoreType> {
    let core = host::read_trimmed(host.sys("devices/cpu_core/cpus")).map(|s| host::parse_cpu_list(&s));
    let atom = host::read_trimmed(host.sys("devices/cpu_atom/cpus")).map(|s| host::parse_cpu_list(&s));
    if let (Some(core), Some(atom)) = (core, atom) {
        return ids
            .iter()
            .map(|id| {
                if core.contains(id) {
                    CoreType::Performance
                } else if atom.contains(id) {
                    CoreType::Efficiency
                } else {
                    CoreType::Unknown
                }
            })
            .collect();
    }

    let caps: Vec<Option<u64>> =
        ids.iter().map(|id| host::read_u64(host.sys(format!("devices/system/cpu/cpu{id}/cpu_capacity")))).collect();
    let known: Vec<u64> = caps.iter().flatten().copied().collect();
    let (min, max) = (known.iter().min(), known.iter().max());
    match (min, max) {
        (Some(min), Some(max)) if min != max && known.len() == ids.len() => {
            caps.iter().map(|c| if *c == Some(*max) { CoreType::Performance } else { CoreType::Efficiency }).collect()
        }
        _ => vec![CoreType::Unknown; ids.len()],
    }
}

#[derive(Debug, Default)]
pub struct CpuMonitor {
    prev_total: Option<CpuTimes>,
    prev_cores: Vec<(usize, CpuTimes)>,
    core_types: Option<Vec<CoreType>>,
    pub btime: Option<u64>,
}

impl CpuMonitor {
    pub fn sample(&mut self, host: &Host) -> CpuInfo {
        let text = host::read_string(host.proc("stat")).unwrap_or_default();
        self.sample_from(host, &text)
    }

    pub fn sample_from(&mut self, host: &Host, text: &str) -> CpuInfo {
        let stat = parse_proc_stat(text);
        self.btime = stat.btime.or(self.btime);
        let ids: Vec<usize> = stat.cores.iter().map(|(id, _)| *id).collect();
        let types = self.core_types.get_or_insert_with(|| detect_core_types(host, &ids)).clone();

        let total = stat.total.unwrap_or_default();
        let d = match self.prev_total {
            Some(prev) => total.delta(&prev),
            None => total,
        };
        let dt = d.total().max(1) as f64;

        let cores = stat
            .cores
            .iter()
            .enumerate()
            .map(|(i, (id, now))| {
                let prev = self.prev_cores.iter().find(|(pid, _)| pid == id).map(|(_, t)| *t);
                let usage = prev.map(|p| now.delta(&p)).unwrap_or(*now).usage();
                CoreUsage { id: *id, usage, core_type: types.get(i).copied().unwrap_or_default() }
            })
            .collect();

        self.prev_total = Some(total);
        self.prev_cores = stat.cores;

        CpuInfo {
            global_usage: d.usage(),
            user_usage: (d.user + d.nice) as f64 / dt * 100.0,
            system_usage: (d.system + d.irq + d.softirq) as f64 / dt * 100.0,
            idle_usage: d.idle as f64 / dt * 100.0,
            iowait_usage: d.iowait as f64 / dt * 100.0,
            cores,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::write;

    const STAT_A: &str =
        "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0\nbtime 1700000000\n";
    const STAT_B: &str =
        "cpu  200 0 150 850 0 0 0 0 0 0\ncpu0 150 0 50 400 0 0 0 0\ncpu1 50 0 100 450 0 0 0 0\nbtime 1700000000\n";

    #[test]
    fn parses_proc_stat() {
        let s = parse_proc_stat(STAT_A);
        assert_eq!(s.total.unwrap().total(), 1000);
        assert_eq!(s.cores.len(), 2);
        assert_eq!(s.btime, Some(1_700_000_000));
    }

    #[test]
    fn usage_from_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::at(dir.path());
        let mut m = CpuMonitor::default();
        m.sample_from(&host, STAT_A);
        let info = m.sample_from(&host, STAT_B);
        // total delta: 100 user + 50 sys + 50 idle = 200 → 75 % busy
        assert!((info.global_usage - 75.0).abs() < 1e-9);
        assert!((info.user_usage - 50.0).abs() < 1e-9);
        assert!((info.cores[0].usage - 100.0).abs() < 1e-9);
        assert!((info.cores[1].usage - 50.0).abs() < 1e-9);
    }

    #[test]
    fn intel_hybrid_core_types() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sys/devices/cpu_core/cpus", "0-1\n");
        write(dir.path(), "sys/devices/cpu_atom/cpus", "2-3\n");
        let types = detect_core_types(&Host::at(dir.path()), &[0, 1, 2, 3]);
        assert_eq!(
            types,
            vec![CoreType::Performance, CoreType::Performance, CoreType::Efficiency, CoreType::Efficiency]
        );
    }

    #[test]
    fn arm_capacity_core_types() {
        let dir = tempfile::tempdir().unwrap();
        for (i, cap) in [(0, 446), (1, 446), (2, 1024)] {
            write(dir.path(), &format!("sys/devices/system/cpu/cpu{i}/cpu_capacity"), &format!("{cap}\n"));
        }
        let types = detect_core_types(&Host::at(dir.path()), &[0, 1, 2]);
        assert_eq!(types, vec![CoreType::Efficiency, CoreType::Efficiency, CoreType::Performance]);
    }

    #[test]
    fn homogeneous_cores_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..2 {
            write(dir.path(), &format!("sys/devices/system/cpu/cpu{i}/cpu_capacity"), "1024\n");
        }
        assert_eq!(detect_core_types(&Host::at(dir.path()), &[0, 1]), vec![CoreType::Unknown; 2]);
    }
}

//! Linux metric collectors. Everything reads through a [`Host`], so tests run
//! against fixture trees instead of the live `/proc` and `/sys`.

pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod host;
pub mod memory;
pub mod network;
pub mod power;
pub mod process;
pub mod signal;
pub mod system;
pub mod thermal;

use std::time::Instant;

use tm_core::model::{ProcessDetail, Snapshot};

pub use host::Host;

/// Owns every stateful monitor and produces one [`Snapshot`] per tick.
pub struct Collector {
    pub host: Host,
    cpu: cpu::CpuMonitor,
    processes: process::ProcessMonitor,
    network: network::NetworkMonitor,
    started: Instant,
}

impl Collector {
    pub fn new(host: Host) -> Self {
        let mut cpu = cpu::CpuMonitor::default();
        let first = cpu.sample(&host);
        let ncpu = first.cores.len().max(1);
        let boot = cpu.btime.map(|b| b as f64).unwrap_or(0.0);
        Self {
            processes: process::ProcessMonitor::new(ncpu, boot),
            cpu,
            network: network::NetworkMonitor::default(),
            host,
            started: Instant::now(),
        }
    }

    /// `full` also collects per-process disk I/O, swap and wakeups (GUI open).
    pub fn sample(&mut self, full: bool) -> Snapshot {
        let now = self.started.elapsed().as_secs_f64();
        let h = &self.host;
        Snapshot {
            timestamp: host::now_unix(),
            cpu: self.cpu.sample(h),
            memory: memory::sample(h),
            processes: self.processes.sample(h, full, now),
            disk: disk::sample(h),
            network: self.network.sample(h, now),
            gpus: gpu::sample(h),
            power: power::sample(h),
            thermal: thermal::sample(h),
            system: system::sample(h, self.cpu.btime),
        }
    }

    pub fn process_detail(&self, pid: u32) -> Option<ProcessDetail> {
        process::detail(&self.host, pid)
    }
}

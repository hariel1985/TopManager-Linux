//! Network throughput from `/proc/net/dev` deltas.

use std::collections::HashMap;
use std::fs;

use tm_core::model::{NetworkInfo, NetworkInterface};

use crate::host::{self, Host};

/// `(name, rx_bytes, tx_bytes)` per interface.
pub fn parse_net_dev(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((name.trim().to_string(), *f.first()?, *f.get(8)?))
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct NetworkMonitor {
    prev: HashMap<String, (u64, u64)>,
    prev_at: Option<f64>,
}

impl NetworkMonitor {
    /// `now` is a monotonic timestamp in seconds.
    pub fn sample(&mut self, host: &Host, now: f64) -> NetworkInfo {
        let text = host::read_string(host.proc("net/dev")).unwrap_or_default();
        let dt = self.prev_at.map(|t| (now - t).max(1e-3));
        let mut info = NetworkInfo::default();
        let mut next = HashMap::new();

        for (name, rx, tx) in parse_net_dev(&text) {
            if name == "lo" {
                continue;
            }
            let (rx_rate, tx_rate) = match (dt, self.prev.get(&name)) {
                // Counters reset when an interface is re-created: saturate to 0.
                (Some(dt), Some(&(prx, ptx))) => {
                    (rx.saturating_sub(prx) as f64 / dt, tx.saturating_sub(ptx) as f64 / dt)
                }
                _ => (0.0, 0.0),
            };
            let is_virtual = fs::read_link(host.sys(format!("class/net/{name}")))
                .map(|p| p.to_string_lossy().contains("/virtual/"))
                .unwrap_or(false);
            let is_up =
                host::read_trimmed(host.sys(format!("class/net/{name}/operstate"))).map(|s| s == "up").unwrap_or(false);
            if !is_virtual {
                info.total_rx_rate += rx_rate;
                info.total_tx_rate += tx_rate;
                info.total_rx_bytes += rx;
                info.total_tx_bytes += tx;
            }
            next.insert(name.clone(), (rx, tx));
            info.interfaces.push(NetworkInterface {
                name,
                rx_bytes: rx,
                tx_bytes: tx,
                rx_rate,
                tx_rate,
                is_up,
                is_virtual,
            });
        }
        self.prev = next;
        self.prev_at = Some(now);
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::{symlink, write};

    fn dev(rx_eth: u64, tx_eth: u64, rx_br: u64) -> String {
        format!(
            "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n  eth0: {rx_eth} 10 0 0 0 0 0 0 {tx_eth} 10 0 0 0 0 0 0\ndocker0: {rx_br} 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
        )
    }

    #[test]
    fn rates_exclude_loopback_and_virtual() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        symlink(root, "sys/class/net/eth0", "../../devices/pci0000:00/0000:00:03.0/net/eth0");
        symlink(root, "sys/class/net/docker0", "../../devices/virtual/net/docker0");
        write(root, "sys/devices/pci0000:00/0000:00:03.0/net/eth0/operstate", "up\n");
        write(root, "proc/net/dev", &dev(1000, 500, 0));
        let host = Host::at(root);
        let mut m = NetworkMonitor::default();
        m.sample(&host, 0.0);
        write(root, "proc/net/dev", &dev(3000, 1500, 99_999));
        let info = m.sample(&host, 2.0);
        assert_eq!(info.interfaces.len(), 2);
        assert_eq!((info.total_rx_rate, info.total_tx_rate), (1000.0, 500.0));
        let eth = info.interfaces.iter().find(|i| i.name == "eth0").unwrap();
        assert!(eth.is_up && !eth.is_virtual);
        assert!(info.interfaces.iter().find(|i| i.name == "docker0").unwrap().is_virtual);
    }

    #[test]
    fn counter_reset_saturates() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "proc/net/dev", &dev(5000, 5000, 0));
        let host = Host::at(dir.path());
        let mut m = NetworkMonitor::default();
        m.sample(&host, 0.0);
        write(dir.path(), "proc/net/dev", &dev(10, 10, 0));
        assert_eq!(m.sample(&host, 1.0).total_rx_rate, 0.0);
    }
}

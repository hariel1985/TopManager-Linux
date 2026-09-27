//! Memory from `/proc/meminfo`, pressure from PSI, compression from zram.

use std::collections::HashMap;
use std::fs;

use tm_core::model::{MemoryInfo, MemoryPressure};

use crate::host::{self, Host};

/// `/proc/meminfo` values, converted to bytes.
pub fn parse_meminfo(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let mut parts = rest.split_whitespace();
            let value: u64 = parts.next()?.parse().ok()?;
            let bytes = if parts.next() == Some("kB") { value * 1024 } else { value };
            Some((key.to_string(), bytes))
        })
        .collect()
}

/// `(some avg10, full avg10)` from `/proc/pressure/memory`.
pub fn parse_psi(text: &str) -> (Option<f64>, Option<f64>) {
    let avg10 = |prefix: &str| {
        text.lines()
            .find(|l| l.starts_with(prefix))?
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix("avg10="))?
            .parse()
            .ok()
    };
    (avg10("some"), avg10("full"))
}

/// Sum of `(orig_data_size, mem_used_total)` over all zram devices.
fn zram_usage(host: &Host) -> (u64, u64) {
    let Ok(entries) = fs::read_dir(host.sys("block")) else { return (0, 0) };
    let mut orig = 0;
    let mut used = 0;
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with("zram") {
            continue;
        }
        if let Some(stat) = host::read_trimmed(e.path().join("mm_stat")) {
            let f: Vec<u64> = stat.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            orig += f.first().copied().unwrap_or(0);
            used += f.get(2).copied().unwrap_or(0);
        }
    }
    (orig, used)
}

pub fn sample(host: &Host) -> MemoryInfo {
    let m = parse_meminfo(&host::read_string(host.proc("meminfo")).unwrap_or_default());
    let psi = host::read_string(host.proc("pressure/memory")).ok().map(|t| parse_psi(&t));
    let (compressed_original, compressed) = zram_usage(host);
    build(&m, psi, compressed_original, compressed)
}

pub fn build(
    m: &HashMap<String, u64>,
    psi: Option<(Option<f64>, Option<f64>)>,
    compressed_original: u64,
    compressed: u64,
) -> MemoryInfo {
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    let free = g("MemFree");
    // Kernels before 3.14 lack MemAvailable; approximate like `free` does.
    let available =
        m.get("MemAvailable").copied().unwrap_or_else(|| free + g("Cached") + g("Buffers") + g("SReclaimable"));
    let (some, full) = psi.unwrap_or((None, None));
    let pressure = match (some, full) {
        (Some(s), f) => MemoryPressure::from_psi(s, f.unwrap_or(0.0)),
        _ => MemoryPressure::Unknown,
    };
    MemoryInfo {
        total,
        used: total.saturating_sub(available),
        free,
        available,
        active: g("Active"),
        inactive: g("Inactive"),
        cached: g("Cached") + g("SReclaimable"),
        buffers: g("Buffers"),
        shmem: g("Shmem"),
        compressed,
        compressed_original,
        swap_used: g("SwapTotal").saturating_sub(g("SwapFree")),
        swap_total: g("SwapTotal"),
        pressure,
        psi_some_avg10: some,
        psi_full_avg10: full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::write;

    const MEMINFO: &str = "MemTotal:       16000000 kB\nMemFree:         2000000 kB\nMemAvailable:    6000000 kB\nBuffers:          100000 kB\nCached:          3000000 kB\nSwapTotal:       4000000 kB\nSwapFree:        3000000 kB\nActive:          5000000 kB\nInactive:        4000000 kB\nShmem:            500000 kB\nSReclaimable:     200000 kB\nHugePages_Total:       0\n";

    #[test]
    fn meminfo_units() {
        let m = parse_meminfo(MEMINFO);
        assert_eq!(m["MemTotal"], 16_000_000 * 1024);
        assert_eq!(m["HugePages_Total"], 0);
    }

    #[test]
    fn psi() {
        let (s, f) = parse_psi(
            "some avg10=12.50 avg60=1.00 avg300=0.00 total=1\nfull avg10=0.30 avg60=0.00 avg300=0.00 total=1\n",
        );
        assert_eq!((s, f), (Some(12.5), Some(0.3)));
    }

    #[test]
    fn sample_from_fixture() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "proc/meminfo", MEMINFO);
        write(
            dir.path(),
            "proc/pressure/memory",
            "some avg10=45.00 avg60=0 avg300=0 total=0\nfull avg10=0.00 avg60=0 avg300=0 total=0\n",
        );
        write(dir.path(), "sys/block/zram0/mm_stat", "1000000 250000 300000 0 300000 0 0 0 0\n");
        let info = sample(&Host::at(dir.path()));
        assert_eq!(info.used, 10_000_000 * 1024);
        assert!((info.usage_percentage() - 62.5).abs() < 1e-9);
        assert_eq!(info.swap_used, 1_000_000 * 1024);
        assert_eq!(info.pressure, MemoryPressure::Critical);
        assert_eq!((info.compressed_original, info.compressed), (1_000_000, 300_000));
    }

    #[test]
    fn missing_psi_is_unknown() {
        let info = build(&parse_meminfo(MEMINFO), None, 0, 0);
        assert_eq!(info.pressure, MemoryPressure::Unknown);
    }
}

//! GPUs from `/sys/class/drm/card*`.
//!
//! What is exposed varies by driver: amdgpu reports utilization and VRAM in
//! sysfs; i915/xe and most ARM drivers report neither (per-process DRM fdinfo
//! will fill that gap later); NVIDIA's proprietary driver needs NVML.

use std::fs;

use tm_core::model::GpuInfo;

use crate::host::{self, Host};

fn vendor_name(vendor: &str) -> Option<&'static str> {
    Some(match vendor {
        "0x1002" => "AMD",
        "0x8086" => "Intel",
        "0x10de" => "NVIDIA",
        "0x1af4" => "Virtio",
        "0x15ad" => "VMware",
        "0x1234" => "QEMU",
        "0x5143" => "Qualcomm",
        _ => return None,
    })
}

/// Drivers of GPUs that share system RAM.
const UNIFIED_DRIVERS: &[&str] =
    &["i915", "xe", "panfrost", "panthor", "msm", "v3d", "vc4", "lima", "asahi", "virtio-pci", "virtio_gpu"];

pub fn sample(host: &Host) -> Vec<GpuInfo> {
    let Ok(entries) = fs::read_dir(host.sys("class/drm")) else { return Vec::new() };
    let mut cards: Vec<_> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            // `card0` but not connectors like `card0-HDMI-A-1`.
            n.strip_prefix("card").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|e| e.path())
        .collect();
    cards.sort();

    cards
        .into_iter()
        .filter_map(|card| {
            let dev = card.join("device");
            let driver = fs::read_link(dev.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))?;
            let vendor = host::read_trimmed(dev.join("vendor")).unwrap_or_default();
            let vram_total = host::read_u64(dev.join("mem_info_vram_total"));
            let vram_used = host::read_u64(dev.join("mem_info_vram_used"));
            let utilization = host::read_u64(dev.join("gpu_busy_percent")).map(|v| v as f64);
            // APUs (amdgpu with a small carve-out) are unified too.
            let small_carveout = vram_total.is_some_and(|t| t < 1 << 30);
            let is_unified_memory = UNIFIED_DRIVERS.contains(&driver.as_str()) || small_carveout;
            let name = match vendor_name(&vendor) {
                Some(v) => format!("{v} GPU ({driver})"),
                None => host::read_trimmed(dev.join("of_node/compatible"))
                    .map(|c| c.split('\0').next().unwrap_or_default().to_string())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| format!("GPU ({driver})")),
            };
            Some(GpuInfo { name, driver, utilization, vram_used, vram_total, is_unified_memory })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::{symlink, write};

    #[test]
    fn amdgpu_and_connectors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "sys/devices/pci/0000:03:00.0/vendor", "0x1002\n");
        write(root, "sys/devices/pci/0000:03:00.0/gpu_busy_percent", "37\n");
        write(root, "sys/devices/pci/0000:03:00.0/mem_info_vram_total", "8589934592\n");
        write(root, "sys/devices/pci/0000:03:00.0/mem_info_vram_used", "1073741824\n");
        std::fs::create_dir_all(root.join("sys/bus/pci/drivers/amdgpu")).unwrap();
        symlink(root, "sys/devices/pci/0000:03:00.0/driver", "../../../bus/pci/drivers/amdgpu");
        std::fs::create_dir_all(root.join("sys/class/drm/card1")).unwrap();
        symlink(root, "sys/class/drm/card1/device", "../../../devices/pci/0000:03:00.0");
        std::fs::create_dir_all(root.join("sys/class/drm/card1-DP-1")).unwrap();

        let gpus = sample(&Host::at(root));
        assert_eq!(gpus.len(), 1);
        let g = &gpus[0];
        assert_eq!(g.name, "AMD GPU (amdgpu)");
        assert_eq!(g.utilization, Some(37.0));
        assert_eq!(g.vram_used, Some(1 << 30));
        assert!(!g.is_unified_memory);
    }
}

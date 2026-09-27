//! Static-ish system facts: OS, kernel, CPU model, uptime.

use tm_core::model::SystemInfo;

use crate::host::{self, Host};

pub fn parse_os_release(text: &str) -> Option<String> {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    };
    get("PRETTY_NAME").or_else(|| get("NAME"))
}

pub fn parse_cpu_model(cpuinfo: &str) -> Option<String> {
    ["model name", "Model", "Hardware", "cpu model"].iter().find_map(|key| {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == *key).then(|| v.trim().to_string()).filter(|v| !v.is_empty())
        })
    })
}

/// ARM `/proc/cpuinfo` has no model name, only an implementer code.
pub fn arm_vendor(cpuinfo: &str) -> Option<&'static str> {
    let code = cpuinfo.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == "CPU implementer").then(|| v.trim().to_lowercase())
    })?;
    Some(match code.as_str() {
        "0x41" => "ARM",
        "0x42" => "Broadcom",
        "0x43" => "Cavium",
        "0x46" => "Fujitsu",
        "0x48" => "HiSilicon",
        "0x4e" => "NVIDIA",
        "0x51" => "Qualcomm",
        "0x61" => "Apple",
        "0x6d" => "Microsoft",
        "0xc0" => "Ampere",
        _ => return None,
    })
}

pub fn sample(host: &Host, btime: Option<u64>) -> SystemInfo {
    let os = host::read_string(host.etc("os-release"))
        .ok()
        .or_else(|| {
            let usr = host.etc_root.parent()?.join("usr/lib/os-release");
            host::read_string(usr).ok()
        })
        .and_then(|t| parse_os_release(&t))
        .unwrap_or_else(|| "Linux".into());
    let cpuinfo = host::read_string(host.proc("cpuinfo")).unwrap_or_default();
    let cpu_model = parse_cpu_model(&cpuinfo)
        .or_else(|| {
            host::read_trimmed(host.sys("firmware/devicetree/base/model")).map(|m| m.trim_end_matches('\0').to_string())
        })
        .or_else(|| arm_vendor(&cpuinfo).map(|v| format!("{v} {} CPU", std::env::consts::ARCH)))
        .unwrap_or_else(|| format!("{} CPU", std::env::consts::ARCH));
    let cpu_count = cpuinfo.lines().filter(|l| l.starts_with("processor")).count();
    let uptime =
        host::read_trimmed(host.proc("uptime")).and_then(|u| u.split_whitespace().next()?.parse().ok()).unwrap_or(0.0);
    SystemInfo {
        hostname: host::read_trimmed(host.proc("sys/kernel/hostname")).unwrap_or_default(),
        os_name: os,
        kernel: host::read_trimmed(host.proc("sys/kernel/osrelease")).unwrap_or_default(),
        architecture: std::env::consts::ARCH.to_string(),
        cpu_model,
        cpu_count: if cpu_count > 0 { cpu_count } else { std::thread::available_parallelism().map_or(1, |n| n.get()) },
        uptime_secs: uptime,
        boot_time: btime.map(|b| b as f64).unwrap_or_else(|| host::now_unix() - uptime),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|d| !d.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release() {
        assert_eq!(
            parse_os_release("NAME=\"Debian GNU/Linux\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n"),
            Some("Debian GNU/Linux 13 (trixie)".into())
        );
        assert_eq!(parse_os_release("NAME=Arch\n"), Some("Arch".into()));
    }

    #[test]
    fn cpu_model_x86_and_arm() {
        assert_eq!(
            parse_cpu_model("processor\t: 0\nmodel name\t: AMD Ryzen 7 7840U\n"),
            Some("AMD Ryzen 7 7840U".into())
        );
        assert_eq!(parse_cpu_model("processor : 0\nBogoMIPS : 48\n"), None);
        assert_eq!(parse_cpu_model("Model\t: Raspberry Pi 5\n"), Some("Raspberry Pi 5".into()));
        assert_eq!(arm_vendor("processor\t: 0\nCPU implementer\t: 0x61\n"), Some("Apple"));
    }
}

//! Process list from `/proc/[pid]/*`.
//!
//! Two refresh levels, like the macOS `ProcessMonitor`:
//! - **light** (HUD only): `stat` + `statm` + the directory owner. Enough for
//!   CPU %, memory, threads, state and the top-N list.
//! - **full** (GUI open): additionally `status` (swap, context switches) and
//!   `io` (disk rates). Values from the last full refresh are kept on light
//!   ones, so rows never flash to zero.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;

use tm_core::model::{EnergyModel, ProcessDetail, ProcessItem, ProcessState};

use crate::host::{self, Host};

#[derive(Debug, Clone, PartialEq)]
pub struct StatFields {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub utime: u64,
    pub stime: u64,
    pub nice: i32,
    pub num_threads: u32,
    pub start_ticks: u64,
}

/// Parse `/proc/[pid]/stat`. `comm` may contain spaces and parentheses, so it
/// is delimited by the first `(` and the *last* `)`.
pub fn parse_stat(text: &str) -> Option<StatFields> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let comm = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // rest[0] is field 3 (state); field N is rest[N - 3].
    let f = |n: usize| rest.get(n - 3).copied();
    let num = |n: usize| f(n).and_then(|v| v.parse::<u64>().ok());
    Some(StatFields {
        pid,
        comm,
        state: f(3)?.chars().next()?,
        ppid: num(4)? as u32,
        utime: num(14)?,
        stime: num(15)?,
        nice: f(19).and_then(|v| v.parse().ok()).unwrap_or(0),
        num_threads: num(20)? as u32,
        start_ticks: num(22)?,
    })
}

/// `(resident pages, shared pages)` from `/proc/[pid]/statm`.
pub fn parse_statm(text: &str) -> Option<(u64, u64)> {
    let mut it = text.split_whitespace().skip(1).map(|v| v.parse::<u64>().ok());
    Some((it.next()??, it.next()??))
}

/// `key: value` pairs of `/proc/[pid]/status` or `io`.
fn parse_kv(text: &str) -> HashMap<&str, &str> {
    text.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect()
}

fn kv_u64(map: &HashMap<&str, &str>, key: &str) -> Option<u64> {
    map.get(key)?.split_whitespace().next()?.parse().ok()
}

/// Desktop app id from a cgroup path, following the systemd XDG naming
/// convention: `app[-<launcher>]-<ApplicationID>-<RANDOM>.scope` or
/// `app[-<launcher>]-<ApplicationID>[@<RANDOM>].service`, with `-` inside the
/// id escaped as `\x2d`.
pub fn app_id_from_cgroup(cgroup: &str) -> Option<String> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::")).unwrap_or(cgroup.lines().next()?);
    let unit = path.trim().rsplit('/').next()?;
    let body = unit.strip_prefix("app-")?;
    let body = if let Some(b) = body.strip_suffix(".scope") {
        b.rsplit_once('-').map(|(head, _random)| head)?
    } else {
        let b = body.strip_suffix(".service")?;
        b.split_once('@').map(|(head, _)| head).unwrap_or(b)
    };
    // An optional launcher prefix never contains a dot; app ids usually do.
    let id = match body.split_once('-') {
        Some((launcher, id)) if !launcher.contains('.') && !id.is_empty() => id,
        _ => body,
    };
    let id = unescape_systemd(id);
    (!id.is_empty()).then_some(id)
}

fn unescape_systemd(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
            let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Processes whose death ends or cripples the desktop session. On Wayland,
/// killing gnome-shell logs the user out.
const PROTECTED: &[&str] = &[
    "gnome-shell",
    "gnome-session-binary",
    "gnome-session-service",
    "gnome-session-ctl",
    "systemd",
    "Xwayland",
    "dbus-daemon",
    "dbus-broker",
    "dbus-broker-launch",
    "gdm-wayland-session",
    "gdm-x-session",
    "topmanagerd",
];

pub fn is_protected(pid: u32, name: &str) -> bool {
    pid <= 1 || PROTECTED.contains(&name)
}

/// uid → user name from `/etc/passwd`, falling back to the numeric uid
/// (network users that only exist in sssd/LDAP).
#[derive(Debug, Default)]
pub struct UserNames {
    map: HashMap<u32, String>,
    loaded: bool,
}

impl UserNames {
    pub fn name(&mut self, host: &Host, uid: u32) -> String {
        if !self.loaded {
            self.loaded = true;
            if let Ok(text) = fs::read_to_string(host.etc("passwd")) {
                for line in text.lines() {
                    let f: Vec<&str> = line.split(':').collect();
                    if let (Some(name), Some(Ok(id))) = (f.first(), f.get(2).map(|v| v.parse::<u32>())) {
                        self.map.entry(id).or_insert_with(|| name.to_string());
                    }
                }
            }
        }
        self.map.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }
}

#[derive(Debug, Clone, Default)]
struct Prev {
    cpu_ticks: u64,
    at: Option<f64>,
    // Cached per-process facts that never change for a (pid, start) identity.
    name: String,
    app_id: Option<String>,
    // Last full-refresh values.
    full_at: Option<f64>,
    read_bytes: u64,
    write_bytes: u64,
    ctxt: u64,
    read_rate: f64,
    write_rate: f64,
    ctxt_rate: f64,
    swapped: u64,
}

#[derive(Debug)]
pub struct ProcessMonitor {
    prev: HashMap<(u32, u64), Prev>,
    users: UserNames,
    clk_tck: f64,
    page_size: u64,
    ncpu: usize,
    pub boot_time: f64,
}

impl ProcessMonitor {
    pub fn new(ncpu: usize, boot_time: f64) -> Self {
        Self {
            prev: HashMap::new(),
            users: UserNames::default(),
            clk_tck: host::clock_ticks_per_second(),
            page_size: host::page_size(),
            ncpu: ncpu.max(1),
            boot_time,
        }
    }

    /// Sample all processes. `now` is a monotonic timestamp in seconds.
    pub fn sample(&mut self, host: &Host, full: bool, now: f64) -> Vec<ProcessItem> {
        let Ok(entries) = fs::read_dir(&host.proc_root) else { return Vec::new() };
        let mut seen = HashMap::with_capacity(self.prev.len());
        let mut out = Vec::new();

        for entry in entries.flatten() {
            let fname = entry.file_name();
            let Some(pid) = fname.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let dir = entry.path();
            let Some(stat) = fs::read_to_string(dir.join("stat")).ok().and_then(|t| parse_stat(&t)) else {
                continue; // exited between readdir and read
            };
            let uid = fs::metadata(&dir).map(|m| m.uid()).unwrap_or(0);
            let key = (pid, stat.start_ticks);
            let mut p = self.prev.remove(&key).unwrap_or_default();
            let is_new = p.at.is_none();

            if is_new {
                p.name = display_name(&dir, &stat.comm);
                p.app_id = fs::read_to_string(dir.join("cgroup")).ok().and_then(|c| app_id_from_cgroup(&c));
            }

            let cpu_ticks = stat.utime + stat.stime;
            let cpu = match p.at {
                None => 0.0,
                Some(at) => {
                    let dt = (now - at).max(1e-3);
                    cpu_ticks.saturating_sub(p.cpu_ticks) as f64 / self.clk_tck / dt * 100.0
                }
            };
            p.cpu_ticks = cpu_ticks;
            p.at = Some(now);

            let (resident, shared) =
                fs::read_to_string(dir.join("statm")).ok().and_then(|t| parse_statm(&t)).unwrap_or((0, 0));

            if full {
                self.refresh_full(&dir, &mut p, now);
            }

            let item = ProcessItem {
                pid,
                ppid: stat.ppid,
                user: self.users.name(host, uid),
                uid,
                cpu,
                cpu_total: cpu / self.ncpu as f64,
                memory: resident.saturating_sub(shared) * self.page_size,
                resident: resident * self.page_size,
                swapped: p.swapped,
                threads: stat.num_threads,
                state: ProcessState::from_proc_char(stat.state),
                start_ticks: stat.start_ticks,
                start_time: self.boot_time + stat.start_ticks as f64 / self.clk_tck,
                disk_read_rate: p.read_rate,
                disk_write_rate: p.write_rate,
                disk_read_bytes: p.read_bytes,
                disk_write_bytes: p.write_bytes,
                energy: EnergyModel::impact(cpu, p.ctxt_rate),
                app_id: p.app_id.clone(),
                protected: is_protected(pid, &p.name),
                name: p.name.clone(),
            };
            out.push(item);
            seen.insert(key, p);
        }
        // Exited processes drop out here.
        self.prev = seen;
        out
    }

    fn refresh_full(&self, dir: &std::path::Path, p: &mut Prev, now: f64) {
        let dt = p.full_at.map(|t| (now - t).max(1e-3));
        if let Ok(status) = fs::read_to_string(dir.join("status")) {
            let kv = parse_kv(&status);
            p.swapped = kv_u64(&kv, "VmSwap").unwrap_or(0) * 1024;
            let ctxt = kv_u64(&kv, "voluntary_ctxt_switches").unwrap_or(0);
            if let Some(dt) = dt {
                p.ctxt_rate = ctxt.saturating_sub(p.ctxt) as f64 / dt;
            }
            p.ctxt = ctxt;
        }
        // Only readable for our own processes (ptrace access mode).
        if let Ok(io) = fs::read_to_string(dir.join("io")) {
            let kv = parse_kv(&io);
            let r = kv_u64(&kv, "read_bytes").unwrap_or(0);
            let w = kv_u64(&kv, "write_bytes").unwrap_or(0);
            if let Some(dt) = dt {
                p.read_rate = r.saturating_sub(p.read_bytes) as f64 / dt;
                p.write_rate = w.saturating_sub(p.write_bytes) as f64 / dt;
            }
            p.read_bytes = r;
            p.write_bytes = w;
        }
        p.full_at = Some(now);
    }
}

/// `comm` is truncated to 15 bytes; prefer the executable name from argv[0]
/// when that looks like a longer version of it. Kernel threads have no cmdline.
fn display_name(dir: &std::path::Path, comm: &str) -> String {
    if comm.len() < 15 {
        return comm.to_string();
    }
    let Ok(cmdline) = fs::read(dir.join("cmdline")) else { return comm.to_string() };
    let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    let argv0 = String::from_utf8_lossy(argv0);
    let base = argv0.rsplit('/').next().unwrap_or_default();
    if base.starts_with(comm) {
        base.to_string()
    } else {
        comm.to_string()
    }
}

pub fn read_start_ticks(host: &Host, pid: u32) -> Option<u64> {
    let text = fs::read_to_string(host.proc(format!("{pid}/stat"))).ok()?;
    Some(parse_stat(&text)?.start_ticks)
}

/// Inspector data for one process.
pub fn detail(host: &Host, pid: u32) -> Option<ProcessDetail> {
    let dir = host.proc(pid.to_string());
    let stat = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
    let link = |name: &str| fs::read_link(dir.join(name)).ok().map(|p| p.to_string_lossy().into_owned());
    let cmdline = fs::read(dir.join("cmdline"))
        .map(|b| {
            b.split(|c| *c == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
        })
        .unwrap_or_default();
    let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
    let kv = parse_kv(&status);
    let rollup = fs::read_to_string(dir.join("smaps_rollup")).ok();
    let rkv = rollup.as_deref().map(parse_kv);
    let kb = |m: &HashMap<&str, &str>, k: &str| kv_u64(m, k).map(|v| v * 1024);
    Some(ProcessDetail {
        pid,
        start_ticks: stat.start_ticks,
        exe: link("exe"),
        cwd: link("cwd"),
        cmdline,
        open_files: fs::read_dir(dir.join("fd")).ok().map(|d| d.count() as u32),
        pss: rkv.as_ref().and_then(|m| kb(m, "Pss")),
        uss: rkv.as_ref().and_then(|m| Some(kb(m, "Private_Clean")? + kb(m, "Private_Dirty")?)),
        swap: kb(&kv, "VmSwap"),
        voluntary_ctxt_switches: kv_u64(&kv, "voluntary_ctxt_switches"),
        nonvoluntary_ctxt_switches: kv_u64(&kv, "nonvoluntary_ctxt_switches"),
        nice: Some(stat.nice),
        cgroup: fs::read_to_string(dir.join("cgroup"))
            .ok()
            .and_then(|c| c.lines().find_map(|l| l.strip_prefix("0::")).map(str::to_string)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::testutil::write;

    fn stat_line(pid: u32, comm: &str, utime: u64, start: u64) -> String {
        format!("{pid} ({comm}) S 1 {pid} {pid} 0 -1 4194560 100 0 0 0 {utime} 0 0 0 20 0 3 0 {start} 1000000 500 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n")
    }

    #[test]
    fn stat_with_tricky_comm() {
        let s = parse_stat(&stat_line(42, "Web Content (x)", 250, 9000)).unwrap();
        assert_eq!(s.pid, 42);
        assert_eq!(s.comm, "Web Content (x)");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 1);
        assert_eq!(s.utime, 250);
        assert_eq!(s.num_threads, 3);
        assert_eq!(s.start_ticks, 9000);
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn statm() {
        assert_eq!(parse_statm("1000 300 100 1 0 50 0\n"), Some((300, 100)));
        assert_eq!(parse_statm(""), None);
    }

    #[test]
    fn app_ids_from_cgroups() {
        let c = |s: &str| app_id_from_cgroup(s);
        assert_eq!(
            c("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.gnome.Terminal-1234.scope\n"),
            Some("org.gnome.Terminal".into())
        );
        assert_eq!(
            c("0::/user.slice/u/app.slice/app-com.anthropic.Claude-60052.scope"),
            Some("com.anthropic.Claude".into())
        );
        assert_eq!(c("0::/x/app-gnome-firefox\\x2desr-99.scope"), Some("firefox-esr".into()));
        assert_eq!(c("0::/x/app-flatpak-org.mozilla.firefox-5555.scope"), Some("org.mozilla.firefox".into()));
        assert_eq!(c("0::/x/app-org.gnome.Nautilus@abc.service"), Some("org.gnome.Nautilus".into()));
        assert_eq!(c("0::/system.slice/cron.service"), None);
        assert_eq!(c("0::/user.slice/user-1000.slice/session-2.scope"), None);
    }

    #[test]
    fn protected_processes() {
        assert!(is_protected(1, "init"));
        assert!(is_protected(500, "gnome-shell"));
        assert!(!is_protected(500, "firefox"));
    }

    #[test]
    fn sample_computes_cpu_and_drops_exited() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "etc/passwd", "root:x:0:0::/root:/bin/sh\n");
        write(root, "proc/10/stat", &stat_line(10, "worker", 0, 500));
        write(root, "proc/10/statm", "1000 300 100 0 0 0 0\n");
        write(root, "proc/11/stat", &stat_line(11, "gone", 0, 600));
        write(root, "proc/11/statm", "10 5 1 0 0 0 0\n");
        write(root, "proc/self", "not a pid dir");
        let host = Host::at(root);
        let mut m = ProcessMonitor::new(4, 1_700_000_000.0);
        m.clk_tck = 100.0;
        m.page_size = 4096;

        let first = m.sample(&host, true, 0.0);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|p| p.cpu == 0.0), "no baseline yet");

        // 2 s later worker used 100 ticks = 1 s of CPU → 50 % of one core.
        write(root, "proc/10/stat", &stat_line(10, "worker", 100, 500));
        std::fs::remove_dir_all(root.join("proc/11")).unwrap();
        let second = m.sample(&host, false, 2.0);
        assert_eq!(second.len(), 1);
        let w = &second[0];
        assert!((w.cpu - 50.0).abs() < 1e-9, "{}", w.cpu);
        assert!((w.cpu_total - 12.5).abs() < 1e-9);
        assert_eq!(w.memory, 200 * 4096);
        assert_eq!(w.resident, 300 * 4096);
        assert_eq!(w.start_time, 1_700_000_005.0);
        assert_eq!(m.prev.len(), 1);
    }

    #[test]
    fn pid_reuse_is_a_new_process() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "proc/10/stat", &stat_line(10, "old", 1000, 500));
        write(root, "proc/10/statm", "1 1 0 0 0 0 0\n");
        let host = Host::at(root);
        let mut m = ProcessMonitor::new(1, 0.0);
        m.sample(&host, false, 0.0);
        // Same pid, different start time: must not inherit the old CPU baseline.
        write(root, "proc/10/stat", &stat_line(10, "new", 5, 900));
        let s = m.sample(&host, false, 1.0);
        assert_eq!(s[0].name, "new");
        assert_eq!(s[0].cpu, 0.0);
    }

    #[test]
    fn full_refresh_rates_survive_light_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "proc/7/stat", &stat_line(7, "io", 0, 1));
        write(root, "proc/7/statm", "1 1 0 0 0 0 0\n");
        write(root, "proc/7/io", "read_bytes: 0\nwrite_bytes: 0\n");
        write(root, "proc/7/status", "VmSwap:\t8 kB\nvoluntary_ctxt_switches:\t0\n");
        let host = Host::at(root);
        let mut m = ProcessMonitor::new(1, 0.0);
        m.sample(&host, true, 0.0);
        write(root, "proc/7/io", "read_bytes: 2048\nwrite_bytes: 4096\n");
        write(root, "proc/7/status", "VmSwap:\t8 kB\nvoluntary_ctxt_switches:\t200\n");
        let full = m.sample(&host, true, 2.0);
        assert_eq!((full[0].disk_read_rate, full[0].disk_write_rate), (1024.0, 2048.0));
        assert_eq!(full[0].swapped, 8192);
        assert!((full[0].energy - 100.0 * 0.045).abs() < 1e-9);
        let light = m.sample(&host, false, 3.0);
        assert_eq!(light[0].disk_read_rate, 1024.0);
    }

    #[test]
    fn long_names_come_from_argv0() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "proc/5/cmdline", "/usr/libexec/gnome-software-service\0--x\0");
        assert_eq!(display_name(&root.join("proc/5"), "gnome-software-"), "gnome-software-service");
        assert_eq!(display_name(&root.join("proc/5"), "bash"), "bash");
    }

    #[test]
    fn detail_reads_rollup_and_cmdline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "proc/9/stat", &stat_line(9, "app", 0, 77));
        write(root, "proc/9/cmdline", "/bin/app\0--flag\0");
        write(root, "proc/9/smaps_rollup", "Rss: 100 kB\nPss: 60 kB\nPrivate_Clean: 10 kB\nPrivate_Dirty: 30 kB\n");
        write(root, "proc/9/status", "VmSwap:\t4 kB\nvoluntary_ctxt_switches:\t5\nnonvoluntary_ctxt_switches:\t6\n");
        write(root, "proc/9/cgroup", "0::/user.slice/app.slice/app-gnome-x-1.scope\n");
        std::fs::create_dir_all(root.join("proc/9/fd")).unwrap();
        let d = detail(&Host::at(root), 9).unwrap();
        assert_eq!(d.cmdline, vec!["/bin/app", "--flag"]);
        assert_eq!((d.pss, d.uss, d.swap), (Some(60 * 1024), Some(40 * 1024), Some(4 * 1024)));
        assert_eq!(d.open_files, Some(0));
        assert_eq!(d.start_ticks, 77);
        assert!(d.cgroup.unwrap().ends_with(".scope"));
    }
}

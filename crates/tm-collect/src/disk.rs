//! Mounted volumes from `/proc/self/mountinfo` + `statvfs`.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs;

use tm_core::model::{DiskInfo, VolumeInfo};

use crate::host::{self, Host};

/// Filesystems that hold user data. Pseudo filesystems, tmpfs, squashfs
/// (snaps) and overlays are left out.
const REAL_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "zfs", "bcachefs",
    "jfs", "reiserfs", "hfsplus", "nilfs2",
];

const HIDDEN_PREFIXES: &[&str] = &["/snap/", "/var/lib/docker/", "/var/lib/containers/", "/proc/", "/sys/"];

#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub mount_point: String,
    pub fs_type: String,
    pub source: String,
}

/// Mount points escape space, tab, newline and backslash as octal (`\040`).
fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            let digits = std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or("x");
            if let Ok(v) = u8::from_str_radix(digits, 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (pre, post) = line.split_once(" - ")?;
            let pre: Vec<&str> = pre.split_whitespace().collect();
            let mut post = post.split_whitespace();
            Some(Mount {
                mount_point: unescape_octal(pre.get(4)?),
                fs_type: post.next()?.to_string(),
                source: unescape_octal(post.next()?),
            })
        })
        .collect()
}

/// Real, user-visible volumes, one per underlying device (btrfs subvolumes
/// and bind mounts share their device's space, so they'd only repeat it).
pub fn visible_mounts(mounts: &[Mount]) -> Vec<Mount> {
    let mut by_source: HashMap<String, Mount> = HashMap::new();
    for m in mounts {
        if !REAL_FS.contains(&m.fs_type.as_str()) {
            continue;
        }
        if HIDDEN_PREFIXES.iter().any(|p| m.mount_point.starts_with(p)) {
            continue;
        }
        let key = format!("{}|{}", m.source, m.fs_type);
        match by_source.get(&key) {
            Some(existing) if existing.mount_point.len() <= m.mount_point.len() => {}
            _ => {
                by_source.insert(key, m.clone());
            }
        }
    }
    let mut out: Vec<Mount> = by_source.into_values().collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}

/// `(usable capacity, available)` in bytes, like `df`: root-reserved blocks
/// count neither as used nor as available.
fn statvfs(path: &str) -> Option<(u64, u64)> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a valid out-pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    let used = (st.f_blocks as u64).saturating_sub(st.f_bfree as u64) * frsize;
    let avail = st.f_bavail as u64 * frsize;
    Some((used + avail, avail))
}

/// device path → filesystem label, from `/dev/disk/by-label`.
fn labels(host: &Host) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let dir = host.dev("disk/by-label");
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            if let Ok(target) = fs::canonicalize(e.path()) {
                let label = unescape_label(&e.file_name().to_string_lossy());
                out.insert(target.to_string_lossy().into_owned(), label);
            }
        }
    }
    out
}

/// udev escapes by-label names as `\x20`.
fn unescape_label(s: &str) -> String {
    s.replace("\\x20", " ").replace("\\x2f", "/")
}

fn is_removable(host: &Host, source: &str, mount_point: &str) -> bool {
    if mount_point.starts_with("/media/") || mount_point.starts_with("/run/media/") {
        return true;
    }
    let Some(dev) = source.strip_prefix("/dev/") else { return false };
    let Ok(real) = fs::canonicalize(host.sys(format!("class/block/{dev}"))) else { return false };
    if real.to_string_lossy().contains("/usb") {
        return true;
    }
    // Partitions carry `removable` on their parent disk.
    [real.join("removable"), real.join("../removable")].iter().any(|p| host::read_trimmed(p).as_deref() == Some("1"))
}

fn volume_name(mount_point: &str, label: Option<&String>) -> String {
    if let Some(l) = label {
        return l.clone();
    }
    match mount_point {
        "/" => "System".into(),
        _ => mount_point.rsplit('/').find(|s| !s.is_empty()).unwrap_or(mount_point).to_string(),
    }
}

pub fn sample(host: &Host) -> DiskInfo {
    let text = host::read_string(host.proc("self/mountinfo")).unwrap_or_default();
    let labels = labels(host);
    let volumes = visible_mounts(&parse_mountinfo(&text))
        .into_iter()
        .filter_map(|m| {
            let (total, free) = statvfs(&m.mount_point)?;
            if total == 0 {
                return None;
            }
            let removable = is_removable(host, &m.source, &m.mount_point);
            Some(VolumeInfo::new(
                volume_name(&m.mount_point, labels.get(&m.source)),
                m.mount_point,
                m.source,
                m.fs_type,
                total,
                free,
                removable,
            ))
        })
        .collect();
    DiskInfo { volumes }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 8:3 / / rw,relatime shared:1 - ext4 /dev/sda3 rw
23 22 0:5 / /proc rw - proc proc rw
24 22 8:1 / /boot/efi rw - vfat /dev/sda1 rw
25 22 0:30 /@home /home rw - btrfs /dev/nvme0n1p2 rw
26 22 0:30 /@snap /home/.snapshots rw - btrfs /dev/nvme0n1p2 rw
27 22 7:0 / /snap/core/1 ro - squashfs /dev/loop0 ro
28 22 0:40 / /run/user/1000 rw - tmpfs tmpfs rw
29 22 8:17 / /media/me/My\\040Stick rw - vfat /dev/sdb1 rw
";

    #[test]
    fn parses_and_unescapes() {
        let m = parse_mountinfo(MOUNTINFO);
        assert_eq!(m.len(), 8);
        assert_eq!(m[0], Mount { mount_point: "/".into(), fs_type: "ext4".into(), source: "/dev/sda3".into() });
        assert_eq!(m[7].mount_point, "/media/me/My Stick");
    }

    #[test]
    fn filters_pseudo_and_dedupes_subvolumes() {
        let v = visible_mounts(&parse_mountinfo(MOUNTINFO));
        let points: Vec<&str> = v.iter().map(|m| m.mount_point.as_str()).collect();
        assert_eq!(points, vec!["/", "/boot/efi", "/home", "/media/me/My Stick"]);
    }

    #[test]
    fn names() {
        assert_eq!(volume_name("/", None), "System");
        assert_eq!(volume_name("/home", None), "home");
        assert_eq!(volume_name("/data", Some(&"Archive".to_string())), "Archive");
    }

    #[test]
    fn removable_by_mount_location() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::at(dir.path());
        assert!(is_removable(&host, "/dev/sdb1", "/run/media/me/USB"));
        assert!(!is_removable(&host, "/dev/sda3", "/"));
    }

    #[test]
    fn statvfs_root_works() {
        let (total, free) = statvfs("/").unwrap();
        assert!(total > 0 && free <= total);
    }
}

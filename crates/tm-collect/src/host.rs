//! Where the kernel interfaces live. Tests point these at fixture directories,
//! which keeps every parser testable without touching the real system.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Host {
    pub proc_root: PathBuf,
    pub sys_root: PathBuf,
    pub etc_root: PathBuf,
    pub dev_root: PathBuf,
}

impl Default for Host {
    fn default() -> Self {
        Self::at("/")
    }
}

impl Host {
    /// A host rooted at `root` (`root/proc`, `root/sys`, …).
    pub fn at(root: impl AsRef<Path>) -> Self {
        let r = root.as_ref();
        Self { proc_root: r.join("proc"), sys_root: r.join("sys"), etc_root: r.join("etc"), dev_root: r.join("dev") }
    }

    pub fn proc(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.proc_root.join(rel)
    }

    pub fn sys(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.sys_root.join(rel)
    }

    pub fn etc(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.etc_root.join(rel)
    }

    pub fn dev(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.dev_root.join(rel)
    }
}

pub fn read_string(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Trimmed file contents, or `None` if unreadable.
pub fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

pub fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    read_trimmed(path)?.parse().ok()
}

/// Parse a kernel cpulist such as `0-3,8,10-11`.
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(n) = part.trim().parse() {
                    out.push(n);
                }
            }
        }
    }
    out
}

pub fn clock_ticks_per_second() -> f64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as f64
    } else {
        100.0
    }
}

pub fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::path::Path;

    pub fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    pub fn symlink(root: &Path, rel: &str, target: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, p).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_list() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert!(parse_cpu_list("").is_empty());
    }
}

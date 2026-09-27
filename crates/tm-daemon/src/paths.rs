//! XDG base directories, resolved at runtime from the environment.
//!
//! Nothing here is derived from a user name or a fixed home location, so the
//! same binary works for any account and any `$HOME`.

use std::env;
use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "topmanager";

fn xdg(var: &str, home_fallback: &str) -> PathBuf {
    resolve(var, env::var_os(var), &home(), home_fallback)
}

/// Use `$XDG_*` only if it is absolute (the spec says to ignore relative
/// values) and writable by us. A value pointing into someone else's home,
/// e.g. from a system-wide `/etc/environment`, falls back to our own
/// `$HOME` default instead of failing. install.sh applies the same rule.
fn resolve(var: &str, value: Option<OsString>, home: &Path, home_fallback: &str) -> PathBuf {
    let fallback = home.join(home_fallback);
    let Some(v) = value.filter(|v| !v.is_empty()).map(PathBuf::from) else { return fallback };
    if v.is_absolute() && creatable(&v) {
        return v;
    }
    eprintln!("topmanagerd: ignoring {var}={} (not usable), using {}", v.display(), fallback.display());
    fallback
}

/// Writable, or creatable under its nearest existing ancestor.
fn creatable(path: &Path) -> bool {
    let Some(existing) = path.ancestors().find(|p| p.exists()) else { return false };
    let Ok(c) = CString::new(existing.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        Self {
            config_dir: xdg("XDG_CONFIG_HOME", ".config").join(APP_DIR),
            state_dir: xdg("XDG_STATE_HOME", ".local/state").join(APP_DIR),
        }
    }

    /// For tests: everything under one directory.
    #[cfg(test)]
    pub fn under(root: &Path) -> Self {
        Self { config_dir: root.join("config"), state_dir: root.join("state") }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn history_file(&self) -> PathBuf {
        self.state_dir.join("history.jsonl")
    }

    pub fn alerts_file(&self) -> PathBuf {
        self.state_dir.join("alerts.json")
    }
}

/// Write via a temp file + rename so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_resolution() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let own = h.join("custom/config");
        assert_eq!(resolve("X", Some(own.clone().into()), h, ".config"), own, "creatable path is used");
        assert_eq!(resolve("X", None, h, ".config"), h.join(".config"));
        assert_eq!(resolve("X", Some("".into()), h, ".config"), h.join(".config"));
        assert_eq!(resolve("X", Some("relative/dir".into()), h, ".config"), h.join(".config"));
        // Not writable by an unprivileged user (tests don't run as root).
        assert_eq!(resolve("X", Some("/proc/1/somewhere".into()), h, ".config"), h.join(".config"));
    }
}

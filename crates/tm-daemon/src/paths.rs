//! XDG base directories, resolved at runtime from the environment.
//!
//! Nothing here is derived from a user name or a fixed home location, so the
//! same binary works for any account and any `$HOME`.

use std::env;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "topmanager";

fn xdg(var: &str, home_fallback: &str) -> PathBuf {
    match env::var_os(var) {
        // The spec says relative values must be ignored.
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home().join(home_fallback),
    }
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

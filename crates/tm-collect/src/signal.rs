//! Race-free process signalling.
//!
//! A pid alone is not an identity: it can be recycled between the moment the
//! UI showed a row and the moment the user clicked "quit". We open a pidfd
//! (pinning that exact process), confirm its start time matches what the UI
//! saw, and only then signal through the pidfd.

use std::fmt;

use crate::host::Host;
use crate::process::{is_protected, read_start_ticks};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Terminate,
    Kill,
    Stop,
    Continue,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "term" | "terminate" => Action::Terminate,
            "kill" => Action::Kill,
            "stop" | "suspend" => Action::Stop,
            "cont" | "continue" | "resume" => Action::Continue,
            _ => return None,
        })
    }

    fn signal(self) -> libc::c_int {
        match self {
            Action::Terminate => libc::SIGTERM,
            Action::Kill => libc::SIGKILL,
            Action::Stop => libc::SIGSTOP,
            Action::Continue => libc::SIGCONT,
        }
    }

    fn is_destructive(self) -> bool {
        !matches!(self, Action::Continue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    NotFound,
    /// The pid now belongs to a different process.
    Replaced,
    /// Ending it would end the session (see `process::is_protected`).
    Protected,
    PermissionDenied,
    Os(i32),
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignalError::NotFound => write!(f, "process no longer exists"),
            SignalError::Replaced => write!(f, "process exited and its pid was reused"),
            SignalError::Protected => write!(f, "refusing to stop a process the desktop session depends on"),
            SignalError::PermissionDenied => write!(f, "permission denied (process belongs to another user)"),
            SignalError::Os(e) => write!(f, "OS error {e}"),
        }
    }
}

impl std::error::Error for SignalError {}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn map_errno(e: i32) -> SignalError {
    match e {
        libc::ESRCH => SignalError::NotFound,
        libc::EPERM => SignalError::PermissionDenied,
        e => SignalError::Os(e),
    }
}

pub fn send(host: &Host, pid: u32, expected_start_ticks: u64, name: &str, action: Action) -> Result<(), SignalError> {
    if action.is_destructive() && is_protected(pid, name) {
        return Err(SignalError::Protected);
    }
    let pid_t = libc::pid_t::try_from(pid).map_err(|_| SignalError::NotFound)?;

    // SAFETY: plain syscall with integer arguments.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid_t, 0) } as libc::c_int;
    if fd < 0 {
        let e = errno();
        if e != libc::ENOSYS {
            return Err(map_errno(e));
        }
        // Kernel < 5.3: best effort with a start-time check right before kill().
        if read_start_ticks(host, pid) != Some(expected_start_ticks) {
            return Err(SignalError::Replaced);
        }
        // SAFETY: plain syscall.
        return if unsafe { libc::kill(pid_t, action.signal()) } == 0 { Ok(()) } else { Err(map_errno(errno())) };
    }

    let result = (|| {
        // The pidfd pins the process; if the pid was reused before we opened
        // it, the start time we read now belongs to the newcomer.
        match read_start_ticks(host, pid) {
            None => return Err(SignalError::NotFound),
            Some(t) if t != expected_start_ticks => return Err(SignalError::Replaced),
            _ => {}
        }
        // SAFETY: fd is a valid pidfd; siginfo may be NULL.
        let r = unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, fd, action.signal(), std::ptr::null::<libc::siginfo_t>(), 0)
        };
        if r == 0 {
            Ok(())
        } else {
            Err(map_errno(errno()))
        }
    })();
    // SAFETY: we own fd.
    unsafe { libc::close(fd) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn parse_actions() {
        assert_eq!(Action::parse("term"), Some(Action::Terminate));
        assert_eq!(Action::parse("resume"), Some(Action::Continue));
        assert_eq!(Action::parse("nuke"), None);
    }

    #[test]
    fn refuses_protected() {
        let host = Host::default();
        assert_eq!(send(&host, 1, 0, "systemd", Action::Kill), Err(SignalError::Protected));
    }

    #[test]
    fn signals_own_child_and_rejects_wrong_start_time() {
        let host = Host::default();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let ticks = read_start_ticks(&host, pid).unwrap();

        assert_eq!(send(&host, pid, ticks + 1, "sleep", Action::Terminate), Err(SignalError::Replaced));
        assert_eq!(send(&host, pid, ticks, "sleep", Action::Stop), Ok(()));
        assert_eq!(send(&host, pid, ticks, "sleep", Action::Continue), Ok(()));
        assert_eq!(send(&host, pid, ticks, "sleep", Action::Terminate), Ok(()));
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(send(&host, pid, ticks, "sleep", Action::Terminate).is_err());
    }
}

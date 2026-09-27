//! Platform-neutral TopManager logic.
//!
//! Everything here is pure: no `/proc`, no clock, no D-Bus. That keeps the
//! health score, alert engine, history math and formatting trivially unit
//! testable, and it is the part ported 1:1 from the macOS Swift sources.

pub mod alert;
pub mod battery;
pub mod format;
pub mod health;
pub mod metrics;
pub mod model;
pub mod settings;

/// D-Bus identity shared by the daemon, the GUI and the Shell extension.
pub mod bus {
    /// The daemon's bus name. Deliberately not the app id: the GUI's
    /// GApplication owns `APP_ID` on the bus to stay single-instance.
    pub const NAME: &str = "io.github.hariel1985.TopManager.Daemon";
    /// Desktop app id (window, .desktop file, icon, notifications).
    pub const APP_ID: &str = "io.github.hariel1985.TopManager";
    pub const PATH: &str = "/io/github/hariel1985/TopManager";
    pub const INTERFACE: &str = "io.github.hariel1985.TopManager1";
    /// Bumped on incompatible changes to the JSON payloads.
    pub const API_VERSION: u32 = 1;
}

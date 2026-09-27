//! Desktop notifications through `org.freedesktop.Notifications`.

use std::collections::HashMap;

use tm_core::alert::{AlertSeverity, SystemAlert};
use tm_core::bus;
use zbus::zvariant::Value;
use zbus::Connection;

pub async fn send(conn: &Connection, alert: &SystemAlert) -> zbus::Result<()> {
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    // Lets GNOME attribute the notification to the app (icon, settings).
    hints.insert("desktop-entry", Value::from(bus::APP_ID));
    let urgency: u8 = match alert.severity {
        AlertSeverity::Critical => 2,
        _ => 1,
    };
    hints.insert("urgency", Value::from(urgency));
    let actions: Vec<&str> = Vec::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "TopManager",
            0u32,
            "utilities-system-monitor",
            alert.title.as_str(),
            alert.message.as_str(),
            actions,
            hints,
            -1i32,
        ),
    )
    .await?;
    Ok(())
}

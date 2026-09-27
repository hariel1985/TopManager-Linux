//! Human-readable formatting, ported from `ByteFormatters.swift`.
//! Binary units (1 KB = 1024 B), matching the macOS app's `.binary` style.

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Negative values clamp to zero instead of wrapping.
pub fn format_bytes_f64(bytes: f64) -> String {
    format_bytes(bytes.max(0.0) as u64)
}

pub fn format_bytes_per_second(bytes_per_second: f64) -> String {
    let v = bytes_per_second.abs();
    if v < 1.0 {
        return "0 B/s".into();
    }
    format!("{}/s", format_bytes(v as u64))
}

pub fn format_bytes_compact(bytes: f64) -> String {
    let v = bytes.abs();
    const K: f64 = 1024.0;
    if v < 1.0 {
        "0".into()
    } else if v < K {
        format!("{v:.0} B")
    } else if v < K * K {
        format!("{:.1} KB", v / K)
    } else if v < K * K * K {
        format!("{:.1} MB", v / (K * K))
    } else {
        format!("{:.2} GB", v / (K * K * K))
    }
}

pub fn format_percentage(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}%")
}

pub fn format_uptime(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let days = s / 86_400;
    let hours = (s % 86_400) / 3600;
    let minutes = (s % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn bytes_gigabyte_scale() {
        let s = format_bytes(2 * 1024 * 1024 * 1024);
        assert_eq!(s, "2.0 GB");
    }

    #[test]
    fn bytes_large_values_drop_decimals() {
        assert_eq!(format_bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn negative_f64_clamps_to_zero() {
        assert_eq!(format_bytes_f64(-500.0), "0 B");
    }

    #[test]
    fn per_second_below_one() {
        assert_eq!(format_bytes_per_second(0.4), "0 B/s");
    }

    #[test]
    fn per_second_suffix() {
        let s = format_bytes_per_second(5.0 * 1024.0 * 1024.0);
        assert!(s.ends_with("/s") && s.contains("MB"), "{s}");
    }

    #[test]
    fn compact() {
        assert_eq!(format_bytes_compact(0.2), "0");
        assert_eq!(format_bytes_compact(512.0), "512 B");
        assert_eq!(format_bytes_compact(1536.0), "1.5 KB");
    }

    #[test]
    fn percentage() {
        assert_eq!(format_percentage(12.345, 1), "12.3%");
        assert_eq!(format_percentage(12.345, 2), "12.35%");
    }

    #[test]
    fn uptime() {
        assert_eq!(format_uptime(45.0 * 60.0), "45m");
        assert_eq!(format_uptime(3.0 * 3600.0 + 12.0 * 60.0), "3h 12m");
        assert_eq!(format_uptime(2.0 * 86400.0 + 5.0 * 3600.0 + 9.0 * 60.0), "2d 5h 9m");
    }
}

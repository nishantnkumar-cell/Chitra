//! Small helpers for the viewer: formatting and the window icon.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// "2026-10-16 22:10" in UTC, without pulling in a date library.
pub fn format_time(t: SystemTime) -> String {
    let Ok(d) = t.duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    let secs = d.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// The window and taskbar icon (the "lotus aperture"), from
/// `assets/chitra.png`, which `tools/make_icon.py` draws.
pub fn icon() -> egui::IconData {
    let png = include_bytes!("../../assets/chitra.png");
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(img) => {
            let img = img.into_rgba8();
            egui::IconData {
                width: img.width(),
                height: img.height(),
                rgba: img.into_raw(),
            }
        }
        Err(_) => egui::IconData::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn dates() {
        assert_eq!(format_time(UNIX_EPOCH), "1970-01-01 00:00 UTC");
        let t = UNIX_EPOCH + Duration::from_secs(1_792_188_600); // 2026-10-16 22:10 UTC
        assert_eq!(format_time(t), "2026-10-16 22:10 UTC");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }

    #[test]
    fn icon_decodes() {
        let icon = icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}

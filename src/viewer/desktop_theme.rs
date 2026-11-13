//! The desktop's light/dark setting on Linux, for "Same as system".
//!
//! KDE Plasma and GNOME publish it through the desktop portal
//! (`org.freedesktop.appearance` `color-scheme`), which winit cannot see on
//! Wayland. This is a minimal D-Bus client in safe Rust (no libdbus, no extra
//! crates): one background thread reads the setting once, then waits for the
//! portal's change signal so the viewer follows the desktop live.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui;

const UNKNOWN: u8 = u8::MAX;

/// The latest light/dark setting reported by the desktop, if any.
pub struct DesktopTheme(Arc<AtomicU8>);

impl DesktopTheme {
    /// Start following the desktop setting; repaints `ctx` when it changes.
    pub fn watch(ctx: &egui::Context) -> DesktopTheme {
        let state = Arc::new(AtomicU8::new(UNKNOWN));
        #[cfg(target_os = "linux")]
        {
            let state = state.clone();
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("desktop-theme".into())
                .spawn(move || {
                    let Some(path) = portal::session_bus_path() else {
                        return;
                    };
                    let _ = portal::follow(&path, |scheme| {
                        state.store(scheme.min(2) as u8, Ordering::Relaxed);
                        ctx.request_repaint();
                    });
                });
        }
        #[cfg(not(target_os = "linux"))]
        let _ = ctx;
        DesktopTheme(state)
    }

    /// Dark or light, or None when the desktop has not said.
    pub fn get(&self) -> Option<egui::Theme> {
        // Portal values: 0 no preference (light on GNOME), 1 dark, 2 light.
        match self.0.load(Ordering::Relaxed) {
            1 => Some(egui::Theme::Dark),
            0 | 2 => Some(egui::Theme::Light),
            _ => None,
        }
    }
}

#[cfg(target_os = "linux")]
mod portal {
    use std::io;
    use std::path::Path;

    pub use super::super::dbus::session_bus_path;
    use super::super::dbus::*;

    const SETTINGS: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";

    /// Read the colour scheme and then report every change, until the bus
    /// connection closes. `report` gets the portal value (0, 1 or 2).
    pub fn follow(path: &Path, mut report: impl FnMut(u32)) -> io::Result<()> {
        let mut bus = Conn::connect(path)?;
        let rule = format!(
            "type='signal',interface='{SETTINGS}',member='SettingChanged',arg0='{NAMESPACE}',arg1='{KEY}'"
        );
        bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        )?;
        bus.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            "s",
            &body_strings(&[&rule]),
        )?;
        let read_one = bus.call(
            PORTAL,
            PORTAL_PATH,
            SETTINGS,
            "ReadOne",
            "ss",
            &body_strings(&[NAMESPACE, KEY]),
        )?;
        let mut read_old = 0;
        loop {
            let msg = bus.read()?;
            match msg.kind {
                METHOD_RETURN if msg.reply_serial == read_one || msg.reply_serial == read_old => {
                    if let Some(v) = msg.body_cursor().variant_u32(4) {
                        report(v);
                    }
                }
                // Portals older than 2023 only have Read, which wraps the value in
                // one more variant.
                ERROR if msg.reply_serial == read_one => {
                    read_old = bus.call(
                        PORTAL,
                        PORTAL_PATH,
                        SETTINGS,
                        "Read",
                        "ss",
                        &body_strings(&[NAMESPACE, KEY]),
                    )?;
                }
                SIGNAL if msg.member.as_deref() == Some("SettingChanged") => {
                    let mut c = msg.body_cursor();
                    if c.str() == Some(NAMESPACE) && c.str() == Some(KEY) {
                        if let Some(v) = c.variant_u32(4) {
                            report(v);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};
        use std::sync::mpsc;
        use std::time::Duration;

        /// Against a real dbus-daemon with a stand-in portal that answers
        /// ReadOne with "dark" and then signals a switch to "light".
        #[test]
        fn follows_a_portal_on_a_real_bus() {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("bus");
            let config = dir.path().join("bus.conf");
            std::fs::write(
                &config,
                format!(
                    "<busconfig><type>session</type><listen>unix:path={}</listen>\
                     <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                    sock.display()
                ),
            )
            .unwrap();
            let Ok(mut daemon) = Command::new("dbus-daemon")
                .arg(format!("--config-file={}", config.display()))
                .args(["--nofork", "--nopidfile"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                eprintln!("dbus-daemon not installed; skipping");
                return;
            };
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }

            // Stand-in portal.
            let mut portal = Conn::connect(&sock).unwrap();
            portal
                .call(
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "Hello",
                    "",
                    &[],
                )
                .unwrap();
            let mut w = Writer::default();
            w.str(PORTAL);
            w.u32(0);
            let request = portal
                .call(
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "RequestName",
                    "su",
                    &w.buf,
                )
                .unwrap();
            while portal.read().unwrap().reply_serial != request {}

            let (tx, rx) = mpsc::channel();
            let client_sock = sock.clone();
            std::thread::spawn(move || {
                let _ = follow(&client_sock, |v| {
                    let _ = tx.send(v);
                });
            });

            // Answer ReadOne with 1 (dark).
            let call = loop {
                let m = portal.read().unwrap();
                if m.kind == METHOD_CALL && m.member.as_deref() == Some("ReadOne") {
                    break m;
                }
            };
            let mut w = Writer::default();
            w.sig("u");
            w.u32(1);
            let sender = call.sender.clone().unwrap();
            portal
                .send(
                    METHOD_RETURN,
                    &[(5, Field::U32(call.serial)), (6, Field::Str(&sender))],
                    "v",
                    &w.buf,
                )
                .unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);

            // Then the user switches to light.
            let mut w = Writer::default();
            w.str(NAMESPACE);
            w.str(KEY);
            w.sig("u");
            w.u32(2);
            portal
                .send(
                    SIGNAL,
                    &[
                        (1, Field::Path(PORTAL_PATH)),
                        (2, Field::Str(SETTINGS)),
                        (3, Field::Str("SettingChanged")),
                    ],
                    "ssv",
                    &w.buf,
                )
                .unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }
}

//! Opening web pages from the app.
//!
//! egui's own `hyperlink_to` opens nothing in this build (eframe's `links`
//! feature, which needs the `webbrowser` crate, is off), and a sandboxed
//! Flatpak cannot start the host's browser itself. So links go through here:
//! in a Flatpak through the desktop portal's OpenURI call (the one way out of
//! the sandbox that needs no extra permission), elsewhere, or if the portal
//! is not there, through the system's opener (`xdg-open`, `open`, ...).

use std::io;
use std::path::Path;

use super::dbus::{Conn, PORTAL, PORTAL_PATH, Writer, session_bus_path};

const OPEN_URI: &str = "org.freedesktop.portal.OpenURI";

/// Only web pages (and `mailto:`) are opened from here, never a file or
/// another program's scheme.
pub fn is_openable(url: &str) -> bool {
    let u = url.trim();
    !u.chars().any(char::is_control)
        && (u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:"))
}

fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// The arguments of `OpenURI(parent_window: s, uri: s, options: a{sv})`.
fn open_uri_body(url: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.str(""); // no parent window
    w.str(url);
    w.dict(&[]);
    w.buf
}

/// Ask the desktop portal to open `url` in the user's browser.
fn portal_open(url: &str) -> io::Result<()> {
    let path = session_bus_path().ok_or_else(|| io::Error::other("no session bus"))?;
    let mut bus = Conn::connect(&path)?;
    bus.hello()?;
    let serial = bus.call(
        PORTAL,
        PORTAL_PATH,
        OPEN_URI,
        "OpenURI",
        "ssa{sv}",
        &open_uri_body(url),
    )?;
    bus.reply(serial)?;
    Ok(())
}

/// Open `url` in the browser without blocking the window. A link that cannot
/// be opened is reported on standard error; the windows that show links also
/// offer to copy them.
pub fn open(url: &str) {
    if !is_openable(url) {
        eprintln!("not opening {url}: only web links are opened");
        return;
    }
    let url = url.trim().to_owned();
    std::thread::spawn(move || {
        if cfg!(target_os = "linux") && in_flatpak() {
            match portal_open(&url) {
                Ok(()) => return,
                Err(e) => eprintln!("the desktop portal could not open {url}: {e}"),
            }
        }
        if let Err(e) = chitra::update::open(&url) {
            eprintln!("{e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_uri_arguments_are_encoded_as_ssa_sv() {
        let url = "https://example.com/a";
        let b = open_uri_body(url);
        // "" : length 0 and its NUL; the URL's length at the next 4-byte
        // boundary, its bytes and NUL; then an empty array (length 0).
        let mut want = vec![0, 0, 0, 0, 0];
        want.extend_from_slice(&[0, 0, 0]);
        want.extend_from_slice(&(url.len() as u32).to_le_bytes());
        want.extend_from_slice(url.as_bytes());
        want.push(0);
        assert!(b.starts_with(&want), "{b:?}");
        // The options array: a zero length, as the last four non-padding bytes.
        assert_eq!(&b[want.len()..want.len() + 3], &[0, 0, 0]);
        assert!(b.len() >= want.len() + 4 && b.len() <= want.len() + 12);
    }

    #[test]
    fn only_web_links_open() {
        assert!(is_openable("https://huggingface.co/x/y.pth"));
        assert!(is_openable("http://example.com"));
        assert!(is_openable("mailto:a@example.com"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("/usr/bin/evil"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("https://x.example\nsecond"));
        assert!(!is_openable(""));
    }
}

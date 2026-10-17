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
    use std::io::{self, Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const SETTINGS: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";

    const METHOD_CALL: u8 = 1;
    const METHOD_RETURN: u8 = 2;
    const ERROR: u8 = 3;
    const SIGNAL: u8 = 4;

    /// The session bus socket, from DBUS_SESSION_BUS_ADDRESS or the systemd
    /// default in XDG_RUNTIME_DIR.
    pub fn session_bus_path() -> Option<PathBuf> {
        if let Ok(addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            for transport in addr.split(';') {
                let Some(params) = transport.strip_prefix("unix:") else {
                    continue;
                };
                for kv in params.split(',') {
                    if let Some(path) = kv.strip_prefix("path=") {
                        return Some(PathBuf::from(unescape(path)));
                    }
                }
            }
        }
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        let path = Path::new(&runtime).join("bus");
        path.exists().then_some(path)
    }

    /// D-Bus addresses escape bytes as %xx.
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

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

    fn body_strings(strings: &[&str]) -> Vec<u8> {
        let mut w = Writer::default();
        for s in strings {
            w.str(s);
        }
        w.buf
    }

    pub struct Conn {
        stream: UnixStream,
        serial: u32,
    }

    impl Conn {
        pub fn connect(path: &Path) -> io::Result<Conn> {
            let mut stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            // SASL EXTERNAL: prove who we are by our uid, sent as hex digits.
            let uid = std::fs::metadata("/proc/self")?.uid().to_string();
            let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
            stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
            let mut line = Vec::new();
            let mut byte = [0u8];
            while !line.ends_with(b"\r\n") {
                stream.read_exact(&mut byte)?;
                line.push(byte[0]);
                if line.len() > 512 {
                    return Err(io::Error::other("D-Bus auth reply too long"));
                }
            }
            if !line.starts_with(b"OK ") {
                return Err(io::Error::other("D-Bus auth refused"));
            }
            stream.write_all(b"BEGIN\r\n")?;
            stream.set_read_timeout(None)?;
            Ok(Conn { stream, serial: 0 })
        }

        /// Send a method call; returns its serial.
        pub fn call(
            &mut self,
            dest: &str,
            path: &str,
            iface: &str,
            member: &str,
            sig: &str,
            body: &[u8],
        ) -> io::Result<u32> {
            self.send(
                METHOD_CALL,
                &[
                    (1, Field::Path(path)),
                    (2, Field::Str(iface)),
                    (3, Field::Str(member)),
                    (6, Field::Str(dest)),
                ],
                sig,
                body,
            )
        }

        pub fn send(
            &mut self,
            kind: u8,
            fields: &[(u8, Field)],
            sig: &str,
            body: &[u8],
        ) -> io::Result<u32> {
            self.serial += 1;
            let msg = encode(kind, self.serial, fields, sig, body);
            self.stream.write_all(&msg)?;
            Ok(self.serial)
        }

        pub fn read(&mut self) -> io::Result<Message> {
            let mut fixed = [0u8; 16];
            self.stream.read_exact(&mut fixed)?;
            let le = match fixed[0] {
                b'l' => true,
                b'B' => false,
                _ => return Err(io::Error::other("bad D-Bus message")),
            };
            let word = |i: usize| {
                let b = [fixed[i], fixed[i + 1], fixed[i + 2], fixed[i + 3]];
                if le {
                    u32::from_le_bytes(b)
                } else {
                    u32::from_be_bytes(b)
                }
            };
            let (body_len, fields_len) = (word(4) as usize, word(12) as usize);
            if body_len > 1 << 20 || fields_len > 1 << 16 {
                return Err(io::Error::other("D-Bus message too large"));
            }
            let header_len = (16 + fields_len).div_ceil(8) * 8;
            let mut data = fixed.to_vec();
            data.resize(header_len + body_len, 0);
            self.stream.read_exact(&mut data[16..])?;
            Message::parse(data, le, fields_len, header_len)
                .ok_or_else(|| io::Error::other("bad D-Bus message"))
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // U32 is only sent by the test portal
    pub enum Field<'a> {
        Path(&'a str),
        Str(&'a str),
        Sig(&'a str),
        U32(u32),
    }

    fn encode(kind: u8, serial: u32, fields: &[(u8, Field)], sig: &str, body: &[u8]) -> Vec<u8> {
        let mut w = Writer::default();
        w.buf.extend_from_slice(&[b'l', kind, 0, 1]);
        w.u32(body.len() as u32);
        w.u32(serial);
        let len_at = w.buf.len();
        w.u32(0);
        w.pad(8);
        let start = w.buf.len();
        let field = |w: &mut Writer, code: u8, f: &Field| {
            w.pad(8);
            w.buf.push(code);
            match f {
                Field::Path(s) => {
                    w.sig("o");
                    w.str(s)
                }
                Field::Str(s) => {
                    w.sig("s");
                    w.str(s)
                }
                Field::Sig(s) => {
                    w.sig("g");
                    w.sig(s)
                }
                Field::U32(v) => {
                    w.sig("u");
                    w.u32(*v)
                }
            }
        };
        for (code, f) in fields {
            field(&mut w, *code, f);
        }
        if !sig.is_empty() {
            field(&mut w, 8, &Field::Sig(sig));
        }
        let n = (w.buf.len() - start) as u32;
        w.buf[len_at..len_at + 4].copy_from_slice(&n.to_le_bytes());
        w.pad(8);
        w.buf.extend_from_slice(body);
        w.buf
    }

    #[derive(Default)]
    pub struct Writer {
        pub buf: Vec<u8>,
    }

    impl Writer {
        fn pad(&mut self, align: usize) {
            while self.buf.len() % align != 0 {
                self.buf.push(0);
            }
        }
        pub fn u32(&mut self, v: u32) {
            self.pad(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        pub fn str(&mut self, s: &str) {
            self.u32(s.len() as u32);
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(0);
        }
        pub fn sig(&mut self, s: &str) {
            self.buf.push(s.len() as u8);
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(0);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // sender/serial: for the test portal
    pub struct Message {
        pub kind: u8,
        pub reply_serial: u32,
        pub member: Option<String>,
        pub sender: Option<String>,
        pub serial: u32,
        data: Vec<u8>,
        le: bool,
        body_at: usize,
    }

    impl Message {
        fn parse(data: Vec<u8>, le: bool, fields_len: usize, header_len: usize) -> Option<Message> {
            let mut c = Cursor {
                b: &data,
                pos: 8,
                le,
            };
            let serial = c.u32()?;
            c.pos = 16;
            let (mut reply_serial, mut member, mut sender) = (0, None, None);
            while c.pos < 16 + fields_len {
                c.align(8);
                let code = c.u8()?;
                match c.sig()? {
                    "u" => {
                        let v = c.u32()?;
                        if code == 5 {
                            reply_serial = v;
                        }
                    }
                    "s" | "o" => {
                        let v = c.str()?;
                        match code {
                            3 => member = Some(v.to_owned()),
                            7 => sender = Some(v.to_owned()),
                            _ => {}
                        }
                    }
                    "g" => {
                        c.sig()?;
                    }
                    _ => return None,
                }
            }
            Some(Message {
                kind: data[1],
                reply_serial,
                member,
                sender,
                serial,
                le,
                body_at: header_len,
                data,
            })
        }

        pub fn body_cursor(&self) -> Cursor<'_> {
            Cursor {
                b: &self.data,
                pos: self.body_at,
                le: self.le,
            }
        }
    }

    /// Reads D-Bus values; positions are absolute so alignment is correct.
    pub struct Cursor<'a> {
        b: &'a [u8],
        pos: usize,
        le: bool,
    }

    impl<'a> Cursor<'a> {
        fn align(&mut self, a: usize) {
            self.pos = self.pos.div_ceil(a) * a;
        }
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
            self.pos += n;
            Some(s)
        }
        fn u8(&mut self) -> Option<u8> {
            Some(self.take(1)?[0])
        }
        fn u32(&mut self) -> Option<u32> {
            self.align(4);
            let b: [u8; 4] = self.take(4)?.try_into().ok()?;
            Some(if self.le {
                u32::from_le_bytes(b)
            } else {
                u32::from_be_bytes(b)
            })
        }
        pub fn str(&mut self) -> Option<&'a str> {
            let n = self.u32()? as usize;
            let s = self.take(n)?;
            self.take(1)?;
            std::str::from_utf8(s).ok()
        }
        fn sig(&mut self) -> Option<&'a str> {
            let n = self.u8()? as usize;
            let s = self.take(n)?;
            self.take(1)?;
            std::str::from_utf8(s).ok()
        }
        /// A u32 inside up to `depth` nested variants.
        pub fn variant_u32(&mut self, depth: u8) -> Option<u32> {
            match self.sig()? {
                "u" => self.u32(),
                "v" if depth > 0 => self.variant_u32(depth - 1),
                _ => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Stdio};
        use std::sync::mpsc;

        #[test]
        fn message_round_trip() {
            let mut w = Writer::default();
            w.sig("v");
            w.sig("u");
            w.u32(1);
            let msg = encode(METHOD_RETURN, 7, &[(5, Field::U32(3))], "v", &w.buf);
            let fields_len = u32::from_le_bytes(msg[12..16].try_into().unwrap()) as usize;
            let header_len = (16 + fields_len).div_ceil(8) * 8;
            let m = Message::parse(msg, true, fields_len, header_len).unwrap();
            assert_eq!((m.kind, m.serial, m.reply_serial), (METHOD_RETURN, 7, 3));
            assert_eq!(m.body_cursor().variant_u32(4), Some(1));
        }

        #[test]
        fn unescapes_bus_addresses() {
            assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
            assert_eq!(unescape("/tmp/a%20b"), "/tmp/a b");
        }

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

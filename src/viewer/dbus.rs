//! A minimal D-Bus client in safe Rust (no libdbus, no extra crates beyond
//! rustix for passing a file descriptor), enough to talk to the desktop
//! portal: the light/dark setting and printing.

use std::io::{self, IoSlice, Read, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

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

pub fn body_strings(strings: &[&str]) -> Vec<u8> {
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
        Conn::open(path, false)
    }

    /// Like [`Conn::connect`], and also agrees with the bus on passing file
    /// descriptors (the print portal takes the PDF as one).
    pub fn connect_with_fds(path: &Path) -> io::Result<Conn> {
        Conn::open(path, true)
    }

    fn open(path: &Path, fds: bool) -> io::Result<Conn> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        // SASL EXTERNAL: prove who we are by our uid, sent as hex digits.
        let uid = std::fs::metadata("/proc/self")?.uid().to_string();
        let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
        stream.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
        if !auth_line(&mut stream)?.starts_with(b"OK ") {
            return Err(io::Error::other("D-Bus auth refused"));
        }
        if fds {
            stream.write_all(b"NEGOTIATE_UNIX_FD\r\n")?;
            if !auth_line(&mut stream)?.starts_with(b"AGREE_UNIX_FD") {
                return Err(io::Error::other("D-Bus bus cannot pass files"));
            }
        }
        stream.write_all(b"BEGIN\r\n")?;
        stream.set_read_timeout(None)?;
        Ok(Conn { stream, serial: 0 })
    }

    /// Say hello to the bus; returns our unique name, such as ":1.42".
    pub fn hello(&mut self) -> io::Result<String> {
        let serial = self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        )?;
        let reply = self.reply(serial)?;
        reply
            .body_cursor()
            .str()
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("bad D-Bus Hello reply"))
    }

    /// Wait for the answer to call `serial`, skipping other messages. A D-Bus
    /// error becomes an `Err` with the error's own message.
    pub fn reply(&mut self, serial: u32) -> io::Result<Message> {
        loop {
            let msg = self.read()?;
            if msg.reply_serial != serial {
                continue;
            }
            return match msg.kind {
                METHOD_RETURN => Ok(msg),
                _ => Err(io::Error::other(
                    msg.body_cursor()
                        .str()
                        .unwrap_or("D-Bus call failed")
                        .to_owned(),
                )),
            };
        }
    }

    /// A method call that hands over one open file, as argument `h` index 0.
    #[allow(clippy::too_many_arguments)]
    pub fn call_with_fd(
        &mut self,
        dest: &str,
        path: &str,
        iface: &str,
        member: &str,
        sig: &str,
        body: &[u8],
        fd: BorrowedFd<'_>,
    ) -> io::Result<u32> {
        use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
        self.serial += 1;
        let msg = encode(
            METHOD_CALL,
            self.serial,
            &[
                (1, Field::Path(path)),
                (2, Field::Str(iface)),
                (3, Field::Str(member)),
                (6, Field::Str(dest)),
                (9, Field::U32(1)),
            ],
            sig,
            body,
        );
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        let fds = [fd];
        if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
            return Err(io::Error::other("cannot attach the file"));
        }
        let sent = sendmsg(
            &self.stream,
            &[IoSlice::new(&msg)],
            &mut control,
            SendFlags::empty(),
        )?;
        // The file travels with the first byte; the rest is plain data.
        self.stream.write_all(&msg[sent..])?;
        Ok(self.serial)
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

/// One line of the SASL handshake.
fn auth_line(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.len() > 512 {
            return Err(io::Error::other("D-Bus auth reply too long"));
        }
    }
    Ok(line)
}

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
    pub fn pad(&mut self, align: usize) {
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
    /// An `a{sv}` of strings and numbers.
    pub fn dict(&mut self, entries: &[(&str, Arg)]) {
        self.u32(0);
        let len_at = self.buf.len() - 4;
        self.pad(8);
        let start = self.buf.len();
        for (key, value) in entries {
            self.pad(8);
            self.str(key);
            match value {
                Arg::Str(v) => {
                    self.sig("s");
                    self.str(v);
                }
                Arg::U32(v) => {
                    self.sig("u");
                    self.u32(*v);
                }
            }
        }
        let n = (self.buf.len() - start) as u32;
        self.buf[len_at..len_at + 4].copy_from_slice(&n.to_le_bytes());
    }
}

/// A value to put in [`Writer::dict`].
pub enum Arg<'a> {
    Str(&'a str),
    U32(u32),
}

/// A value read from an `a{sv}`; types Chitra has no use for are `Other`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U32(u32),
    I32(i32),
    F64(f64),
    Bool(bool),
    Str(String),
    Dict(Vec<(String, Value)>),
    Other,
}

impl Value {
    pub fn get<'v>(dict: &'v [(String, Value)], key: &str) -> Option<&'v Value> {
        dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F64(v) => Some(*v),
            Value::U32(v) => Some(*v as f64),
            Value::I32(v) => Some(*v as f64),
            _ => None,
        }
    }
}

/// Alignment of values of a type, by the type's first signature character.
fn alignment(c: u8) -> usize {
    match c {
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b'h' | b's' | b'o' | b'a' => 4,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 1,
    }
}

/// Length of the first complete type in a signature.
fn type_len(sig: &[u8]) -> Option<usize> {
    match *sig.first()? {
        b'a' => Some(1 + type_len(&sig[1..])?),
        open @ (b'(' | b'{') => {
            let close = if open == b'(' { b')' } else { b'}' };
            let mut i = 1;
            while *sig.get(i)? != close {
                i += type_len(&sig[i..])?;
            }
            Some(i + 1)
        }
        _ => Some(1),
    }
}

#[cfg_attr(not(test), allow(dead_code))] // sender/serial: for the test portal
pub struct Message {
    pub kind: u8,
    pub reply_serial: u32,
    pub member: Option<String>,
    pub path: Option<String>,
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
        let (mut reply_serial, mut member, mut path, mut sender) = (0, None, None, None);
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
                        1 => path = Some(v.to_owned()),
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
            path,
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
    pub fn u32(&mut self) -> Option<u32> {
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
    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.align(N);
        let mut b: [u8; N] = self.take(N)?.try_into().ok()?;
        if !self.le {
            b.reverse();
        }
        Some(b)
    }

    /// An `a{sv}`, with nested ones read too.
    pub fn dict(&mut self) -> Option<Vec<(String, Value)>> {
        self.dict_at(0)
    }

    fn dict_at(&mut self, depth: u8) -> Option<Vec<(String, Value)>> {
        let len = self.u32()? as usize;
        self.align(8);
        let end = self.pos.checked_add(len)?;
        let mut out = Vec::new();
        while self.pos < end {
            self.align(8);
            let key = self.str()?.to_owned();
            let sig = self.sig()?;
            out.push((key, self.value(sig.as_bytes(), depth + 1)?));
        }
        (self.pos == end).then_some(out)
    }

    /// One value of the complete type `sig`.
    fn value(&mut self, sig: &[u8], depth: u8) -> Option<Value> {
        if depth > 16 {
            return None;
        }
        Some(match sig {
            b"u" => Value::U32(self.u32()?),
            b"i" => Value::I32(i32::from_le_bytes(self.fixed::<4>()?)),
            b"b" => Value::Bool(self.u32()? != 0),
            b"d" => Value::F64(f64::from_le_bytes(self.fixed::<8>()?)),
            b"s" | b"o" => Value::Str(self.str()?.to_owned()),
            b"a{sv}" => Value::Dict(self.dict_at(depth)?),
            _ => {
                self.skip(sig, depth)?;
                Value::Other
            }
        })
    }

    /// Step over one value of the complete type `sig`.
    fn skip(&mut self, sig: &[u8], depth: u8) -> Option<()> {
        if depth > 16 {
            return None;
        }
        let c = *sig.first()?;
        self.align(alignment(c));
        match c {
            b'y' => drop(self.take(1)?),
            b'n' | b'q' => drop(self.take(2)?),
            b'b' | b'i' | b'u' | b'h' => drop(self.take(4)?),
            b'x' | b't' | b'd' => drop(self.take(8)?),
            b's' | b'o' => drop(self.str()?),
            b'g' => drop(self.sig()?),
            b'v' => {
                let inner = self.sig()?;
                self.skip(inner.as_bytes(), depth + 1)?;
            }
            b'a' => {
                let len = self.u32()? as usize;
                self.align(alignment(*sig.get(1)?));
                self.take(len)?;
            }
            b'(' | b'{' => {
                let mut i = 1;
                while i < sig.len() - 1 {
                    let n = type_len(&sig[i..])?;
                    self.skip(&sig[i..i + n], depth + 1)?;
                    i += n;
                }
            }
            _ => return None,
        }
        Some(())
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
    fn reads_nested_dicts_and_skips_the_rest() {
        // Build {"token": u 7, "page-setup": a{sv}, "list": as, "w": d 210.0}.
        let mut w = Writer::default();
        w.u32(0);
        w.pad(8);
        let start = w.buf.len();
        w.pad(8);
        w.str("token");
        w.sig("u");
        w.u32(7);
        w.pad(8);
        w.str("page-setup");
        w.sig("a{sv}");
        // The nested dict must be written at the right alignment.
        let mut nested = Writer { buf: w.buf.clone() };
        nested.dict(&[("Name", Arg::Str("iso_a4")), ("Copies", Arg::U32(2))]);
        w.buf = nested.buf;
        w.pad(8);
        w.str("list");
        w.sig("as");
        w.u32(8);
        w.str("abc");
        w.pad(8);
        w.str("w");
        w.sig("d");
        w.pad(8);
        w.buf.extend_from_slice(&210.0f64.to_le_bytes());
        let n = (w.buf.len() - start) as u32;
        w.buf[0..4].copy_from_slice(&n.to_le_bytes());
        let mut c = Cursor {
            b: &w.buf,
            pos: 0,
            le: true,
        };
        let d = c.dict().unwrap();
        assert_eq!(Value::get(&d, "token"), Some(&Value::U32(7)));
        assert_eq!(Value::get(&d, "list"), Some(&Value::Other));
        assert_eq!(Value::get(&d, "w").and_then(Value::as_f64), Some(210.0));
        let Some(Value::Dict(ps)) = Value::get(&d, "page-setup") else {
            panic!("{d:?}");
        };
        assert_eq!(Value::get(ps, "Name"), Some(&Value::Str("iso_a4".into())));
        assert_eq!(Value::get(ps, "Copies"), Some(&Value::U32(2)));
        assert_eq!(c.pos, w.buf.len());
    }

    #[test]
    fn unescapes_bus_addresses() {
        assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%20b"), "/tmp/a b");
    }
}

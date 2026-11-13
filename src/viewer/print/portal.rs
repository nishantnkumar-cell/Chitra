//! Printing through the desktop portal (`org.freedesktop.portal.Print`): the
//! desktop's own print dialog on GNOME and KDE Plasma, inside the Flatpak
//! sandbox too. Chitra asks for the dialog first (PreparePrint), lays the
//! photo out on the paper the user picked, and hands over a PDF (Print).

use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicU32, Ordering};

use super::super::dbus::{Arg, Conn, PORTAL, PORTAL_PATH, SIGNAL, Value, Writer, session_bus_path};

const PRINT: &str = "org.freedesktop.portal.Print";

/// The paper the user picked in the desktop's dialog, in mm, turned to the
/// orientation they picked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSetup {
    pub width_mm: f32,
    pub height_mm: f32,
    /// The largest of the four margins the dialog reports.
    pub margin_mm: f32,
}

pub enum Outcome {
    Printed,
    Cancelled,
}

pub enum Error {
    /// No portal, or one without printing; use Chitra's own dialog instead.
    Unavailable,
    Failed(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Failed(e.to_string())
    }
}

/// Show the desktop's print dialog and print the PDF `make_pdf` writes for
/// the paper chosen there.
pub fn print(
    title: &str,
    make_pdf: impl FnOnce(PageSetup) -> Result<Vec<u8>, String>,
) -> Result<Outcome, Error> {
    let path = session_bus_path().ok_or(Error::Unavailable)?;
    print_on(&path, title, make_pdf)
}

fn print_on(
    path: &std::path::Path,
    title: &str,
    make_pdf: impl FnOnce(PageSetup) -> Result<Vec<u8>, String>,
) -> Result<Outcome, Error> {
    let mut bus = Conn::connect_with_fds(path).map_err(|_| Error::Unavailable)?;
    let name = bus.hello().map_err(|_| Error::Unavailable)?;

    // Step 1: the dialog.
    let token = next_token();
    let request = watch_request(&mut bus, &name, &token)?;
    let mut w = Writer::default();
    w.str("");
    w.str(title);
    w.dict(&[]);
    w.dict(&[]);
    w.dict(&[("handle_token", Arg::Str(&token))]);
    let serial = bus.call(
        PORTAL,
        PORTAL_PATH,
        PRINT,
        "PreparePrint",
        "ssa{sv}a{sv}a{sv}",
        &w.buf,
    )?;
    // An error here means this desktop's portal cannot print.
    bus.reply(serial).map_err(|_| Error::Unavailable)?;
    let results = match wait_response(&mut bus, &request)? {
        Some(r) => r,
        None => return Ok(Outcome::Cancelled),
    };
    let print_token = match Value::get(&results, "token") {
        Some(Value::U32(t)) => *t,
        _ => return Err(Error::Failed("the print dialog gave no print job".into())),
    };
    let setup = match Value::get(&results, "page-setup") {
        Some(Value::Dict(d)) => page_setup(d),
        _ => None,
    }
    .unwrap_or(PageSetup {
        width_mm: 210.0,
        height_mm: 297.0,
        margin_mm: chitra::print::FIT_MARGIN_MM,
    });

    // Step 2: the PDF, as an open file that is already gone from the folder.
    let pdf = make_pdf(setup).map_err(Error::Failed)?;
    let tmp = std::env::temp_dir().join(format!("chitra-print-{}-{token}.pdf", std::process::id()));
    std::fs::write(&tmp, &pdf)?;
    let file = File::open(&tmp);
    let _ = std::fs::remove_file(&tmp);
    let file = file?;

    let token = next_token();
    let request = watch_request(&mut bus, &name, &token)?;
    let mut w = Writer::default();
    w.str("");
    w.str(title);
    w.u32(0); // index of the file sent with the message
    w.dict(&[
        ("handle_token", Arg::Str(&token)),
        ("token", Arg::U32(print_token)),
    ]);
    let serial = bus.call_with_fd(
        PORTAL,
        PORTAL_PATH,
        PRINT,
        "Print",
        "ssha{sv}",
        &w.buf,
        file.as_fd(),
    )?;
    bus.reply(serial)?;
    match wait_response(&mut bus, &request)? {
        Some(_) => Ok(Outcome::Printed),
        None => Ok(Outcome::Cancelled),
    }
}

fn next_token() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "chitra{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Subscribe to the Response signal of the request the portal will create
/// for `token`, before making the call, so the answer cannot be missed.
fn watch_request(bus: &mut Conn, name: &str, token: &str) -> Result<String, Error> {
    let sender = name.trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let rule = format!(
        "type='signal',interface='org.freedesktop.portal.Request',member='Response',path='{path}'"
    );
    let mut w = Writer::default();
    w.str(&rule);
    let serial = bus.call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "AddMatch",
        "s",
        &w.buf,
    )?;
    bus.reply(serial)?;
    Ok(path)
}

/// The results of a portal request, or None when the user cancelled.
fn wait_response(bus: &mut Conn, request: &str) -> Result<Option<Vec<(String, Value)>>, Error> {
    loop {
        let msg = bus.read()?;
        if msg.kind != SIGNAL
            || msg.member.as_deref() != Some("Response")
            || msg.path.as_deref() != Some(request)
        {
            continue;
        }
        let mut c = msg.body_cursor();
        let code = c
            .u32()
            .ok_or_else(|| Error::Failed("bad portal reply".into()))?;
        return match code {
            0 => Ok(Some(c.dict().unwrap_or_default())),
            1 => Ok(None),
            _ => Err(Error::Failed(
                "the print dialog closed with an error".into(),
            )),
        };
    }
}

/// The paper from a GTK-style page setup: Width and Height are the portrait
/// paper size, Orientation turns it.
fn page_setup(d: &[(String, Value)]) -> Option<PageSetup> {
    let num = |k: &str| Value::get(d, k).and_then(Value::as_f64).map(|v| v as f32);
    let (w, h) = (num("Width")?, num("Height")?);
    if !(w > 10.0 && h > 10.0 && w < 2000.0 && h < 2000.0) {
        return None;
    }
    let landscape = matches!(
        Value::get(d, "Orientation"),
        Some(Value::Str(o)) if o.contains("landscape")
    );
    let margin = ["MarginTop", "MarginBottom", "MarginLeft", "MarginRight"]
        .into_iter()
        .filter_map(num)
        .fold(0.0f32, f32::max)
        .clamp(0.0, w.min(h) / 4.0);
    let (width_mm, height_mm) = if landscape { (h, w) } else { (w, h) };
    Some(PageSetup {
        width_mm,
        height_mm,
        margin_mm: margin,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::dbus::{Field, METHOD_CALL, METHOD_RETURN, Message};
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Against a real dbus-daemon with a stand-in print portal: the dialog
    /// answers with a print token, and the PDF arrives with that token.
    #[test]
    fn prints_through_a_portal_on_a_real_bus() {
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

        let mut portal = Conn::connect_with_fds(&sock).unwrap();
        portal.hello().unwrap();
        let mut w = Writer::default();
        w.str(PORTAL);
        w.u32(0);
        let serial = portal
            .call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                "su",
                &w.buf,
            )
            .unwrap();
        portal.reply(serial).unwrap();

        let client_sock = sock.clone();
        let client = std::thread::spawn(move || {
            let mut sizes = None;
            let r = print_on(&client_sock, "test", |setup| {
                sizes = Some(setup);
                Ok(b"%PDF-1.4 test".to_vec())
            });
            (
                r.map(|o| matches!(o, Outcome::Printed))
                    .map_err(|e| match e {
                        Error::Unavailable => "unavailable".to_string(),
                        Error::Failed(e) => e,
                    }),
                sizes,
            )
        });

        let answer = |portal: &mut Conn, call: &Message, results: &[(&str, Arg)]| {
            let mut c = call.body_cursor();
            let (_, _) = (c.str(), c.str());
            let sender = call.sender.clone().unwrap();
            let token = if call.member.as_deref() == Some("PreparePrint") {
                c.dict().unwrap();
                c.dict().unwrap();
                c.dict().unwrap()
            } else {
                c.u32().unwrap();
                c.dict().unwrap()
            };
            let Some(Value::Str(token)) = Value::get(&token, "handle_token") else {
                panic!("no handle_token");
            };
            let path = format!(
                "/org/freedesktop/portal/desktop/request/{}/{token}",
                sender.trim_start_matches(':').replace('.', "_")
            );
            let mut w = Writer::default();
            w.str(&path);
            portal
                .send(
                    METHOD_RETURN,
                    &[(5, Field::U32(call.serial)), (6, Field::Str(&sender))],
                    "o",
                    &w.buf,
                )
                .unwrap();
            let mut w = Writer::default();
            w.u32(0);
            w.dict(results);
            portal
                .send(
                    SIGNAL,
                    &[
                        (1, Field::Path(&path)),
                        (2, Field::Str("org.freedesktop.portal.Request")),
                        (3, Field::Str("Response")),
                        (6, Field::Str(&sender)),
                    ],
                    "ua{sv}",
                    &w.buf,
                )
                .unwrap();
        };
        let next_call = |portal: &mut Conn| loop {
            let m = portal.read().unwrap();
            if m.kind == METHOD_CALL {
                break m;
            }
        };
        let call = next_call(&mut portal);
        assert_eq!(call.member.as_deref(), Some("PreparePrint"));
        answer(&mut portal, &call, &[("token", Arg::U32(9))]);
        let call = next_call(&mut portal);
        assert_eq!(call.member.as_deref(), Some("Print"));
        let mut c = call.body_cursor();
        let (_, _, fd_index) = (c.str(), c.str(), c.u32());
        assert_eq!(fd_index, Some(0));
        let opts = c.dict().unwrap();
        assert_eq!(Value::get(&opts, "token"), Some(&Value::U32(9)));
        answer(&mut portal, &call, &[]);

        let (result, setup) = client.join().unwrap();
        assert_eq!(result, Ok(true));
        assert_eq!(
            setup.unwrap().width_mm,
            210.0,
            "A4 when the dialog says nothing"
        );
        let _ = daemon.kill();
        let _ = daemon.wait();
    }

    #[test]
    fn reads_the_page_setup() {
        let d = vec![
            ("Width".to_string(), Value::F64(101.6)),
            ("Height".to_string(), Value::F64(152.4)),
            ("MarginTop".to_string(), Value::F64(3.0)),
            ("MarginLeft".to_string(), Value::F64(6.35)),
            ("Orientation".to_string(), Value::Str("landscape".into())),
        ];
        let s = page_setup(&d).unwrap();
        assert_eq!((s.width_mm, s.height_mm, s.margin_mm), (152.4, 101.6, 6.35));
        assert!(page_setup(&d[2..]).is_none());
    }
}

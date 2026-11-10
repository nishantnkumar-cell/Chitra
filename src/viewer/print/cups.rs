//! Printing through CUPS with its `lp` and `lpstat` commands, on macOS and on
//! Linux desktops without a print portal. Chitra shows its own small dialog
//! for these (printer, paper, copies) and sends a PDF laid out for the paper.

use std::path::Path;
use std::process::{Command, Stdio};

use chitra::print::Paper;

/// The printers CUPS knows and the default one, if any.
pub fn printers() -> (Vec<String>, Option<String>) {
    let run = |args: &[&str]| {
        Command::new("lpstat")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    // `-e` lists every destination; older CUPS only has `-a`.
    let mut names: Vec<String> = match run(&["-e"]) {
        Some(out) => out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        None => run(&["-a"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .map(String::from)
            .collect(),
    };
    names.dedup();
    let default = ["LPDEST", "PRINTER"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|p| names.contains(p))
        .or_else(|| run(&["-d"]).as_deref().and_then(parse_default));
    (names, default)
}

/// "system default destination: Office" (or a translation with the name last).
fn parse_default(out: &str) -> Option<String> {
    let line = out.lines().next()?;
    let (_, name) = line.rsplit_once(": ")?;
    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

/// Send `pdf` to `printer`. `exact` turns off any scaling, for photos that
/// must keep their physical size.
pub fn submit(
    pdf: &Path,
    printer: &str,
    paper: Paper,
    copies: u32,
    exact: bool,
    title: &str,
) -> Result<(), String> {
    let mut cmd = Command::new("lp");
    cmd.arg("-d")
        .arg(printer)
        .arg("-n")
        .arg(copies.clamp(1, 99).to_string())
        .arg("-t")
        .arg(title)
        .arg("-o")
        .arg(format!("media={}", paper.cups_media()));
    if exact {
        cmd.arg("-o").arg("print-scaling=none");
    }
    let out = cmd
        .arg("--")
        .arg(pdf)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run lp: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("lp: ").to_string())
    }
}

/// The paper most people near the user have: Letter in North America and a
/// few other places, A4 everywhere else.
pub fn local_paper() -> Paper {
    let locale = ["LC_ALL", "LC_PAPER", "LANG"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    let region = locale
        .split(['.', '@'])
        .next()
        .and_then(|l| l.split_once('_'))
        .map(|(_, r)| r.to_ascii_uppercase())
        .unwrap_or_default();
    match region.as_str() {
        "US" | "CA" | "MX" | "PH" | "CL" | "CO" | "VE" | "PR" => Paper::Letter,
        _ => Paper::A4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_default_printer() {
        assert_eq!(
            parse_default("system default destination: Office_Laser\n"),
            Some("Office_Laser".into())
        );
        assert_eq!(parse_default("no system default destination\n"), None);
    }
}

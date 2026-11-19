//! The little network access Chitra has (AI model downloads and the update
//! check) goes through the system's `curl`: part of Windows 10 and later,
//! macOS and most Linux desktops. Chitra itself links no network code.

use std::path::Path;
use std::process::Command;

/// `curl` set up for HTTPS only, failing on HTTP errors, without a console
/// window flashing up on Windows.
pub fn curl() -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--retry",
        "2",
        "--connect-timeout",
        "15",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The body of a small HTTPS GET.
pub fn get_text(url: &str, headers: &[&str]) -> Result<String, String> {
    let mut cmd = curl();
    cmd.args(["--max-time", "20"]);
    for h in headers {
        cmd.arg("-H").arg(h);
    }
    let out = cmd
        .arg(url)
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("curl: ");
        return Err(if err.is_empty() {
            format!("request failed ({})", out.status)
        } else {
            err.to_string()
        });
    }
    String::from_utf8(out.stdout).map_err(|_| "the reply was not text".into())
}

/// Download `url` to `dest`.
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = curl()
        .arg("-o")
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("download failed ({status})"))
    }
}

//! Checking for a newer Chitra on the project's GitHub releases, and
//! fetching the package that fits this computer.
//!
//! Only the public release listing is read: nothing about the user or their
//! photos is sent. Downloads are checked against the SHA-256 GitHub
//! publishes for each release file.

use std::fs;
use std::path::{Path, PathBuf};

use crate::json::{self, Value};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const HOMEPAGE: &str = "https://github.com/nishantnkumar-cell/Chitra";
pub const RELEASES: &str = "https://github.com/nishantnkumar-cell/Chitra/releases";
pub const ISSUES: &str = "https://github.com/nishantnkumar-cell/Chitra/issues";
pub const USER_GUIDE: &str =
    "https://github.com/nishantnkumar-cell/Chitra/blob/main/docs/USER_GUIDE.md";
const LATEST_API: &str = "https://api.github.com/repos/nishantnkumar-cell/Chitra/releases/latest";
/// Every link taken from the reply must start with this, so a tampered
/// reply cannot send the user elsewhere.
const OWN_PREFIX: &str = "https://github.com/nishantnkumar-cell/Chitra/";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.3.1", without the leading "v".
    pub version: String,
    /// The release's web page.
    pub page: String,
    /// The release notes (Markdown), shortened.
    pub notes: String,
    /// The package for this computer, when there is one.
    pub package: Option<Package>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lower-case hex SHA-256 published with the release; always set, because a
    /// package without one is not offered.
    pub sha256: Option<String>,
}

/// What kind of package this copy of Chitra came from, which decides how it
/// is updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Windows: the installer is downloaded and run.
    WindowsInstaller,
    /// Linux tarball: the new tarball is downloaded.
    LinuxTarball,
    /// Debian/Ubuntu package: the new .deb is downloaded.
    Deb,
    /// Flatpak: updated by the system's software centre, no network here.
    Flatpak,
    /// macOS or anything else: the release page is opened.
    Other,
}

impl Install {
    pub fn current() -> Install {
        if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
            Install::Flatpak
        } else if cfg!(all(windows, target_arch = "x86_64")) {
            Install::WindowsInstaller
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            let from_deb = std::env::current_exe().is_ok_and(|p| p.starts_with("/usr/bin"))
                && Path::new("/var/lib/dpkg/info/chitra.list").exists();
            if from_deb {
                Install::Deb
            } else {
                Install::LinuxTarball
            }
        } else {
            Install::Other
        }
    }

    /// Whether the app checks for updates itself.
    pub fn checks(self) -> bool {
        self != Install::Flatpak
    }

    fn wants(self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        match self {
            Install::WindowsInstaller => n.starts_with("chitra-setup-") && n.ends_with(".exe"),
            Install::LinuxTarball => n.ends_with("-linux-x86_64.tar.gz"),
            Install::Deb => n.starts_with("chitra_") && n.ends_with("_amd64.deb"),
            Install::Flatpak | Install::Other => false,
        }
    }
}

/// `a` is a later version than `b` ("0.10.0" > "0.9.2"; a leading "v" and
/// a pre-release suffix such as "-rc1" are ignored).
pub fn is_newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let v = v.split(['-', '+']).next().unwrap_or("");
        let mut out = [0; 3];
        for (o, p) in out.iter_mut().zip(v.split('.')) {
            *o = p.parse().unwrap_or(0);
        }
        out
    }
    parts(a) > parts(b)
}

/// The latest release, from GitHub's listing.
pub fn parse_latest(text: &str, install: Install) -> Result<Release, String> {
    let v = json::parse(text)?;
    if v.get("draft").and_then(Value::as_bool) == Some(true) {
        return Err("the latest release is a draft".into());
    }
    let tag = v
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("no version in the reply")?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if version.is_empty() || !version.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!("unexpected version \"{tag}\""));
    }
    let page = v
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with(OWN_PREFIX))
        .map_or_else(|| RELEASES.to_string(), str::to_string);
    let mut notes: String = v
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(2000)
        .collect();
    notes = notes.replace("\r\n", "\n").trim().to_string();
    let package = v
        .get("assets")
        .map(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .find_map(|a| {
            let name = a.get("name")?.as_str()?;
            if !install.wants(name) || name.contains(['/', '\\']) {
                return None;
            }
            let url = a.get("browser_download_url")?.as_str()?;
            if !url.starts_with(&format!("{OWN_PREFIX}releases/download/")) {
                return None;
            }
            // A download without a published checksum is not offered: the
            // person is sent to the release page instead.
            let sha256 = a
                .get("digest")
                .and_then(Value::as_str)
                .and_then(|d| d.strip_prefix("sha256:"))
                .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_ascii_lowercase)?;
            Some(Package {
                name: name.to_string(),
                url: url.to_string(),
                size: a.get("size").and_then(Value::as_f64).unwrap_or(0.0) as u64,
                sha256: Some(sha256),
            })
        });
    Ok(Release {
        version,
        page,
        notes,
        package,
    })
}

/// Ask GitHub for the latest release.
pub fn latest() -> Result<Release, String> {
    let text = crate::net::get_text(
        LATEST_API,
        &[
            "Accept: application/vnd.github+json",
            &format!("User-Agent: Chitra/{VERSION}"),
        ],
    )
    .map_err(|e| format!("could not reach GitHub: {e}"))?;
    parse_latest(&text, Install::current())
}

/// Where a downloaded package goes: the Downloads folder, or the temporary
/// folder for the Windows installer (it is run straight away).
pub fn download_dir(install: Install) -> PathBuf {
    if install == Install::WindowsInstaller {
        return std::env::temp_dir();
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(d) = home
        .as_ref()
        .map(|h| h.join("Downloads"))
        .filter(|d| d.is_dir())
    {
        return d;
    }
    home.unwrap_or_else(std::env::temp_dir)
}

/// Download `pkg` into `dir`, checking its checksum. `part` is where it is
/// written meanwhile, so the caller can show progress from its size.
pub fn part_path(pkg: &Package, dir: &Path) -> PathBuf {
    dir.join(format!("{}.part", pkg.name))
}

pub fn download(pkg: &Package, dir: &Path) -> Result<PathBuf, String> {
    let Some(want) = &pkg.sha256 else {
        return Err("no checksum is published for this download; use the release page".into());
    };
    let dest = dir.join(&pkg.name);
    let part = part_path(pkg, dir);
    crate::net::download(&pkg.url, &part).inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })?;
    let got = crate::ai::file_sha256(&part)?;
    if &got != want {
        let _ = fs::remove_file(&part);
        return Err("the download did not match its published checksum".into());
    }
    fs::rename(&part, &dest).map_err(|e| format!("cannot save {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Open a web page or folder with the system's default handler.
pub fn open(target: &str) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(target)
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot open {target}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("V0.3.0-rc1", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
    }

    const LISTING: &str = r#"{
      "html_url": "https://github.com/nishantnkumar-cell/Chitra/releases/tag/v0.4.0",
      "tag_name": "v0.4.0", "draft": false, "body": "New:\r\n- About window",
      "assets": [
        {"name": "Chitra-0.4.0-x86_64.flatpak", "size": 5,
         "browser_download_url": "https://github.com/nishantnkumar-cell/Chitra/releases/download/v0.4.0/Chitra-0.4.0-x86_64.flatpak"},
        {"name": "chitra-0.4.0-linux-x86_64.tar.gz", "size": 15351255,
         "digest": "sha256:EA41289501608F743915E597CD870575E64098B1947949A148EE878C6E50A82F",
         "browser_download_url": "https://github.com/nishantnkumar-cell/Chitra/releases/download/v0.4.0/chitra-0.4.0-linux-x86_64.tar.gz"},
        {"name": "chitra_0.4.0_amd64.deb", "size": 12447972,
         "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000001",
         "browser_download_url": "https://github.com/nishantnkumar-cell/Chitra/releases/download/v0.4.0/chitra_0.4.0_amd64.deb"},
        {"name": "Chitra-Setup-0.4.0-x64.exe", "size": 9496378, "digest": null,
         "browser_download_url": "https://evil.example/Chitra-Setup-0.4.0-x64.exe"}
      ]}"#;

    #[test]
    fn reads_the_release_listing() {
        let r = parse_latest(LISTING, Install::LinuxTarball).unwrap();
        assert_eq!(r.version, "0.4.0");
        assert!(r.page.ends_with("/tag/v0.4.0"));
        assert_eq!(r.notes, "New:\n- About window");
        let p = r.package.unwrap();
        assert_eq!(p.name, "chitra-0.4.0-linux-x86_64.tar.gz");
        assert_eq!(p.size, 15351255);
        assert_eq!(
            p.sha256.as_deref(),
            Some("ea41289501608f743915e597cd870575e64098b1947949a148ee878c6e50a82f")
        );
    }

    #[test]
    fn ignores_links_outside_the_project() {
        let r = parse_latest(LISTING, Install::WindowsInstaller).unwrap();
        assert_eq!(r.package, None);
        assert_eq!(
            parse_latest(LISTING, Install::Flatpak).unwrap().package,
            None
        );
    }

    #[test]
    fn finds_the_debian_package() {
        let deb = parse_latest(LISTING, Install::Deb)
            .unwrap()
            .package
            .unwrap();
        assert_eq!(deb.name, "chitra_0.4.0_amd64.deb");
        assert_eq!(deb.sha256.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn a_package_without_a_checksum_is_not_offered() {
        // The flatpak asset in LISTING has no digest at all.
        let listing = LISTING.replace("sha256:", "md5:");
        for install in [Install::LinuxTarball, Install::Deb] {
            assert_eq!(parse_latest(&listing, install).unwrap().package, None);
        }
        let pkg = Package {
            name: "x.deb".into(),
            url: format!("{OWN_PREFIX}releases/download/v1/x.deb"),
            size: 1,
            sha256: None,
        };
        assert!(download(&pkg, &std::env::temp_dir()).is_err());
    }
}

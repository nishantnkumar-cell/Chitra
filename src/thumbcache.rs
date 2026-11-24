//! The thumbnail cache that Linux desktops share (the freedesktop.org
//! Thumbnail Managing Standard): `~/.cache/thumbnails/{normal,large}/<md5 of
//! the file's URI>.png`, a PNG that records the URI and modification time of
//! the photo it shows. File managers and viewers fill and read the same folder,
//! so a folder already browsed in Dolphin or Files shows in Chitra at once, and
//! the other way round. Without it every thumbnail is a full decode of a
//! photo, every time. Off Linux this does nothing.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use image::RgbaImage;

/// A thumbnail found in the cache.
pub struct Cached {
    pub image: RgbaImage,
    /// Size of the photo it shows (from the cache entry, else the thumbnail's).
    pub full_size: [u32; 2],
}

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether the shared cache is in use: on Linux and not turned off.
pub fn enabled() -> bool {
    cfg!(target_os = "linux") && ENABLED.load(Ordering::Relaxed)
}

/// Turn the shared cache on or off (a setting).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The cache folder and size in pixels for thumbnails of up to `side` pixels:
/// the standard's `normal` (128) or `large` (256); bigger ones are not cached.
pub fn class(side: u32) -> Option<(&'static str, u32)> {
    match side {
        0..=128 => Some(("normal", 128)),
        129..=256 => Some(("large", 256)),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
/// `file://` and the absolute path with everything but unreserved characters
/// and `/` percent-encoded (what file managers hash).
pub fn uri(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let abs = std::path::absolute(path).ok()?;
    let mut out = String::from("file://");
    for &b in abs.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Some(out)
}

#[cfg(target_os = "linux")]
fn file_name(uri: &str) -> String {
    use md5::{Digest, Md5};
    let sum = Md5::digest(uri.as_bytes());
    let mut s = String::with_capacity(36);
    for b in sum {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str(".png");
    s
}

#[cfg(target_os = "linux")]
fn mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(target_os = "linux")]
fn root() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache")))?;
    Some(base.join("thumbnails"))
}

/// A valid cache entry for `path` for thumbnails of up to `side` pixels.
pub fn lookup(path: &Path, side: u32) -> Option<Cached> {
    #[cfg(target_os = "linux")]
    {
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        lookup_in(&root()?, path, side)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, side);
        None
    }
}

/// Keep `thumb` (at most `class(side)` pixels, the photo being `full` pixels)
/// for next time. Failures are ignored: it is only a cache.
pub fn store(path: &Path, side: u32, thumb: &RgbaImage, full: (u32, u32)) {
    #[cfg(target_os = "linux")]
    {
        if ENABLED.load(Ordering::Relaxed)
            && let Some(root) = root()
        {
            let _ = store_in(&root, path, side, thumb, full);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, side, thumb, full);
    }
}

#[cfg(target_os = "linux")]
fn lookup_in(root: &Path, path: &Path, side: u32) -> Option<Cached> {
    let (dir, _) = class(side)?;
    let uri = uri(path)?;
    let file = root.join(dir).join(file_name(&uri));
    // A thumbnail is small; anything big is not one.
    if std::fs::metadata(&file).ok()?.len() > 4 << 20 {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .ok()?;
    let info = reader.info();
    let text = |key: &str| {
        info.uncompressed_latin1_text
            .iter()
            .find(|t| t.keyword == key)
            .map(|t| t.text.clone())
    };
    if text("Thumb::URI")? != uri || text("Thumb::MTime")?.parse::<u64>().ok()? != mtime_secs(path)?
    {
        return None;
    }
    let dims = (
        text("Thumb::Image::Width").and_then(|v| v.parse::<u32>().ok()),
        text("Thumb::Image::Height").and_then(|v| v.parse::<u32>().ok()),
    );
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let full_size = match dims {
        (Some(w), Some(h)) => [w, h],
        _ => [image.width(), image.height()],
    };
    Some(Cached { image, full_size })
}

#[cfg(target_os = "linux")]
fn store_in(
    root: &Path,
    path: &Path,
    side: u32,
    thumb: &RgbaImage,
    full: (u32, u32),
) -> Option<()> {
    use std::io::BufWriter;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let (dir, _) = class(side)?;
    let uri = uri(path)?;
    let mtime = mtime_secs(path)?;
    let size = std::fs::metadata(path).ok()?.len();
    let folder = root.join(dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .ok()?;
    let name = file_name(&uri);
    let tmp = folder.join(format!(".{name}.{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .ok()?;
    let write = || -> Result<(), png::EncodingError> {
        let mut enc = png::Encoder::new(BufWriter::new(file), thumb.width(), thumb.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        for (k, v) in [
            ("Thumb::URI", uri.clone()),
            ("Thumb::MTime", mtime.to_string()),
            ("Thumb::Size", size.to_string()),
            ("Thumb::Image::Width", full.0.to_string()),
            ("Thumb::Image::Height", full.1.to_string()),
            ("Software", "Chitra".to_string()),
        ] {
            enc.add_text_chunk(k.to_string(), v)?;
        }
        let mut w = enc.write_header()?;
        w.write_image_data(thumb.as_raw())?;
        w.finish()
    };
    if write().is_err() || std::fs::rename(&tmp, folder.join(&name)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn names_and_uris_follow_the_standard() {
        assert_eq!(uri(Path::new("/tmp/a.png")).unwrap(), "file:///tmp/a.png");
        assert_eq!(
            uri(Path::new("/tmp/a b/\u{e9}.png")).unwrap(),
            "file:///tmp/a%20b/%C3%A9.png"
        );
        // md5 of "file:///tmp/a.png", worked out with another tool.
        assert_eq!(
            file_name("file:///tmp/a.png"),
            "a04bfd79b77efaccf5f6adb271b86f1e.png"
        );
        assert_eq!(class(100), Some(("normal", 128)));
        assert_eq!(class(200), Some(("large", 256)));
        assert_eq!(class(300), None);
    }

    #[test]
    fn a_thumbnail_comes_back_until_the_photo_changes() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("p.jpg");
        std::fs::write(&photo, b"not really a photo").unwrap();
        let root = dir.path().join("cache");
        let thumb = RgbaImage::from_fn(30, 20, |x, y| {
            image::Rgba([x as u8 * 8, y as u8 * 12, 7, 255])
        });
        assert!(lookup_in(&root, &photo, 128).is_none(), "nothing yet");
        store_in(&root, &photo, 128, &thumb, (6000, 4000)).unwrap();
        let got = lookup_in(&root, &photo, 100).expect("found");
        assert_eq!(got.image, thumb);
        assert_eq!(got.full_size, [6000, 4000]);
        // The other size class is separate.
        assert!(lookup_in(&root, &photo, 200).is_none());
        // Changing the photo invalidates it.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&photo)
            .unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        assert!(lookup_in(&root, &photo, 128).is_none());
    }
}

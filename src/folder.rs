//! Listing and sorting the images in a folder.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::format::is_supported_path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    /// Natural order: "img2" before "img10".
    #[default]
    Name,
    Modified,
    Size,
    Extension,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [
        SortKey::Name,
        SortKey::Modified,
        SortKey::Size,
        SortKey::Extension,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Modified => "Date modified",
            SortKey::Size => "File size",
            SortKey::Extension => "Type",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl Entry {
    pub fn from_path(path: PathBuf) -> Entry {
        let meta = fs::metadata(&path).ok();
        Entry {
            size: meta.as_ref().map_or(0, |m| m.len()),
            modified: meta.and_then(|m| m.modified().ok()),
            path,
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Supported images directly inside `dir` (not recursive), unsorted.
pub fn list_images(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        if !is_supported_path(&path) {
            continue;
        }
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(Entry {
            path,
            size: meta.len(),
            modified: meta.modified().ok(),
        });
    }
    Ok(out)
}

/// The first `limit` photos found inside the sub-folders of `dir`, walking
/// them depth-first in natural order (each folder's photos in the given sort
/// order). For folders whose photos are all in sub-folders, such as
/// Trips/2024 Goa. Stops after `max_dirs` folders so a huge tree stays quick.
pub fn images_below(
    dir: &Path,
    limit: usize,
    max_dirs: usize,
    key: SortKey,
    descending: bool,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = list_subfolders(dir).into_iter().rev().collect();
    let mut visited = 0;
    while let Some(d) = stack.pop() {
        if out.len() >= limit || visited >= max_dirs {
            break;
        }
        visited += 1;
        let mut photos = list_images(&d).unwrap_or_default();
        sort(&mut photos, key, descending);
        photos.truncate(limit - out.len());
        out.extend(photos);
        stack.extend(list_subfolders(&d).into_iter().rev());
    }
    out
}

/// Sub-folders directly inside `dir`, in natural name order. Hidden and
/// system folders (names starting with `.` or `$`, "System Volume
/// Information") are skipped.
pub fn list_subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = read
        .flatten()
        .filter(|item| item.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|item| {
            let name = item.file_name();
            let name = name.to_string_lossy();
            !(name.starts_with('.')
                || name.starts_with('$')
                || name.eq_ignore_ascii_case("System Volume Information"))
        })
        .map(|item| item.path())
        .collect();
    out.sort_by(|a, b| natural_cmp(a, b));
    out
}

pub fn natural_cmp(a: &Path, b: &Path) -> Ordering {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    natord::compare_ignore_case(&name(a), &name(b)).then_with(|| a.cmp(b))
}

pub fn sort(entries: &mut [Entry], key: SortKey, descending: bool) {
    entries.sort_by(|a, b| {
        let primary = match key {
            SortKey::Name => Ordering::Equal,
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Extension => {
                let ext = |p: &Path| {
                    p.extension()
                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default()
                };
                ext(&a.path).cmp(&ext(&b.path))
            }
        };
        let ord = primary.then_with(|| natural_cmp(&a.path, &b.path));
        if descending { ord.reverse() } else { ord }
    });
}

/// Expand files and folders into a list of image files for batch work.
/// Explicitly named files are always kept; folders contribute supported images.
pub fn collect_inputs(paths: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk(path, recursive, &mut out);
        } else {
            out.push(path.clone());
        }
    }
    out
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            dirs.push(path);
        } else if kind.is_file() && is_supported_path(&path) {
            files.push(path);
        }
    }
    files.sort_by(|a, b| natural_cmp(a, b));
    out.extend(files);
    if recursive {
        dirs.sort_by(|a, b| natural_cmp(a, b));
        for d in dirs {
            walk(&d, true, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v: Vec<Entry> = ["img10.jpg", "IMG2.jpg", "img1.png"]
            .iter()
            .map(|n| Entry {
                path: PathBuf::from(n),
                size: 0,
                modified: None,
            })
            .collect();
        sort(&mut v, SortKey::Name, false);
        let names: Vec<_> = v.iter().map(|e| e.file_name()).collect();
        assert_eq!(names, ["img1.png", "IMG2.jpg", "img10.jpg"]);
        sort(&mut v, SortKey::Name, true);
        assert_eq!(v[0].file_name(), "img10.jpg");
    }

    #[test]
    fn subfolders_in_natural_order() {
        let dir = std::env::temp_dir().join(format!("chitra-subfolders-{}", std::process::id()));
        for name in ["day10", "Day2", ".hidden", "$RECYCLE.BIN", "day1"] {
            fs::create_dir_all(dir.join(name)).unwrap();
        }
        fs::write(dir.join("photo.jpg"), b"").unwrap();
        let names: Vec<String> = list_subfolders(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["day1", "Day2", "day10"]);
    }

    #[test]
    fn images_below_walks_sub_folders_in_order() {
        let dir = std::env::temp_dir().join(format!("chitra-below-{}", std::process::id()));
        for f in [
            "2025 Leh/b.jpg",
            "2024 Goa/beach/x.png",
            "2024 Goa/a.jpg",
            "2024 Goa/c.jpg",
        ] {
            let p = dir.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"").unwrap();
        }
        let names = |v: Vec<Entry>| -> Vec<String> { v.iter().map(|e| e.file_name()).collect() };
        let all = images_below(&dir, 10, 100, SortKey::Name, false);
        let two = images_below(&dir, 2, 100, SortKey::Name, false);
        let one_dir = images_below(&dir, 10, 1, SortKey::Name, false);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names(all), ["a.jpg", "c.jpg", "x.png", "b.jpg"]);
        assert_eq!(names(two), ["a.jpg", "c.jpg"]);
        assert_eq!(names(one_dir), ["a.jpg", "c.jpg"]);
    }

    #[test]
    fn sort_by_size_then_name() {
        let mk = |n: &str, s| Entry {
            path: PathBuf::from(n),
            size: s,
            modified: None,
        };
        let mut v = vec![mk("b.jpg", 5), mk("a.jpg", 5), mk("c.jpg", 1)];
        sort(&mut v, SortKey::Size, false);
        let names: Vec<_> = v.iter().map(|e| e.file_name()).collect();
        assert_eq!(names, ["c.jpg", "a.jpg", "b.jpg"]);
    }
}

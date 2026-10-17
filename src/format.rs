//! The image formats Chitra reads and writes, and how to recognise them.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// A supported image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Jpeg,
    JpegXl,
    Png,
    Gif,
    Bmp,
    WebP,
    /// AVIF (AV1-coded HEIF).
    Avif,
    /// HEIF/HEIC (HEVC-coded HEIF, as from iPhones). Read only.
    Heif,
}

impl Format {
    /// Every format Chitra can open.
    pub const ALL: [Format; 8] = [
        Format::Jpeg,
        Format::JpegXl,
        Format::Png,
        Format::Gif,
        Format::Bmp,
        Format::WebP,
        Format::Avif,
        Format::Heif,
    ];

    /// The formats Chitra can write.
    pub const WRITABLE: [Format; 7] = [
        Format::Jpeg,
        Format::JpegXl,
        Format::Png,
        Format::Gif,
        Format::Bmp,
        Format::WebP,
        Format::Avif,
    ];

    /// Short display name, e.g. "JPEG XL".
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::JpegXl => "JPEG XL",
            Format::Png => "PNG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::WebP => "WebP",
            Format::Avif => "AVIF",
            Format::Heif => "HEIF",
        }
    }

    /// Preferred file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::JpegXl => "jxl",
            Format::Png => "png",
            Format::Gif => "gif",
            Format::Bmp => "bmp",
            Format::WebP => "webp",
            Format::Avif => "avif",
            Format::Heif => "heic",
        }
    }

    /// Every extension recognised for this format, lower case.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Format::JpegXl => &["jxl"],
            Format::Png => &["png", "apng"],
            Format::Gif => &["gif"],
            Format::Bmp => &["bmp", "dib"],
            Format::WebP => &["webp"],
            Format::Avif => &["avif"],
            Format::Heif => &["heic", "heif", "hif"],
        }
    }

    /// Whether Chitra can write this format (HEIF is read only).
    pub fn can_write(self) -> bool {
        self != Format::Heif
    }

    /// Whether this format can hold an alpha channel when written by Chitra.
    pub fn supports_alpha(self) -> bool {
        !matches!(self, Format::Jpeg | Format::Bmp)
    }

    pub fn from_extension(ext: &str) -> Option<Format> {
        let ext = ext.to_ascii_lowercase();
        Format::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
    }

    pub fn from_path(path: &Path) -> Option<Format> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Format::from_extension)
    }

    /// Identify a format from the first bytes of a file.
    pub fn from_magic(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Format::Png)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Format::Gif)
        } else if bytes.starts_with(b"BM") && bytes.len() >= 14 {
            Some(Format::Bmp)
        } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Format::WebP)
        } else if bytes.starts_with(&[0xFF, 0x0A])
            || bytes.starts_with(&[
                0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
            ])
        {
            Some(Format::JpegXl)
        } else if has_brand(
            bytes,
            &[b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"],
        ) {
            Some(Format::Heif)
        } else if has_brand(bytes, &[b"avif", b"avis"]) {
            Some(Format::Avif)
        } else {
            None
        }
    }
}

/// An ISO-BMFF `ftyp` box naming one of `brands` as its major or a compatible
/// brand. HEIC and AVIF share the container and differ in these brands.
fn has_brand(bytes: &[u8], brands: &[&[u8]]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(16, bytes.len());
    // Major brand at 8..12, minor version at 12..16, compatible brands after.
    std::iter::once(&bytes[8..12])
        .chain(bytes[16..end].chunks_exact(4))
        .any(|brand| brands.contains(&brand))
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().trim_start_matches('.').to_ascii_lowercase();
        let format = match s.as_str() {
            "jpegxl" | "jpeg-xl" | "jpeg_xl" => Some(Format::JpegXl),
            other => Format::from_extension(other),
        };
        match format {
            Some(f) if f.can_write() => Ok(f),
            Some(f) => Err(format!(
                "{f} can be read but not written (choose jpg, jxl, png, gif, bmp, webp or avif)"
            )),
            None => Err(format!(
                "unknown format '{s}' (expected one of: jpg, jxl, png, gif, bmp, webp, avif)"
            )),
        }
    }
}

/// True when the path has an extension Chitra can open.
pub fn is_supported_path(path: &Path) -> bool {
    Format::from_path(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_extensions() {
        assert_eq!("JPG".parse::<Format>().unwrap(), Format::Jpeg);
        assert_eq!(".jpeg".parse::<Format>().unwrap(), Format::Jpeg);
        assert_eq!("jxl".parse::<Format>().unwrap(), Format::JpegXl);
        assert_eq!("jpeg-xl".parse::<Format>().unwrap(), Format::JpegXl);
        assert_eq!("WebP".parse::<Format>().unwrap(), Format::WebP);
        assert!("tiff".parse::<Format>().is_err());
        // HEIF is read only, so it is refused as an output format.
        assert!(
            "heic"
                .parse::<Format>()
                .unwrap_err()
                .contains("not written")
        );
    }

    #[test]
    fn magic_numbers() {
        assert_eq!(
            Format::from_magic(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(Format::Jpeg)
        );
        assert_eq!(Format::from_magic(b"GIF89a...."), Some(Format::Gif));
        assert_eq!(
            Format::from_magic(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(Format::WebP)
        );
        assert_eq!(
            Format::from_magic(&[0xFF, 0x0A, 0x00]),
            Some(Format::JpegXl)
        );
        assert_eq!(
            Format::from_magic(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
            Some(Format::Heif)
        );
        assert_eq!(
            Format::from_magic(b"\0\0\0\x18ftypmif1\0\0\0\0mif1heic"),
            Some(Format::Heif)
        );
        // AVIF shares the container but is a different format.
        assert_eq!(
            Format::from_magic(b"\0\0\0\x18ftypavif\0\0\0\0mif1avif"),
            Some(Format::Avif)
        );
        assert_eq!(Format::from_magic(b"hello"), None);
        assert_eq!(Format::from_magic(&[]), None);
    }

    #[test]
    fn supported_paths() {
        assert!(is_supported_path(Path::new("a/b/photo.JPEG")));
        assert!(is_supported_path(Path::new("x.jxl")));
        assert!(is_supported_path(Path::new("IMG_0001.HEIC")));
        assert!(!is_supported_path(Path::new("notes.txt")));
        assert!(!is_supported_path(Path::new("noext")));
    }
}

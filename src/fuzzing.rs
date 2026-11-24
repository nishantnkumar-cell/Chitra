//! Entry points for fuzzing (`fuzz/`) and the mutation test
//! (`tests/robustness.rs`). They run the parsers on arbitrary bytes *without*
//! the panic net the app puts around codecs, so a panic shows up as the bug it
//! is. Nothing here is part of the supported API.

use std::path::Path;

use crate::decode::{DecodeOptions, decode_unisolated};
use crate::format::Format;

/// Decode `data` as the format its first bytes say (else as each format).
pub fn decode(data: &[u8]) {
    match Format::from_magic(data) {
        Some(f) => decode_as(data, f),
        None => {
            for f in Format::ALL {
                decode_as(data, f);
            }
        }
    }
}

/// Decode `data` as `format`, as a still and as an animation.
pub fn decode_as(data: &[u8], format: Format) {
    for animation in [false, true] {
        let _ = decode_unisolated(data, format, DecodeOptions { animation });
    }
}

/// Parse `data` as a PyTorch weights file.
pub fn pth(data: &[u8]) {
    crate::ai::fuzz_pth(data);
}

/// Parse `data` as the JSON of a release listing.
pub fn release_json(data: &[u8]) {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = crate::json::parse(text);
        let _ = crate::update::parse_latest(text, crate::update::Install::LinuxTarball);
    }
}

/// Turn `data` into a file name with the rename pattern engine.
pub fn rename(data: &[u8]) {
    if let Ok(pattern) = std::str::from_utf8(data) {
        let name =
            crate::convert::render_name(pattern, Path::new("/a/b/photo.jpg"), 7, Format::Png);
        assert!(
            !name.contains(['/', '\\']),
            "a file name must stay in its folder"
        );
    }
}

/// Read `data` as an EXIF block: fields, orientation and location removal.
pub fn exif(data: &[u8]) {
    let _ = crate::metadata::readable_fields(data);
    let _ = crate::metadata::orientation(data);
    let clean = crate::metadata::strip_gps(data);
    assert_eq!(clean.len(), data.len(), "location removal keeps the size");
    let _ = crate::metadata::normalize_exif(data.to_vec());
}

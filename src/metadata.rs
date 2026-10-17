//! EXIF helpers: normalising raw blocks, reading the fields worth showing, and
//! resetting the orientation tag once pixels have been rotated.

use image::metadata::Orientation;

/// Strip any wrapper so the block starts at the TIFF header ("II*\0" or "MM\0*").
pub fn normalize_exif(mut raw: Vec<u8>) -> Option<Vec<u8>> {
    if raw.starts_with(b"Exif\0\0") {
        raw.drain(..6);
    }
    let is_tiff = raw.starts_with(b"II*\0") || raw.starts_with(b"MM\0*");
    if is_tiff { Some(raw) } else { None }
}

/// Orientation stored in an EXIF block (TIFF form).
pub fn orientation(exif: &[u8]) -> Orientation {
    Orientation::from_exif_chunk(exif).unwrap_or(Orientation::NoTransforms)
}

/// Return a copy of the EXIF block with orientation set to "normal", for writing
/// next to pixels that already have the rotation applied.
pub fn exif_for_output(exif: &[u8]) -> Vec<u8> {
    let mut out = exif.to_vec();
    let _ = Orientation::remove_from_exif_chunk(&mut out);
    out
}

/// Human-readable EXIF fields, in a stable order, for the info panel and `chtra info`.
pub fn readable_fields(exif: &[u8]) -> Vec<(String, String)> {
    use exif::{In, Tag};

    let Ok(parsed) = exif::Reader::new().read_raw(exif.to_vec()) else {
        return Vec::new();
    };
    const WANTED: &[(Tag, &str)] = &[
        (Tag::Make, "Camera make"),
        (Tag::Model, "Camera model"),
        (Tag::LensModel, "Lens"),
        (Tag::DateTimeOriginal, "Taken"),
        (Tag::ExposureTime, "Exposure"),
        (Tag::FNumber, "Aperture"),
        (Tag::PhotographicSensitivity, "ISO"),
        (Tag::FocalLength, "Focal length"),
        (Tag::FocalLengthIn35mmFilm, "Focal length (35 mm)"),
        (Tag::ExposureBiasValue, "Exposure bias"),
        (Tag::Flash, "Flash"),
        (Tag::WhiteBalance, "White balance"),
        (Tag::Orientation, "Orientation"),
        (Tag::Software, "Software"),
        (Tag::Artist, "Artist"),
        (Tag::Copyright, "Copyright"),
    ];
    let mut out = Vec::new();
    for (tag, label) in WANTED {
        if let Some(field) = parsed.get_field(*tag, In::PRIMARY) {
            let value = if *tag == Tag::Orientation {
                orientation_label(field.value.get_uint(0).unwrap_or(1)).to_string()
            } else {
                field.display_value().with_unit(&parsed).to_string()
            };
            let value = value.trim_matches('"').trim().to_string();
            if !value.is_empty() {
                out.push((label.to_string(), value));
            }
        }
    }
    if let Some(gps) = gps_position(&parsed) {
        out.push(("GPS".to_string(), gps));
    }
    out
}

fn orientation_label(value: u32) -> &'static str {
    match value {
        1 => "Normal",
        2 => "Mirrored",
        3 => "Rotated 180°",
        4 => "Mirrored vertically",
        5 => "Mirrored, rotated 90° left",
        6 => "Rotated 90° right",
        7 => "Mirrored, rotated 90° right",
        8 => "Rotated 90° left",
        _ => "Unknown",
    }
}

fn gps_position(parsed: &exif::Exif) -> Option<String> {
    use exif::{In, Tag, Value};

    fn degrees(field: &exif::Field) -> Option<f64> {
        match &field.value {
            Value::Rational(v) if v.len() == 3 => {
                Some(v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0)
            }
            _ => None,
        }
    }
    fn reference(parsed: &exif::Exif, tag: Tag) -> Option<char> {
        let field = parsed.get_field(tag, In::PRIMARY)?;
        match &field.value {
            Value::Ascii(v) => v.first()?.first().map(|b| *b as char),
            _ => None,
        }
    }
    let lat = degrees(parsed.get_field(Tag::GPSLatitude, In::PRIMARY)?)?;
    let lon = degrees(parsed.get_field(Tag::GPSLongitude, In::PRIMARY)?)?;
    let lat = if reference(parsed, Tag::GPSLatitudeRef) == Some('S') {
        -lat
    } else {
        lat
    };
    let lon = if reference(parsed, Tag::GPSLongitudeRef) == Some('W') {
        -lon
    } else {
        lon
    };
    Some(format!("{lat:.6}, {lon:.6}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal little-endian TIFF with a single Orientation entry.
    fn tiff_with_orientation(value: u16) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"II*\0");
        t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        t.extend_from_slice(&1u16.to_le_bytes()); // one entry
        t.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        t.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        t.extend_from_slice(&1u32.to_le_bytes()); // count
        t.extend_from_slice(&value.to_le_bytes());
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        t
    }

    #[test]
    fn normalize_strips_prefix() {
        let mut raw = b"Exif\0\0".to_vec();
        raw.extend(tiff_with_orientation(6));
        let norm = normalize_exif(raw).unwrap();
        assert!(norm.starts_with(b"II*\0"));
        assert!(normalize_exif(b"garbage".to_vec()).is_none());
    }

    #[test]
    fn orientation_round_trip() {
        let exif = tiff_with_orientation(6);
        assert_eq!(orientation(&exif), Orientation::Rotate90);
        let reset = exif_for_output(&exif);
        assert_eq!(orientation(&reset), Orientation::NoTransforms);
        let fields = readable_fields(&exif);
        assert!(fields.iter().any(|(k, _)| k == "Orientation"));
    }
}

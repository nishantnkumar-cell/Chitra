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

/// A copy of an EXIF block (TIFF form) with its GPS directory blanked: the
/// position, altitude, time and every other GPS value (and the bytes they point
/// to) are zeroed, so the location is gone and not just hidden. The rest of the
/// EXIF stays, and the block stays valid (the GPS directory is empty). A block
/// that cannot be read is returned unchanged. Location can also sit in XMP or a
/// camera's own MakerNote; those are not touched.
pub fn strip_gps(exif: &[u8]) -> Vec<u8> {
    let mut out = exif.to_vec();
    let little = match out.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return out,
    };
    let r16 = |b: &[u8], o: usize| -> Option<usize> {
        let s = b.get(o..o.checked_add(2)?)?;
        Some(usize::from(if little {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        }))
    };
    let r32 = |b: &[u8], o: usize| -> Option<usize> {
        let s = b.get(o..o.checked_add(4)?)?;
        let v = if little {
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        } else {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]])
        };
        usize::try_from(v).ok()
    };
    let size_of = |kind: usize| -> usize {
        match kind {
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => 1,
        }
    };
    let Some(ifd0) = r32(&out, 4) else { return out };
    let Some(n0) = r16(&out, ifd0) else {
        return out;
    };
    for i in 0..n0.min(512) {
        let e = ifd0 + 2 + 12 * i;
        if r16(&out, e) != Some(0x8825) {
            continue;
        }
        let Some(gps) = r32(&out, e + 8) else {
            continue;
        };
        let n = r16(&out, gps).unwrap_or(0).min(512);
        let mut spans = Vec::new();
        for j in 0..n {
            let g = gps + 2 + 12 * j;
            let (Some(kind), Some(count)) = (r16(&out, g + 2), r32(&out, g + 4)) else {
                continue;
            };
            let size = size_of(kind).saturating_mul(count);
            if size > 4
                && let Some(at) = r32(&out, g + 8)
                && let Some(end) = at.checked_add(size)
                && end <= out.len()
            {
                spans.push((at, end));
            }
        }
        // The values, then the directory itself (its count becomes 0).
        for (a, b) in spans {
            out[a..b].fill(0);
        }
        let end = (gps + 2 + 12 * n + 4).min(out.len());
        if gps < end {
            out[gps..end].fill(0);
        }
    }
    out
}

/// Human-readable EXIF fields, in a stable order, for the info panel and `chitra info`.
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

#[cfg(test)]
mod gps_tests {
    use super::strip_gps;
    use exif::experimental::Writer;
    use exif::{Field, In, Rational, Tag, Value};

    fn exif_with_location(little: bool) -> Vec<u8> {
        let make = Field {
            tag: Tag::Make,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"TestCam".to_vec()]),
        };
        let lat = Field {
            tag: Tag::GPSLatitude,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![
                Rational { num: 12, denom: 1 },
                Rational { num: 34, denom: 1 },
                Rational {
                    num: 5678,
                    denom: 100,
                },
            ]),
        };
        let lat_ref = Field {
            tag: Tag::GPSLatitudeRef,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"N".to_vec()]),
        };
        let mut w = Writer::new();
        w.push_field(&make);
        w.push_field(&lat);
        w.push_field(&lat_ref);
        let mut buf = std::io::Cursor::new(Vec::new());
        w.write(&mut buf, little).unwrap();
        buf.into_inner()
    }

    #[test]
    fn the_location_goes_and_the_rest_of_the_exif_stays() {
        for little in [true, false] {
            let original = exif_with_location(little);
            let parsed = exif::Reader::new().read_raw(original.clone()).unwrap();
            assert!(parsed.get_field(Tag::GPSLatitude, In::PRIMARY).is_some());
            let needle: [u8; 4] = if little {
                5678u32.to_le_bytes()
            } else {
                5678u32.to_be_bytes()
            };
            assert!(original.windows(4).any(|w| w == needle));

            let clean = strip_gps(&original);
            assert_eq!(clean.len(), original.len());
            let parsed = exif::Reader::new()
                .read_raw(clean.clone())
                .expect("still valid");
            assert!(parsed.get_field(Tag::GPSLatitude, In::PRIMARY).is_none());
            assert!(parsed.get_field(Tag::GPSLatitudeRef, In::PRIMARY).is_none());
            assert!(
                !clean.windows(4).any(|w| w == needle),
                "the value bytes are zeroed"
            );
            let make = parsed.get_field(Tag::Make, In::PRIMARY).unwrap();
            assert_eq!(make.display_value().to_string(), "\"TestCam\"");
        }
        // Not an EXIF block: unchanged.
        assert_eq!(strip_gps(b"junk"), b"junk");
        assert_eq!(strip_gps(b""), b"");
    }
}

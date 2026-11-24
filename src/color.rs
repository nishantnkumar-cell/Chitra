//! Colour management for display.
//!
//! Photos from phones and cameras are often in a wider colour space than sRGB
//! (Display P3, Adobe RGB). The screen is treated as sRGB, so those pixels must
//! be converted, or the colours look dull. The conversion is done on the
//! screen-sized copy, not on the full photo, so it costs a few milliseconds.

use moxcms::{ColorProfile, Layout, TransformOptions};

/// Whether two colorants are the same colour (profiles of the same space vary
/// by rounding).
fn same(a: moxcms::Xyzd, b: moxcms::Xyzd) -> bool {
    (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01 && (a.z - b.z).abs() < 0.01
}

/// Converts RGBA8 pixels (`rgba`) from the colours of the ICC profile `icc` to
/// sRGB, in place. Returns whether anything was converted: not when the
/// profile is unreadable or not an RGB one, or already has sRGB's primaries
/// (so ordinary photos are left byte for byte as they are).
pub fn to_srgb_in_place(rgba: &mut [u8], icc: &[u8]) -> bool {
    let Ok(src) = ColorProfile::new_from_slice(icc) else {
        return false;
    };
    let srgb = ColorProfile::new_srgb();
    if same(src.red_colorant, srgb.red_colorant)
        && same(src.green_colorant, srgb.green_colorant)
        && same(src.blue_colorant, srgb.blue_colorant)
    {
        return false;
    }
    let Ok(transform) = src.create_transform_8bit(
        Layout::Rgba,
        &srgb,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return false;
    };
    let mut out = vec![0u8; rgba.len()];
    if transform.transform(rgba, &mut out).is_err() {
        return false;
    }
    rgba.copy_from_slice(&out);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_p3_pixels_change_and_srgb_ones_do_not() {
        let px = [200u8, 100, 50, 255, 30, 220, 120, 128];
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        let mut a = px;
        assert!(to_srgb_in_place(&mut a, &p3));
        assert_ne!(a[..3], px[..3], "P3 colours are converted");
        assert!(
            (0..3).any(|i| a[i].abs_diff(px[i]) > 3),
            "the change is visible: {a:?} from {px:?}"
        );
        assert_eq!((a[3], a[7]), (255, 128), "alpha is kept");

        let srgb = ColorProfile::new_srgb().encode().unwrap();
        let mut b = px;
        assert!(!to_srgb_in_place(&mut b, &srgb), "already sRGB");
        assert_eq!(b, px);
        assert!(!to_srgb_in_place(&mut b, b"not a profile"));
        assert_eq!(b, px);
    }
}

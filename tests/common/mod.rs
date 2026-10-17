#![allow(dead_code)]

use image::{DynamicImage, Rgb, RgbImage, Rgba, RgbaImage};

/// A photo-like test pattern: smooth gradients plus some hard edges.
pub fn pattern_rgb(w: u32, h: u32) -> DynamicImage {
    let img = RgbImage::from_fn(w, h, |x, y| {
        let r = (x * 255 / w.max(1)) as u8;
        let g = (y * 255 / h.max(1)) as u8;
        let b = if (x / 8 + y / 8) % 2 == 0 { 200 } else { 40 };
        Rgb([r, g, b])
    });
    DynamicImage::ImageRgb8(img)
}

pub fn pattern_rgba(w: u32, h: u32) -> DynamicImage {
    let img = RgbaImage::from_fn(w, h, |x, y| {
        let a = (x * 255 / w.max(1)) as u8;
        Rgba([(y * 255 / h.max(1)) as u8, 128, 255 - a, a])
    });
    DynamicImage::ImageRgba8(img)
}

/// Mean absolute difference per sample, 0..255.
pub fn mean_abs_diff(a: &DynamicImage, b: &DynamicImage) -> f64 {
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    assert_eq!(a.dimensions(), b.dimensions());
    let sum: u64 = a
        .as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64)
        .sum();
    sum as f64 / a.as_raw().len() as f64
}

/// Minimal little-endian TIFF/EXIF block holding only an Orientation tag.
pub fn exif_with_orientation(value: u16) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"II*\0");
    t.extend_from_slice(&8u32.to_le_bytes());
    t.extend_from_slice(&1u16.to_le_bytes());
    t.extend_from_slice(&0x0112u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&value.to_le_bytes());
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

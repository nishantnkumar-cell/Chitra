//! Resizing with `fast_image_resize` (SIMD, multithreaded) and the size maths
//! shared by the viewer and the converter.

use std::str::FromStr;

use fast_image_resize::{FilterType, IntoImageView, ResizeAlg, ResizeOptions, Resizer};
use image::DynamicImage;

/// How the converter should size its output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResizeSpec {
    /// Longest side becomes this many pixels.
    LongestSide(u32),
    /// Scale both sides by this percentage.
    Percent(f32),
    /// Fit inside a box, keeping the aspect ratio.
    Fit(u32, u32),
}

impl FromStr for ResizeSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        let bad = || format!("invalid size '{s}' (use 2048, 50% or 1920x1080)");
        if let Some(p) = s.strip_suffix('%') {
            let p: f32 = p.trim().parse().map_err(|_| bad())?;
            if !(p > 0.0 && p <= 1000.0) {
                return Err(bad());
            }
            return Ok(ResizeSpec::Percent(p));
        }
        if let Some((w, h)) = s.split_once('x') {
            let w: u32 = w.trim().parse().map_err(|_| bad())?;
            let h: u32 = h.trim().parse().map_err(|_| bad())?;
            if w == 0 || h == 0 {
                return Err(bad());
            }
            return Ok(ResizeSpec::Fit(w, h));
        }
        let n: u32 = s.parse().map_err(|_| bad())?;
        if n == 0 {
            return Err(bad());
        }
        Ok(ResizeSpec::LongestSide(n))
    }
}

/// Largest size with the same aspect ratio that fits in `max_w` x `max_h`.
pub fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (w, h);
    }
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    scaled(w, h, scale)
}

fn scaled(w: u32, h: u32, scale: f64) -> (u32, u32) {
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    (nw, nh)
}

/// Output size for a resize spec. Never enlarges unless `upscale` is set.
pub fn target_size(w: u32, h: u32, spec: ResizeSpec, upscale: bool) -> (u32, u32) {
    let (nw, nh) = match spec {
        ResizeSpec::LongestSide(n) => fit_within(w, h, n, n),
        ResizeSpec::Percent(p) => scaled(w, h, p as f64 / 100.0),
        ResizeSpec::Fit(bw, bh) => fit_within(w, h, bw, bh),
    };
    if !upscale && (nw > w || nh > h) {
        (w, h)
    } else {
        (nw, nh)
    }
}

/// High-quality resize (Lanczos3 for shrinking, Catmull-Rom for enlarging).
pub fn resize(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    if image.width() == width && image.height() == height {
        return image.clone();
    }
    // 32-bit float images are rare here; let `image` handle anything the SIMD
    // resizer does not know.
    if image.pixel_type().is_none() {
        return image.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    }
    let filter = if width < image.width() {
        FilterType::Lanczos3
    } else {
        FilterType::CatmullRom
    };
    let mut dst = DynamicImage::new(width, height, image.color());
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter));
    match Resizer::new().resize(image, &mut dst, &opts) {
        Ok(()) => dst,
        Err(_) => image.resize_exact(width, height, image::imageops::FilterType::Lanczos3),
    }
}

/// Fast downscale for previews and thumbnails (bilinear-ish quality is plenty).
pub fn thumbnail(image: &DynamicImage, max_w: u32, max_h: u32) -> DynamicImage {
    let (w, h) = fit_within(image.width(), image.height(), max_w, max_h);
    if w >= image.width() || h >= image.height() {
        return image.clone();
    }
    if image.pixel_type().is_none() {
        return image.thumbnail(w, h);
    }
    let mut dst = DynamicImage::new(w, h, image.color());
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    match Resizer::new().resize(image, &mut dst, &opts) {
        Ok(()) => dst,
        Err(_) => image.thumbnail(w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        assert_eq!(
            "2048".parse::<ResizeSpec>().unwrap(),
            ResizeSpec::LongestSide(2048)
        );
        assert_eq!(
            "50%".parse::<ResizeSpec>().unwrap(),
            ResizeSpec::Percent(50.0)
        );
        assert_eq!(
            "1920x1080".parse::<ResizeSpec>().unwrap(),
            ResizeSpec::Fit(1920, 1080)
        );
        assert!("0".parse::<ResizeSpec>().is_err());
        assert!("x".parse::<ResizeSpec>().is_err());
        assert!("-5%".parse::<ResizeSpec>().is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(
            target_size(6000, 4000, ResizeSpec::LongestSide(3000), false),
            (3000, 2000)
        );
        assert_eq!(
            target_size(4000, 6000, ResizeSpec::LongestSide(3000), false),
            (2000, 3000)
        );
        assert_eq!(
            target_size(100, 50, ResizeSpec::LongestSide(3000), false),
            (100, 50)
        );
        assert_eq!(
            target_size(100, 50, ResizeSpec::LongestSide(200), true),
            (200, 100)
        );
        assert_eq!(
            target_size(1000, 500, ResizeSpec::Percent(50.0), false),
            (500, 250)
        );
        assert_eq!(
            target_size(4000, 3000, ResizeSpec::Fit(1920, 1080), false),
            (1440, 1080)
        );
        assert_eq!(fit_within(1, 10000, 100, 100), (1, 100));
    }

    #[test]
    fn resize_rgba_and_luma16() {
        let img = DynamicImage::new_rgba8(64, 32);
        let out = resize(&img, 16, 8);
        assert_eq!((out.width(), out.height()), (16, 8));
        let img = DynamicImage::new_luma16(10, 10);
        let out = resize(&img, 5, 5);
        assert_eq!(out.color(), image::ColorType::L16);
        let t = thumbnail(&DynamicImage::new_rgb8(400, 200), 100, 100);
        assert_eq!((t.width(), t.height()), (100, 50));
    }
}

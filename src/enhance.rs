//! Upscale and sharpen: optional noise reduction, a Lanczos3 enlargement by a
//! whole factor (1x to 4x), then an unsharp mask on brightness only.
//!
//! Sharpening the brightness (and adding the same change to red, green and
//! blue) keeps colours from fringing, and the noise threshold leaves flat
//! areas alone, so JPEG blocks and grain in old photos are not made worse.

use image::DynamicImage;
use rayon::prelude::*;

use crate::resize::{FilterType, resize_with};

/// Largest output the upscaler will make, in pixels (a 12 MP photo at 4x is 192 MP).
pub const MAX_OUTPUT_PIXELS: u64 = 256_000_000;
/// Largest side the encoders accept (JPEG's limit).
pub const MAX_OUTPUT_SIDE: u32 = 65_535;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnhanceOptions {
    /// Whole-number enlargement, 1 to 4.
    pub scale: u32,
    /// Sharpening strength: 0 = none, 1.0 = 100%.
    pub amount: f32,
    /// Blur radius (Gaussian sigma) of the unsharp mask, in output pixels.
    pub radius: f32,
    /// Brightness differences below this (0-255 scale) are sharpened less, so
    /// noise and JPEG blocks stay quiet.
    pub threshold: f32,
    /// Noise reduction before enlarging, 0 (off) to 100; see `denoise`.
    /// Not used with `ai`, which removes noise itself.
    pub denoise: f32,
    /// Enlarge with the AI model instead of Lanczos3; see `ai`.
    pub ai: bool,
    /// Which AI model.
    pub ai_model: crate::ai::Model,
    /// AI noise removal, 0 (keep noise) to 1 (Fast model only).
    pub ai_noise: f32,
}

impl EnhanceOptions {
    pub fn new(scale: u32) -> Self {
        let scale = scale.clamp(1, 4);
        EnhanceOptions {
            scale,
            amount: 0.8,
            radius: default_radius(scale),
            threshold: 3.0,
            denoise: 0.0,
            ai: false,
            ai_model: crate::ai::Model::Fast,
            ai_noise: 0.5,
        }
    }

    pub fn sharpens(&self) -> bool {
        self.amount > 0.0 && self.radius > 0.0
    }
}

/// A radius that suits the scale: detail made by a 4x enlargement is about
/// four times wider than the original's.
pub fn default_radius(scale: u32) -> f32 {
    0.6 + 0.4 * scale.clamp(1, 4) as f32
}

/// Output size, or an error when it would be too large to make.
pub fn output_size(width: u32, height: u32, scale: u32) -> Result<(u32, u32), String> {
    let (w, h) = (width as u64 * scale as u64, height as u64 * scale as u64);
    if w > MAX_OUTPUT_SIDE as u64 || h > MAX_OUTPUT_SIDE as u64 || w * h > MAX_OUTPUT_PIXELS {
        return Err(format!(
            "{width} x {height} at {scale}x would be {w} x {h}, too large \
             (limit {} megapixels, {MAX_OUTPUT_SIDE} px a side)",
            MAX_OUTPUT_PIXELS / 1_000_000
        ));
    }
    Ok((w as u32, h as u32))
}

/// Reduce noise, upscale, then sharpen.
pub fn enhance(image: &DynamicImage, opts: &EnhanceOptions) -> Result<DynamicImage, String> {
    let (w, h) = output_size(image.width(), image.height(), opts.scale)?;
    if opts.ai {
        // The model enlarges by its own scale; other sizes are resampled.
        output_size(image.width(), image.height(), opts.ai_model.scale())?;
        let big = crate::ai::upscale(image, opts.ai_model, opts.ai_noise)?;
        let mut out = if (big.width(), big.height()) == (w, h) {
            big
        } else {
            resize_with(&big, w, h, FilterType::Lanczos3)
        };
        if opts.sharpens() {
            sharpen(&mut out, opts.amount, opts.radius, opts.threshold);
        }
        return Ok(out);
    }
    let clean;
    let image = if opts.denoise > 0.0 {
        clean = crate::denoise::denoise(image, opts.denoise);
        &clean
    } else {
        image
    };
    let mut out = resize_with(image, w, h, FilterType::Lanczos3);
    if opts.sharpens() {
        sharpen(&mut out, opts.amount, opts.radius, opts.threshold);
    }
    Ok(out)
}

/// Enhance only part of `image`, for a quick preview. `(x, y, w, h)` is in
/// `image` pixels; the result is that area at the output scale, matching the
/// same area of a full `enhance` (the crop is taken with a margin wide enough
/// for both filters, then trimmed).
pub fn enhance_region(
    image: &DynamicImage,
    opts: &EnhanceOptions,
    (x, y, w, h): (u32, u32, u32, u32),
) -> Result<DynamicImage, String> {
    let s = opts.scale.clamp(1, 4);
    let before = if opts.ai {
        opts.ai_model.reach()
    } else {
        crate::denoise::reach(opts.denoise)
    };
    let margin = 4 + (3.0 * opts.radius / s as f32).ceil() as u32 + before;
    let x0 = x.saturating_sub(margin);
    let y0 = y.saturating_sub(margin);
    let x1 = (x + w + margin).min(image.width());
    let y1 = (y + h + margin).min(image.height());
    let crop = image.crop_imm(x0, y0, x1 - x0, y1 - y0);
    let opts = EnhanceOptions { scale: s, ..*opts };
    let big = enhance(&crop, &opts)?;
    Ok(big.crop_imm((x - x0) * s, (y - y0) * s, w * s, h * s))
}

/// Sample types the sharpener works on directly.
trait Sample: Copy + Send + Sync {
    const MAX: f32;
    fn get(self) -> f32;
    fn put(v: f32) -> Self;
}

impl Sample for u8 {
    const MAX: f32 = 255.0;
    fn get(self) -> f32 {
        self as f32
    }
    fn put(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
}

impl Sample for u16 {
    const MAX: f32 = 65535.0;
    fn get(self) -> f32 {
        self as f32
    }
    fn put(v: f32) -> Self {
        v.round().clamp(0.0, 65535.0) as u16
    }
}

/// Unsharp mask on brightness. `amount` 1.0 = 100%, `radius` is the Gaussian
/// sigma in pixels, `threshold` is on a 0-255 scale.
pub fn sharpen(image: &mut DynamicImage, amount: f32, radius: f32, threshold: f32) {
    if amount <= 0.0 || radius <= 0.0 {
        return;
    }
    let (w, h) = (image.width() as usize, image.height() as usize);
    let p = Params {
        amount,
        sigma: radius,
        threshold: threshold.max(0.0),
    };
    match image {
        DynamicImage::ImageLuma8(b) => run::<u8>(b, w, h, 1, 1, p),
        DynamicImage::ImageLumaA8(b) => run::<u8>(b, w, h, 2, 1, p),
        DynamicImage::ImageRgb8(b) => run::<u8>(b, w, h, 3, 3, p),
        DynamicImage::ImageRgba8(b) => run::<u8>(b, w, h, 4, 3, p),
        DynamicImage::ImageLuma16(b) => run::<u16>(b, w, h, 1, 1, p),
        DynamicImage::ImageLumaA16(b) => run::<u16>(b, w, h, 2, 1, p),
        DynamicImage::ImageRgb16(b) => run::<u16>(b, w, h, 3, 3, p),
        DynamicImage::ImageRgba16(b) => run::<u16>(b, w, h, 4, 3, p),
        other => {
            // Float images: work at 16 bits.
            let mut b = other.to_rgba16();
            run::<u16>(&mut b, w, h, 4, 3, p);
            *other = DynamicImage::ImageRgba16(b);
        }
    }
}

#[derive(Clone, Copy)]
struct Params {
    amount: f32,
    sigma: f32,
    threshold: f32,
}

/// Rows processed together; the blur needs `radius` extra rows above and below.
const BAND: usize = 64;

fn luma<T: Sample>(px: &[T], color: usize) -> f32 {
    if color == 3 {
        0.299 * px[0].get() + 0.587 * px[1].get() + 0.114 * px[2].get()
    } else {
        px[0].get()
    }
}

pub(crate) fn kernel(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as usize;
    let mut k: Vec<f32> = (0..=2 * r)
        .map(|i| {
            let d = i as f32 - r as f32;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

fn run<T: Sample>(data: &mut [T], w: usize, h: usize, ch: usize, color: usize, p: Params) {
    if w == 0 || h == 0 {
        return;
    }
    // Brightness of the whole image at 16 bits: the bands below read rows
    // that belong to their neighbours, which are being changed in parallel.
    let to16 = 65535.0 / T::MAX;
    let mut y16 = vec![0u16; w * h];
    y16.par_chunks_mut(w)
        .zip(data.par_chunks(w * ch))
        .for_each(|(dst, src)| {
            for (d, px) in dst.iter_mut().zip(src.chunks_exact(ch)) {
                *d = (luma(px, color) * to16).round().clamp(0.0, 65535.0) as u16;
            }
        });

    let k = kernel(p.sigma);
    let r = k.len() / 2;
    let from16 = T::MAX / 65535.0;
    let t = p.threshold / 255.0 * T::MAX;

    data.par_chunks_mut(BAND * w * ch)
        .enumerate()
        .for_each(|(band, rows)| {
            let y0 = band * BAND;
            let n = rows.len() / (w * ch);
            // Horizontal blur of the rows this band needs, edges clamped.
            let span = n + 2 * r;
            let mut hb = vec![0f32; span * w];
            for (j, out) in hb.chunks_exact_mut(w).enumerate() {
                let sy = (y0 + j).saturating_sub(r).min(h - 1);
                let src = &y16[sy * w..(sy + 1) * w];
                blur_row(src, out, &k);
            }
            // Vertical blur, then add the detail back.
            let mut acc = vec![0f32; w];
            for row in 0..n {
                acc.iter_mut().for_each(|a| *a = 0.0);
                for (i, kv) in k.iter().enumerate() {
                    let line = &hb[(row + i) * w..(row + i + 1) * w];
                    for (a, v) in acc.iter_mut().zip(line) {
                        *a += kv * v;
                    }
                }
                let pixels = &mut rows[row * w * ch..(row + 1) * w * ch];
                for (px, blurred) in pixels.chunks_exact_mut(ch).zip(&acc) {
                    let mut d = luma(px, color) - blurred * from16;
                    if t > 0.0 {
                        let a = d.abs();
                        if a < t {
                            d *= a / t;
                        }
                    }
                    let add = p.amount * d;
                    for c in px.iter_mut().take(color) {
                        *c = T::put(c.get() + add);
                    }
                }
            }
        });
}

fn blur_row(src: &[u16], out: &mut [f32], k: &[f32]) {
    let w = src.len();
    let r = k.len() / 2;
    let at = |i: isize| src[i.clamp(0, w as isize - 1) as usize] as f32;
    for (x, o) in out.iter_mut().enumerate() {
        let mut s = 0.0;
        if x >= r && x + r < w {
            for (kv, v) in k.iter().zip(&src[x - r..=x + r]) {
                s += kv * *v as f32;
            }
        } else {
            for (i, kv) in k.iter().enumerate() {
                s += kv * at(x as isize + i as isize - r as isize);
            }
        }
        *o = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn edge() -> DynamicImage {
        // Left half dark, right half light.
        DynamicImage::ImageRgb8(RgbImage::from_fn(40, 20, |x, _| {
            if x < 20 {
                Rgb([60, 60, 60])
            } else {
                Rgb([180, 180, 180])
            }
        }))
    }

    #[test]
    fn scales_by_whole_factors() {
        for s in 1..=4 {
            let out = enhance(&edge(), &EnhanceOptions::new(s)).unwrap();
            assert_eq!((out.width(), out.height()), (40 * s, 20 * s));
        }
        assert!(output_size(20000, 20000, 4).is_err());
        assert_eq!(output_size(960, 1280, 4).unwrap(), (3840, 5120));
    }

    #[test]
    fn sharpening_raises_edge_contrast() {
        let mut img = edge();
        sharpen(&mut img, 1.0, 1.5, 0.0);
        let rgb = img.to_rgb8();
        // Just either side of the edge: darker than dark, lighter than light.
        assert!(rgb.get_pixel(19, 10)[0] < 60);
        assert!(rgb.get_pixel(20, 10)[0] > 180);
        // Far from the edge nothing changes.
        assert_eq!(rgb.get_pixel(2, 10)[0], 60);
        assert_eq!(rgb.get_pixel(37, 10)[0], 180);
    }

    #[test]
    fn threshold_leaves_small_noise_alone() {
        let noisy = DynamicImage::ImageLuma8(image::GrayImage::from_fn(30, 30, |x, y| {
            image::Luma([if (x + y) % 2 == 0 { 100 } else { 101 }])
        }));
        let mut img = noisy.clone();
        sharpen(&mut img, 2.0, 1.0, 10.0);
        let max_change = img
            .to_luma8()
            .pixels()
            .zip(noisy.to_luma8().pixels())
            .map(|(a, b)| (a[0] as i32 - b[0] as i32).abs())
            .max()
            .unwrap();
        assert!(max_change <= 1, "{max_change}");
    }

    #[test]
    fn alpha_and_16_bit_are_kept() {
        let mut img = DynamicImage::new_rgba16(16, 16);
        sharpen(&mut img, 1.0, 1.0, 0.0);
        assert_eq!(img.color(), image::ColorType::Rgba16);
        let mut img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([10, 20, 30, 77]),
        ));
        sharpen(&mut img, 3.0, 2.0, 0.0);
        assert!(img.to_rgba8().pixels().all(|p| p[3] == 77));
    }

    #[test]
    fn region_matches_full_image() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_fn(64, 48, |x, y| {
            Rgb([
                (x * 7 % 256) as u8,
                (y * 11 % 256) as u8,
                ((x * y) % 256) as u8,
            ])
        }));
        let mut opts = EnhanceOptions::new(3);
        opts.denoise = 70.0;
        let full = enhance(&src, &opts).unwrap().to_rgb8();
        let part = enhance_region(&src, &opts, (20, 10, 16, 12))
            .unwrap()
            .to_rgb8();
        assert_eq!((part.width(), part.height()), (48, 36));
        let mut worst = 0;
        for (x, y, p) in part.enumerate_pixels() {
            let q = full.get_pixel(60 + x, 30 + y);
            for c in 0..3 {
                worst = worst.max((p[c] as i32 - q[c] as i32).abs());
            }
        }
        assert!(
            worst <= 2,
            "preview differs from the saved result by {worst}"
        );
    }
}

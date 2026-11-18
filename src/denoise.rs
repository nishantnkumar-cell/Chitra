//! Noise reduction for old and heavily compressed photos, run before
//! upscaling so the enlargement and the sharpening don't magnify grain.
//!
//! Brightness gets an edge-preserving (bilateral) filter: neighbours are only
//! averaged in when their brightness is close, so edges stay sharp while
//! grain is smoothed. Colour gets a plain blur, which removes the colour
//! blotches JPEG leaves behind; the eye barely notices softer colour detail.

use image::{ColorType, DynamicImage, Rgba32FImage};
use rayon::prelude::*;

/// Bilateral filter reach, in pixels.
const LUMA_RADIUS: usize = 3;
const LUMA_SPATIAL_SIGMA: f32 = 1.6;
/// Range weights are looked up in a table this long.
const LUT: usize = 256;

/// Largest colour blur for `strength` 100, in pixels.
const MAX_CHROMA_SIGMA: f32 = 3.0;

/// How far beyond an area the filter reads, in pixels, so a crop with this
/// margin gives the same result inside as the whole photo.
pub fn reach(strength: f32) -> u32 {
    if strength <= 0.0 {
        return 0;
    }
    let chroma = (3.0 * chroma_sigma(strength)).ceil() as u32;
    chroma.max(LUMA_RADIUS as u32) + 1
}

fn chroma_sigma(strength: f32) -> f32 {
    0.5 + (MAX_CHROMA_SIGMA - 0.5) * (strength / 100.0).clamp(0.0, 1.0)
}

/// Reduce noise. `strength` is 0 (off) to 100.
pub fn denoise(image: &DynamicImage, strength: f32) -> DynamicImage {
    if strength <= 0.0 || image.width() == 0 || image.height() == 0 {
        return image.clone();
    }
    let t = (strength / 100.0).clamp(0.0, 1.0);
    let color = image.color();
    let gray = !color.has_color();
    let mut px = image.to_rgba32f();
    let (w, h) = (px.width() as usize, px.height() as usize);

    // Brightness and two colour-difference planes.
    let n = w * h;
    let mut y = vec![0f32; n];
    let mut cb = vec![0f32; if gray { 0 } else { n }];
    let mut cr = vec![0f32; if gray { 0 } else { n }];
    {
        let raw: &[f32] = px.as_raw();
        y.par_iter_mut().enumerate().for_each(|(i, v)| {
            let p = &raw[i * 4..i * 4 + 3];
            *v = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
        });
        if !gray {
            cb.par_iter_mut()
                .zip(cr.par_iter_mut())
                .enumerate()
                .for_each(|(i, (b, r))| {
                    let p = &raw[i * 4..i * 4 + 3];
                    *b = p[2] - y[i];
                    *r = p[0] - y[i];
                });
        }
    }

    // Grain up to about 20 levels (of 255) at full strength.
    let range_sigma = 0.004 + 0.075 * t;
    let y2 = bilateral(&y, w, h, range_sigma);
    let (cb, cr) = if gray {
        (cb, cr)
    } else {
        let k = crate::enhance::kernel(chroma_sigma(strength));
        (blur(&cb, w, h, &k), blur(&cr, w, h, &k))
    };

    let raw: &mut [f32] = &mut px;
    raw.par_chunks_mut(4).enumerate().for_each(|(i, p)| {
        let l = y2[i];
        if gray {
            p[0] = l.clamp(0.0, 1.0);
            p[1] = p[0];
            p[2] = p[0];
        } else {
            let r = l + cr[i];
            let b = l + cb[i];
            let g = (l - 0.299 * r - 0.114 * b) / 0.587;
            p[0] = r.clamp(0.0, 1.0);
            p[1] = g.clamp(0.0, 1.0);
            p[2] = b.clamp(0.0, 1.0);
        }
    });
    back_to(px, color)
}

fn back_to(px: Rgba32FImage, color: ColorType) -> DynamicImage {
    let img = DynamicImage::ImageRgba32F(px);
    match color {
        ColorType::L8 => DynamicImage::ImageLuma8(img.to_luma8()),
        ColorType::La8 => DynamicImage::ImageLumaA8(img.to_luma_alpha8()),
        ColorType::Rgb8 => DynamicImage::ImageRgb8(img.to_rgb8()),
        ColorType::Rgba8 => DynamicImage::ImageRgba8(img.to_rgba8()),
        ColorType::L16 => DynamicImage::ImageLuma16(img.to_luma16()),
        ColorType::La16 => DynamicImage::ImageLumaA16(img.to_luma_alpha16()),
        ColorType::Rgb16 => DynamicImage::ImageRgb16(img.to_rgb16()),
        ColorType::Rgba16 => DynamicImage::ImageRgba16(img.to_rgba16()),
        ColorType::Rgb32F => DynamicImage::ImageRgb32F(img.to_rgb32f()),
        ColorType::Rgba32F => img,
        _ => DynamicImage::ImageRgba8(img.to_rgba8()),
    }
}

/// Edge-preserving smoothing of one plane (values 0..1).
fn bilateral(src: &[f32], w: usize, h: usize, range_sigma: f32) -> Vec<f32> {
    let r = LUMA_RADIUS as isize;
    let side = 2 * LUMA_RADIUS + 1;
    let spatial: Vec<f32> = (0..side * side)
        .map(|i| {
            let dx = (i % side) as f32 - r as f32;
            let dy = (i / side) as f32 - r as f32;
            (-(dx * dx + dy * dy) / (2.0 * LUMA_SPATIAL_SIGMA * LUMA_SPATIAL_SIGMA)).exp()
        })
        .collect();
    // Differences beyond 3 sigma get no weight.
    let max_d = 3.0 * range_sigma;
    let lut: Vec<f32> = (0..LUT)
        .map(|i| {
            let d = i as f32 / (LUT - 1) as f32 * max_d;
            (-(d * d) / (2.0 * range_sigma * range_sigma)).exp()
        })
        .collect();
    let to_bin = (LUT - 1) as f32 / max_d;

    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let c = src[y * w + x];
            let (mut sum, mut wsum) = (0.0, 0.0);
            for dy in -r..=r {
                let sy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                let line = &src[sy * w..(sy + 1) * w];
                let srow = &spatial[((dy + r) as usize) * side..((dy + r) as usize + 1) * side];
                for (dx, sw) in (-r..=r).zip(srow) {
                    let sx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                    let v = line[sx];
                    let bin = ((v - c).abs() * to_bin) as usize;
                    if bin < LUT {
                        let wt = sw * lut[bin];
                        sum += wt * v;
                        wsum += wt;
                    }
                }
            }
            *o = if wsum > 0.0 { sum / wsum } else { c };
        }
    });
    out
}

/// Separable Gaussian blur of one plane, edges clamped.
fn blur(src: &[f32], w: usize, h: usize, k: &[f32]) -> Vec<f32> {
    let r = (k.len() / 2) as isize;
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let line = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let sx = (x as isize + i as isize - r).clamp(0, w as isize - 1) as usize;
                s += kv * line[sx];
            }
            *o = s;
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (i, kv) in k.iter().enumerate() {
            let sy = (y as isize + i as isize - r).clamp(0, h as isize - 1) as usize;
            for (o, v) in row.iter_mut().zip(&tmp[sy * w..(sy + 1) * w]) {
                *o += kv * v;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, RgbImage};

    /// Deterministic noise, -amp..amp.
    fn noise(x: u32, y: u32, amp: i32) -> i32 {
        let h = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663);
        (h % (2 * amp as u32 + 1)) as i32 - amp
    }

    fn noisy_edge() -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(64, 64, |x, y| {
            let base = if x < 32 { 70 } else { 170 };
            let v = |a: i32| (base + noise(x, y, 10) + a).clamp(0, 255) as u8;
            Rgb([v(0), v(noise(y, x, 6)), v(-3)])
        }))
    }

    fn spread(img: &DynamicImage, x0: u32, x1: u32) -> f64 {
        let g = img.to_luma8();
        let vals: Vec<f64> = (8..56)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .map(|(x, y)| g.get_pixel(x, y)[0] as f64)
            .collect();
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64).sqrt()
    }

    #[test]
    fn smooths_grain_but_keeps_the_edge() {
        let src = noisy_edge();
        let out = denoise(&src, 60.0);
        assert_eq!(out.color(), src.color());
        // Much less grain on both sides.
        assert!(spread(&out, 4, 26) < spread(&src, 4, 26) * 0.5);
        assert!(spread(&out, 38, 60) < spread(&src, 38, 60) * 0.5);
        // The edge stays a sharp step.
        let g = out.to_luma8();
        let (a, b) = (g.get_pixel(29, 32)[0] as i32, g.get_pixel(34, 32)[0] as i32);
        assert!(b - a > 80, "edge {a} -> {b}");
    }

    #[test]
    fn off_and_gray_and_alpha() {
        let src = noisy_edge();
        assert_eq!(denoise(&src, 0.0).to_rgb8(), src.to_rgb8());
        let gray = DynamicImage::ImageLuma8(GrayImage::from_fn(20, 20, |x, y| {
            Luma([(120 + noise(x, y, 8)) as u8])
        }));
        assert_eq!(denoise(&gray, 50.0).color(), ColorType::L8);
        let rgba = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            10,
            10,
            image::Rgba([10, 20, 30, 77]),
        ));
        assert!(denoise(&rgba, 80.0).to_rgba8().pixels().all(|p| p[3] == 77));
    }
}

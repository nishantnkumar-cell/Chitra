//! Every format round-trips through our own encoder and decoder, and broken
//! files fail cleanly instead of crashing.

mod common;

use std::time::Duration;

use chitra::decode::{DecodeOptions, Frame, decode_bytes};
use chitra::encode::{EncodeOptions, encode, is_lossy};
use chitra::format::Format;
use common::*;
use image::DynamicImage;

fn encode_to_vec(img: &DynamicImage, opts: &EncodeOptions) -> Vec<u8> {
    let mut out = Vec::new();
    encode(img, &[], opts, &mut out)
        .unwrap_or_else(|e| panic!("{:?} encode failed: {e}", opts.format));
    out
}

#[test]
fn every_format_round_trips() {
    let src = pattern_rgb(97, 61); // odd sizes catch stride bugs
    for format in Format::WRITABLE {
        let opts = EncodeOptions::new(format);
        let bytes = encode_to_vec(&src, &opts);
        assert_eq!(Format::from_magic(&bytes), Some(format), "{format:?} magic");
        let decoded = decode_bytes(&bytes, format, DecodeOptions::default()).unwrap();
        assert_eq!(
            (decoded.image.width(), decoded.image.height()),
            (97, 61),
            "{format:?} size"
        );
        let diff = mean_abs_diff(&src, &decoded.image);
        let limit = match format {
            Format::Gif => 12.0, // 256-colour palette
            f if is_lossy(f, false) => 6.0,
            _ => 0.0,
        };
        assert!(diff <= limit, "{format:?}: mean diff {diff} > {limit}");
    }
}

#[test]
fn alpha_is_kept_where_supported() {
    let src = pattern_rgba(40, 30);
    for format in [Format::Png, Format::WebP, Format::JpegXl] {
        let mut opts = EncodeOptions::new(format);
        opts.lossless = true;
        let bytes = encode_to_vec(&src, &opts);
        let decoded = decode_bytes(&bytes, format, DecodeOptions::default()).unwrap();
        assert!(decoded.image.color().has_alpha(), "{format:?} lost alpha");
        assert_eq!(
            mean_abs_diff(&src, &decoded.image),
            0.0,
            "{format:?} not lossless"
        );
    }
    // JPEG has no alpha: transparent pixels are blended onto white, not black.
    let clear = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        8,
        8,
        image::Rgba([0, 0, 0, 0]),
    ));
    let bytes = encode_to_vec(&clear, &EncodeOptions::new(Format::Jpeg));
    let decoded = decode_bytes(&bytes, Format::Jpeg, DecodeOptions::default())
        .unwrap()
        .image
        .to_rgb8();
    assert!(decoded.pixels().all(|p| p.0.iter().all(|c| *c > 245)));
}

#[test]
fn sixteen_bit_survives_png_and_jxl() {
    let img = image::ImageBuffer::from_fn(33, 17, |x, y| {
        image::Rgb([x as u16 * 1000, y as u16 * 3000, 65535u16])
    });
    let src = DynamicImage::ImageRgb16(img);
    for format in [Format::Png, Format::JpegXl] {
        let mut opts = EncodeOptions::new(format);
        opts.lossless = true;
        let bytes = encode_to_vec(&src, &opts);
        let decoded = decode_bytes(&bytes, format, DecodeOptions::default()).unwrap();
        assert_eq!(decoded.image.color(), image::ColorType::Rgb16, "{format:?}");
        assert_eq!(decoded.image.as_bytes(), src.as_bytes(), "{format:?}");
    }
}

#[test]
fn grayscale_round_trips() {
    let src = DynamicImage::ImageLuma8(image::GrayImage::from_fn(20, 10, |x, y| {
        image::Luma([(x * 10 + y) as u8])
    }));
    for format in Format::WRITABLE {
        let mut opts = EncodeOptions::new(format);
        opts.lossless = true;
        let bytes = encode_to_vec(&src, &opts);
        let decoded = decode_bytes(&bytes, format, DecodeOptions::default()).unwrap();
        assert_eq!(
            (decoded.image.width(), decoded.image.height()),
            (20, 10),
            "{format:?}"
        );
    }
}

#[test]
fn animated_gif_keeps_frames_and_timing() {
    let frames: Vec<Frame> = (0..3)
        .map(|i| Frame {
            image: image::RgbaImage::from_pixel(16, 16, image::Rgba([i * 100, 0, 0, 255])),
            delay: Duration::from_millis(50 + i as u64 * 50),
        })
        .collect();
    let first = DynamicImage::ImageRgba8(frames[0].image.clone());
    let mut bytes = Vec::new();
    encode(
        &first,
        &frames,
        &EncodeOptions::new(Format::Gif),
        &mut bytes,
    )
    .unwrap();

    let anim = decode_bytes(&bytes, Format::Gif, DecodeOptions { animation: true }).unwrap();
    assert!(anim.is_animated());
    assert_eq!(anim.frames.len(), 3);
    let delays: Vec<u128> = anim.frames.iter().map(|f| f.delay.as_millis()).collect();
    assert_eq!(delays, [50, 100, 150]);
    assert_eq!(anim.frames[2].image.get_pixel(0, 0).0[0], 200);

    let still = decode_bytes(&bytes, Format::Gif, DecodeOptions { animation: false }).unwrap();
    assert!(!still.is_animated());
}

#[test]
fn exif_orientation_is_applied() {
    let src = pattern_rgb(40, 20);
    let mut opts = EncodeOptions::new(Format::Jpeg);
    opts.exif = Some(exif_with_orientation(6)); // rotate 90° clockwise
    let bytes = encode_to_vec(&src, &opts);
    let decoded = decode_bytes(&bytes, Format::Jpeg, DecodeOptions::default()).unwrap();
    assert_eq!((decoded.image.width(), decoded.image.height()), (20, 40));
    assert!(decoded.exif.is_some());
}

#[test]
fn broken_files_fail_without_panicking() {
    let src = pattern_rgba(64, 48);
    for format in Format::WRITABLE {
        let mut opts = EncodeOptions::new(format);
        opts.lossless = true;
        let good = encode_to_vec(&src, &opts);
        // Truncations at many points, plus flipped bytes, must never panic.
        let step = (good.len() / 40).max(1);
        for cut in (0..good.len()).step_by(step) {
            let _ = decode_bytes(&good[..cut], format, DecodeOptions { animation: true });
        }
        let mut seed = 0x2545F491u32;
        for _ in 0..60 {
            let mut bad = good.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = (seed as usize) % bad.len();
                bad[i] ^= (seed >> 24) as u8 | 1;
            }
            let _ = decode_bytes(&bad, format, DecodeOptions { animation: true });
        }
        assert!(
            decode_bytes(b"definitely not an image", format, DecodeOptions::default()).is_err()
        );
    }
}

#[test]
fn heif_decodes() {
    let bytes = std::fs::read("tests/data/pattern.heic").unwrap();
    assert_eq!(Format::from_magic(&bytes), Some(Format::Heif));
    let decoded = decode_bytes(&bytes, Format::Heif, DecodeOptions::default()).unwrap();
    let diff = mean_abs_diff(&pattern_rgb(97, 61), &decoded.image);
    assert!(diff <= 6.0, "HEIF: mean diff {diff}");

    let bytes = std::fs::read("tests/data/alpha.heic").unwrap();
    let decoded = decode_bytes(&bytes, Format::Heif, DecodeOptions::default()).unwrap();
    assert!(decoded.image.color().has_alpha(), "HEIF lost alpha");
    assert!(mean_abs_diff(&pattern_rgba(40, 30), &decoded.image) <= 6.0);

    // Read only: asking for HEIF output is an error, not a crash.
    let mut out = Vec::new();
    assert!(
        encode(
            &decoded.image,
            &[],
            &EncodeOptions::new(Format::Heif),
            &mut out
        )
        .is_err()
    );
}

#[test]
fn avif_from_libavif_and_with_alpha() {
    // Made by libavif, with an odd width (needs the vendored oxideav-avif fix).
    let bytes = std::fs::read("tests/data/pattern.avif").unwrap();
    assert_eq!(Format::from_magic(&bytes), Some(Format::Avif));
    let decoded = decode_bytes(&bytes, Format::Avif, DecodeOptions::default()).unwrap();
    let diff = mean_abs_diff(&pattern_rgb(97, 61), &decoded.image);
    assert!(diff <= 6.0, "AVIF: mean diff {diff}");

    let src = pattern_rgba(40, 30);
    let bytes = encode_to_vec(&src, &EncodeOptions::new(Format::Avif));
    let decoded = decode_bytes(&bytes, Format::Avif, DecodeOptions::default()).unwrap();
    assert!(decoded.image.color().has_alpha(), "AVIF lost alpha");
    assert!(mean_abs_diff(&src, &decoded.image) <= 6.0);
}

#[test]
fn broken_heif_fails_without_panicking() {
    let good = std::fs::read("tests/data/pattern.heic").unwrap();
    let step = (good.len() / 60).max(1);
    for cut in (0..good.len()).step_by(step) {
        let _ = decode_bytes(&good[..cut], Format::Heif, DecodeOptions::default());
    }
    let mut seed = 0x2545F491u32;
    for _ in 0..200 {
        let mut bad = good.clone();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let i = (seed as usize) % bad.len();
            bad[i] ^= (seed >> 24) as u8 | 1;
        }
        let _ = decode_bytes(&bad, Format::Heif, DecodeOptions::default());
    }
}

#[test]
fn detects_format_from_content_not_name() {
    let bytes = encode_to_vec(&pattern_rgb(8, 8), &EncodeOptions::new(Format::Png));
    assert_eq!(
        chitra::decode::detect_format(&bytes, Some(std::path::Path::new("x.jpg"))),
        Some(Format::Png)
    );
}

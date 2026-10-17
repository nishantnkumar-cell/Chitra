//! Encoding pixels to files. All encoders are pure Rust:
//! JPEG, PNG, GIF, BMP via `image`; lossless WebP via `image-webp`; lossless
//! JPEG XL via `zune-jpegxl`. With the `lossy-encoders` feature, lossy WebP
//! (`zenwebp`) and lossy JPEG XL (`jxl-encoder`) are added.

use std::fmt;
use std::io::Write;

use image::codecs::avif::AvifEncoder;
use image::codecs::bmp::BmpEncoder;
use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ColorType, Delay, DynamicImage, ImageEncoder, Rgba, RgbaImage};

use crate::decode::Frame;
use crate::format::Format;

#[derive(Clone, Debug)]
pub struct EncodeOptions {
    pub format: Format,
    /// 1 to 100. Used by JPEG, lossy WebP and lossy JPEG XL; for GIF it
    /// trades palette quality for speed.
    pub quality: u8,
    /// Force lossless WebP / JPEG XL even when lossy encoders are built in.
    pub lossless: bool,
    /// EXIF block (TIFF form, orientation already reset) to embed where supported.
    pub exif: Option<Vec<u8>>,
    /// ICC profile to embed where supported.
    pub icc: Option<Vec<u8>>,
}

impl EncodeOptions {
    pub fn new(format: Format) -> Self {
        EncodeOptions {
            format,
            quality: 90,
            lossless: false,
            exif: None,
            icc: None,
        }
    }
}

#[derive(Debug)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot encode image: {}", self.0)
    }
}

impl std::error::Error for EncodeError {}

impl From<image::ImageError> for EncodeError {
    fn from(e: image::ImageError) -> Self {
        EncodeError(e.to_string())
    }
}

impl From<std::io::Error> for EncodeError {
    fn from(e: std::io::Error) -> Self {
        EncodeError(e.to_string())
    }
}

/// True when this build can write lossy WebP and JPEG XL.
pub const LOSSY_ENCODERS: bool = cfg!(feature = "lossy-encoders");

/// Whether the encoder for `format` can store EXIF.
pub fn supports_exif(format: Format) -> bool {
    matches!(format, Format::Jpeg | Format::Png | Format::WebP)
        || (LOSSY_ENCODERS && format == Format::JpegXl)
}

/// Whether `format` will be written lossy (and so uses the quality setting).
pub fn is_lossy(format: Format, lossless: bool) -> bool {
    match format {
        Format::Jpeg | Format::Avif => true,
        Format::WebP | Format::JpegXl => LOSSY_ENCODERS && !lossless,
        _ => false,
    }
}

/// Encode `image` (or `frames`, when animated and the format can animate) into `out`.
pub fn encode<W: Write>(
    image: &DynamicImage,
    frames: &[Frame],
    opts: &EncodeOptions,
    out: W,
) -> Result<(), EncodeError> {
    crate::isolate(|| encode_unchecked(image, frames, opts, out)).map_err(EncodeError)?
}

fn encode_unchecked<W: Write>(
    image: &DynamicImage,
    frames: &[Frame],
    opts: &EncodeOptions,
    mut out: W,
) -> Result<(), EncodeError> {
    let quality = opts.quality.clamp(1, 100);
    match opts.format {
        Format::Heif => {
            return Err(EncodeError(
                "HEIF can be read but not written; choose another format".into(),
            ));
        }
        Format::Jpeg => {
            let img = without_alpha_8bit(image);
            let mut enc = JpegEncoder::new_with_quality(&mut out, quality);
            add_metadata(&mut enc, opts);
            enc.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        Format::Png => {
            let img = png_compatible(image);
            let mut enc = PngEncoder::new_with_quality(
                &mut out,
                CompressionType::Default,
                FilterType::Adaptive,
            );
            add_metadata(&mut enc, opts);
            enc.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        Format::Avif => {
            // Speed 10 of 1-10: rav1e is slow, and on a 12 MP photo speed 10 is
            // 3.4x faster than 6 for a 3% larger file at similar quality.
            let img = eight_bit(image);
            let enc = AvifEncoder::new_with_speed_quality(&mut out, 10, quality);
            enc.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        Format::Bmp => {
            let img = eight_bit(image);
            let mut enc = BmpEncoder::new(&mut out);
            enc.encode(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        Format::WebP => {
            let img = eight_bit(image);
            #[cfg(feature = "lossy-encoders")]
            if !opts.lossless {
                lossy::webp(&img, opts, quality, &mut out)?;
                out.flush()?;
                return Ok(());
            }
            let mut enc = WebPEncoder::new_lossless(&mut out);
            add_metadata(&mut enc, opts);
            enc.write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )?;
        }
        Format::Gif => {
            // Speed 1 is best quality, 30 fastest; quality 100 maps to 1.
            let speed = (31 - (quality as i32 * 30 / 100)).clamp(1, 30);
            let mut enc = GifEncoder::new_with_speed(&mut out, speed);
            if frames.len() > 1 {
                enc.set_repeat(Repeat::Infinite)?;
                let frames = frames.iter().map(|f| {
                    let ms = f.delay.as_millis().min(u32::MAX as u128) as u32;
                    image::Frame::from_parts(
                        f.image.clone(),
                        0,
                        0,
                        Delay::from_numer_denom_ms(ms, 1),
                    )
                });
                enc.encode_frames(frames)?;
            } else {
                let rgba = image.to_rgba8();
                enc.encode_frame(image::Frame::new(rgba))?;
            }
        }
        Format::JpegXl => {
            #[cfg(feature = "lossy-encoders")]
            if !opts.lossless {
                lossy::jxl(image, opts, quality, &mut out)?;
                out.flush()?;
                return Ok(());
            }
            encode_jxl(image, &mut out)?
        }
    }
    out.flush()?;
    Ok(())
}

fn add_metadata<E: ImageEncoder>(enc: &mut E, opts: &EncodeOptions) {
    if let Some(icc) = &opts.icc {
        let _ = enc.set_icc_profile(icc.clone());
    }
    if let Some(exif) = &opts.exif {
        let _ = enc.set_exif_metadata(exif.clone());
    }
}

fn has_alpha(image: &DynamicImage) -> bool {
    image.color().has_alpha()
}

/// 8-bit, keeping alpha and grey where present.
fn eight_bit(image: &DynamicImage) -> DynamicImage {
    match image.color() {
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8 => image.clone(),
        ColorType::L16 => image.to_luma8().into(),
        ColorType::La16 => image.to_luma_alpha8().into(),
        _ if has_alpha(image) => image.to_rgba8().into(),
        _ => image.to_rgb8().into(),
    }
}

/// PNG keeps 8- and 16-bit data; float images become 16-bit.
fn png_compatible(image: &DynamicImage) -> DynamicImage {
    match image.color() {
        ColorType::Rgb32F => image.to_rgb16().into(),
        ColorType::Rgba32F => image.to_rgba16().into(),
        _ => image.clone(),
    }
}

/// JPEG has no alpha: blend onto white so transparent areas don't turn black.
fn without_alpha_8bit(image: &DynamicImage) -> DynamicImage {
    if !has_alpha(image) {
        return match image.color() {
            ColorType::L8 | ColorType::Rgb8 => image.clone(),
            ColorType::L16 => image.to_luma8().into(),
            _ => image.to_rgb8().into(),
        };
    }
    let rgba: RgbaImage = image.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (dst, Rgba([r, g, b, a])) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = *a as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        *dst = image::Rgb([blend(*r), blend(*g), blend(*b)]);
    }
    rgb.into()
}

fn encode_jxl<W: Write>(image: &DynamicImage, out: &mut W) -> Result<(), EncodeError> {
    use zune_core::bit_depth::BitDepth;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::EncoderOptions;
    use zune_jpegxl::JxlSimpleEncoder;

    let (w, h) = (image.width() as usize, image.height() as usize);
    let (data, colorspace, depth): (Vec<u8>, ColorSpace, BitDepth) = match image.color() {
        ColorType::L8 => (image.as_bytes().to_vec(), ColorSpace::Luma, BitDepth::Eight),
        ColorType::La8 => (
            image.as_bytes().to_vec(),
            ColorSpace::LumaA,
            BitDepth::Eight,
        ),
        ColorType::Rgb8 => (image.as_bytes().to_vec(), ColorSpace::RGB, BitDepth::Eight),
        ColorType::Rgba8 => (image.as_bytes().to_vec(), ColorSpace::RGBA, BitDepth::Eight),
        ColorType::L16 => (
            ne_bytes(image.to_luma16().as_raw()),
            ColorSpace::Luma,
            BitDepth::Sixteen,
        ),
        ColorType::La16 => (
            ne_bytes(image.to_luma_alpha16().as_raw()),
            ColorSpace::LumaA,
            BitDepth::Sixteen,
        ),
        c if c.has_alpha() => (
            ne_bytes(image.to_rgba16().as_raw()),
            ColorSpace::RGBA,
            BitDepth::Sixteen,
        ),
        _ => (
            ne_bytes(image.to_rgb16().as_raw()),
            ColorSpace::RGB,
            BitDepth::Sixteen,
        ),
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(255) as u8);
    let options = EncoderOptions::new(w, h, colorspace, depth).set_num_threads(threads);
    let mut buf = Vec::new();
    JxlSimpleEncoder::new(&data, options)
        .encode(&mut buf)
        .map_err(|e| EncodeError(format!("{e:?}")))?;
    out.write_all(&buf)?;
    Ok(())
}

fn ne_bytes(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

#[cfg(feature = "lossy-encoders")]
mod lossy {
    use std::io::Write;

    use image::{ColorType, DynamicImage};

    use super::{EncodeError, EncodeOptions};

    pub fn webp<W: Write>(
        img: &DynamicImage,
        opts: &EncodeOptions,
        quality: u8,
        out: &mut W,
    ) -> Result<(), EncodeError> {
        use zenwebp::{EncodeRequest, LossyConfig, PixelLayout};

        let layout = match img.color() {
            ColorType::L8 => PixelLayout::L8,
            ColorType::La8 => PixelLayout::La8,
            ColorType::Rgb8 => PixelLayout::Rgb8,
            _ => PixelLayout::Rgba8,
        };
        let rgba;
        let pixels = if matches!(
            img.color(),
            ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
        ) {
            img.as_bytes()
        } else {
            rgba = img.to_rgba8();
            rgba.as_raw()
        };
        let config = LossyConfig::new()
            .with_quality(quality as f32)
            .with_method(4);
        let mut req = EncodeRequest::lossy(&config, pixels, layout, img.width(), img.height());
        if let Some(icc) = &opts.icc {
            req = req.with_icc_profile(icc);
        }
        if let Some(exif) = &opts.exif {
            req = req.with_exif(exif);
        }
        let bytes = req.encode().map_err(|e| EncodeError(e.to_string()))?;
        out.write_all(&bytes)?;
        Ok(())
    }

    /// Map a JPEG-style quality to a JPEG XL Butteraugli distance (libjxl's mapping).
    pub fn jxl_distance(quality: u8) -> f32 {
        let q = quality.clamp(1, 100) as f32;
        if q >= 30.0 {
            0.1 + (100.0 - q) * 0.09
        } else {
            6.4 + (2.5f32).powf((30.0 - q) / 5.0) / 6.25
        }
    }

    pub fn jxl<W: Write>(
        img: &DynamicImage,
        opts: &EncodeOptions,
        quality: u8,
        out: &mut W,
    ) -> Result<(), EncodeError> {
        use jxl_encoder::{ImageMetadata, LossyConfig, PixelLayout};

        let (layout, converted): (PixelLayout, DynamicImage) = match img.color() {
            ColorType::L8 => (PixelLayout::Gray8, img.clone()),
            ColorType::La8 => (PixelLayout::GrayAlpha8, img.clone()),
            ColorType::Rgb8 => (PixelLayout::Rgb8, img.clone()),
            ColorType::Rgba8 => (PixelLayout::Rgba8, img.clone()),
            ColorType::L16 => (PixelLayout::Gray16, img.clone()),
            ColorType::Rgb16 => (PixelLayout::Rgb16, img.clone()),
            ColorType::Rgba16 => (PixelLayout::Rgba16, img.clone()),
            c if c.has_alpha() => (PixelLayout::Rgba16, img.to_rgba16().into()),
            _ => (PixelLayout::Rgb16, img.to_rgb16().into()),
        };
        let config = LossyConfig::new(jxl_distance(quality)).with_effort(3);
        let mut meta = ImageMetadata::new();
        if let Some(icc) = &opts.icc {
            meta = meta.with_icc_profile(icc);
        }
        if let Some(exif) = &opts.exif {
            meta = meta.with_exif(exif);
        }
        let bytes = config
            .encode_request(converted.width(), converted.height(), layout)
            .with_metadata(&meta)
            .encode(converted.as_bytes())
            .map_err(|e| EncodeError(format!("{e:?}")))?;
        out.write_all(&bytes)?;
        Ok(())
    }
}

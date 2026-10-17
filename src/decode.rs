//! Decoding files into pixels. Everything here is pure Rust: `image` (with
//! `zune-jpeg` for JPEG) for the classic formats, Google's `jxl-rs` for JPEG XL
//! `heifer` for HEIF/HEIC and `oxideav-avif` for AVIF.

use std::fmt;
use std::io::{self, Cursor};
use std::path::Path;
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{
    AnimationDecoder, DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Limits, RgbaImage,
};

use crate::format::Format;
use crate::metadata;

/// Largest single allocation a decoder may make (2 GiB, enough for ~500 MP RGBA).
pub(crate) const MAX_ALLOC: u64 = 2 << 30;
/// Animated files with more frames than this are shown as a still image.
const MAX_FRAMES: usize = 2000;

/// One frame of an animation, already composited onto the full canvas.
#[derive(Clone)]
pub struct Frame {
    pub image: RgbaImage,
    pub delay: Duration,
}

/// A decoded image plus the metadata Chitra cares about.
pub struct Decoded {
    pub format: Format,
    /// The still image (first frame for animations), with EXIF orientation applied.
    pub image: DynamicImage,
    /// All frames when the file is animated and animation was requested; otherwise empty.
    pub frames: Vec<Frame>,
    /// EXIF block in TIFF form, if the file has one.
    pub exif: Option<Vec<u8>>,
    /// Embedded ICC colour profile, if any.
    pub icc: Option<Vec<u8>>,
}

impl Decoded {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeOptions {
    /// Decode every frame of animated GIF, WebP, APNG and JPEG XL files.
    pub animation: bool,
}

#[derive(Debug)]
pub enum DecodeError {
    Io(io::Error),
    UnknownFormat,
    Invalid(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "cannot read file: {e}"),
            DecodeError::UnknownFormat => f.write_str("not a supported image format"),
            DecodeError::Invalid(e) => write!(f, "cannot decode image: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        DecodeError::Io(e)
    }
}

impl From<image::ImageError> for DecodeError {
    fn from(e: image::ImageError) -> Self {
        DecodeError::Invalid(e.to_string())
    }
}

/// Work out the format of some bytes, trusting the content over the file name.
pub fn detect_format(data: &[u8], path: Option<&Path>) -> Option<Format> {
    Format::from_magic(data).or_else(|| path.and_then(Format::from_path))
}

pub fn decode_file(path: &Path, opts: DecodeOptions) -> Result<Decoded, DecodeError> {
    let data = std::fs::read(path)?;
    let format = detect_format(&data, Some(path)).ok_or(DecodeError::UnknownFormat)?;
    decode_bytes(&data, format, opts)
}

pub fn decode_bytes(
    data: &[u8],
    format: Format,
    opts: DecodeOptions,
) -> Result<Decoded, DecodeError> {
    crate::isolate(|| decode_unchecked(data, format, opts)).map_err(DecodeError::Invalid)?
}

fn decode_unchecked(
    data: &[u8],
    format: Format,
    opts: DecodeOptions,
) -> Result<Decoded, DecodeError> {
    match format {
        Format::JpegXl => decode_jxl(data, opts),
        Format::Heif => decode_heif(data),
        Format::Avif => decode_avif(data),
        _ => decode_with_image(data, format, opts),
    }
}

fn image_format(format: Format) -> image::ImageFormat {
    match format {
        Format::Jpeg => image::ImageFormat::Jpeg,
        Format::Png => image::ImageFormat::Png,
        Format::Gif => image::ImageFormat::Gif,
        Format::Bmp => image::ImageFormat::Bmp,
        Format::WebP => image::ImageFormat::WebP,
        Format::JpegXl => unreachable!("JPEG XL is decoded by jxl-rs"),
        Format::Heif => unreachable!("HEIF is decoded by heifer"),
        Format::Avif => unreachable!("AVIF is decoded by oxideav-avif"),
    }
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    limits
}

fn decode_with_image(
    data: &[u8],
    format: Format,
    opts: DecodeOptions,
) -> Result<Decoded, DecodeError> {
    let frames = if opts.animation {
        decode_animation(data, format)?
    } else {
        Vec::new()
    };

    let mut reader = ImageReader::with_format(Cursor::new(data), image_format(format));
    reader.limits(limits());
    let mut decoder = reader.into_decoder()?;
    let exif = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .and_then(metadata::normalize_exif);
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = match &exif {
        Some(exif) => metadata::orientation(exif),
        None => decoder.orientation().unwrap_or(Orientation::NoTransforms),
    };

    let image = if frames.len() > 1 {
        DynamicImage::ImageRgba8(frames[0].image.clone())
    } else {
        let mut image = DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        image
    };

    Ok(Decoded {
        format,
        image,
        frames: if frames.len() > 1 { frames } else { Vec::new() },
        exif,
        icc,
    })
}

/// HEIF/HEIC via `heifer` (pure Rust, no unsafe). The file's own crop,
/// rotation and mirror boxes are already applied, so the EXIF orientation tag
/// is ignored, as Apple and libheif do.
fn decode_heif(data: &[u8]) -> Result<Decoded, DecodeError> {
    let options = heifer::Options {
        max_threads: 0,
        // Same ceiling as the other decoders: MAX_ALLOC bytes of 8-bit RGBA.
        max_pixels: MAX_ALLOC / 4,
    };
    let heif = heifer::decode_with_options(data, &options)
        .map_err(|e| DecodeError::Invalid(e.to_string()))?;
    let (w, h) = (heif.width, heif.height);
    let invalid = || DecodeError::Invalid("decoded HEIF image has the wrong size".into());
    let image = if heif.bit_depth <= 8 {
        if heif.has_alpha {
            DynamicImage::ImageRgba8(
                ImageBuffer::from_raw(w, h, heif.to_rgba8()).ok_or_else(invalid)?,
            )
        } else {
            DynamicImage::ImageRgb8(
                ImageBuffer::from_raw(w, h, heif.to_rgb8()).ok_or_else(invalid)?,
            )
        }
    } else {
        // 10- and 12-bit photos keep their precision as 16-bit.
        let max = u32::from(heif.max_value()).max(1);
        let scale = |v: u16| (u32::from(v) * 65535 / max) as u16;
        if heif.has_alpha {
            let px = heif.data.iter().map(|&v| scale(v)).collect();
            DynamicImage::ImageRgba16(ImageBuffer::from_raw(w, h, px).ok_or_else(invalid)?)
        } else {
            let px = heif
                .data
                .chunks_exact(4)
                .flat_map(|p| [scale(p[0]), scale(p[1]), scale(p[2])])
                .collect();
            DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, px).ok_or_else(invalid)?)
        }
    };
    let meta = heifer::read_metadata(data).ok();
    let (exif, icc) = match meta {
        Some(m) => (m.exif.and_then(metadata::normalize_exif), m.icc_profile),
        None => (None, None),
    };
    Ok(Decoded {
        format: Format::Heif,
        image,
        frames: Vec::new(),
        exif,
        icc,
    })
}

/// AVIF via `oxideav-avif` (pure Rust, no unsafe, with its own AV1 decoder).
/// Alpha, grids and the crop, rotation and mirror boxes are applied by the
/// crate; higher bit depths are reduced to 8 bits.
fn decode_avif(data: &[u8]) -> Result<Decoded, DecodeError> {
    let avif = oxideav_avif::decode(data).map_err(|e| DecodeError::Invalid(e.to_string()))?;
    let (w, h) = (avif.width(), avif.height());
    if u64::from(w) * u64::from(h) * 4 > MAX_ALLOC {
        return Err(DecodeError::Invalid("image too large".into()));
    }
    let invalid = || DecodeError::Invalid("decoded AVIF image has the wrong size".into());
    let rgba = ImageBuffer::from_raw(w, h, avif.to_rgba8()).ok_or_else(invalid)?;
    let image = if avif.has_alpha() {
        DynamicImage::ImageRgba8(rgba)
    } else {
        DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
    };
    Ok(Decoded {
        format: Format::Avif,
        image,
        frames: Vec::new(),
        exif: None,
        icc: None,
    })
}

fn decode_animation(data: &[u8], format: Format) -> Result<Vec<Frame>, DecodeError> {
    let frames = match format {
        Format::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(data))?;
            decoder.set_limits(limits())?;
            collect_frames(decoder.into_frames())
        }
        Format::WebP => {
            let decoder = WebPDecoder::new(Cursor::new(data))?;
            if !decoder.has_animation() {
                return Ok(Vec::new());
            }
            collect_frames(decoder.into_frames())
        }
        Format::Png => {
            let decoder = PngDecoder::new(Cursor::new(data))?;
            if !decoder.is_apng()? {
                return Ok(Vec::new());
            }
            collect_frames(decoder.apng()?.into_frames())
        }
        _ => return Ok(Vec::new()),
    };
    // A broken animation still shows its first frame via the still-image path.
    Ok(frames.unwrap_or_default())
}

fn collect_frames(frames: image::Frames<'_>) -> Result<Vec<Frame>, DecodeError> {
    let mut out = Vec::new();
    for frame in frames.take(MAX_FRAMES + 1) {
        let frame = frame?;
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = num.checked_div(den).unwrap_or(0);
        out.push(Frame {
            delay: normalize_delay(ms),
            image: frame.into_buffer(),
        });
    }
    if out.len() > MAX_FRAMES {
        out.truncate(1);
    }
    Ok(out)
}

/// Browsers treat very short delays as 100 ms; do the same so old GIFs play at
/// their intended speed.
fn normalize_delay(ms: u32) -> Duration {
    Duration::from_millis(if ms <= 10 { 100 } else { ms as u64 })
}

/// Lets jxl-rs spread its work over the rayon thread pool; single-threaded it
/// is about 4x slower on large photos.
struct RayonRunner;

impl jxl::api::JxlParallelRunner for RayonRunner {
    fn run(
        &mut self,
        num: usize,
        fun: &jxl::api::JxlParallelRunnerFun<'_>,
    ) -> jxl::error::Result<()> {
        use rayon::prelude::*;
        (0..num).into_par_iter().try_for_each(fun)
    }

    fn num_threads(&self) -> usize {
        rayon::current_num_threads()
    }
}

fn jxl_err(e: jxl::error::Error) -> DecodeError {
    DecodeError::Invalid(e.to_string())
}

fn truncated() -> DecodeError {
    DecodeError::Invalid("truncated JPEG XL file".into())
}

fn decode_jxl(data: &[u8], opts: DecodeOptions) -> Result<Decoded, DecodeError> {
    use jxl::api::{
        Endianness, JxlAuxBoxType, JxlBitDepth, JxlColorType, JxlDataFormat, JxlDecoder,
        JxlDecoderOptions, JxlPixelFormat, ProcessingResult, states,
    };

    let mut input: &[u8] = data;
    let mut runner = RayonRunner;
    let mut options = JxlDecoderOptions::default();
    options.request_aux_boxes = vec![JxlAuxBoxType::EXIF];
    // Refuse images over ~1 gigasample so a hostile file cannot exhaust memory.
    options.sample_limit = Some(1 << 30);

    let mut decoder: JxlDecoder<states::WithImageInfo> =
        match JxlDecoder::<states::Initialized>::new(options)
            .process(&mut input, Some(&mut runner))
            .map_err(jxl_err)?
        {
            ProcessingResult::Complete { result } => result,
            ProcessingResult::NeedsMoreInput { .. } => return Err(truncated()),
        };

    let info = decoder.basic_info().clone();
    let (w, h) = info.size;
    let (w32, h32) = (
        u32::try_from(w).map_err(|_| DecodeError::Invalid("image too large".into()))?,
        u32::try_from(h).map_err(|_| DecodeError::Invalid("image too large".into()))?,
    );
    let grayscale = decoder.current_pixel_format().color_type.is_grayscale();
    let alpha = info
        .extra_channels
        .iter()
        .any(|c| c.ec_type == jxl::headers::extra_channels::ExtraChannel::Alpha);
    let animated = opts.animation && info.animation.is_some();

    // Pick the output layout: RGBA8 for animations, otherwise keep bit depth.
    #[derive(Clone, Copy, PartialEq)]
    enum Out {
        U8,
        U16,
        F32,
    }
    let depth = match info.bit_depth {
        _ if animated => Out::U8,
        JxlBitDepth::Float { .. } => Out::F32,
        JxlBitDepth::Int { bits_per_sample } if bits_per_sample <= 8 => Out::U8,
        JxlBitDepth::Int { .. } => Out::U16,
    };
    let (grayscale, alpha) = if animated || depth == Out::F32 {
        (false, true)
    } else {
        (grayscale, alpha)
    };
    let color_type = match (grayscale, alpha) {
        (true, false) => JxlColorType::Grayscale,
        (true, true) => JxlColorType::GrayscaleAlpha,
        (false, false) => JxlColorType::Rgb,
        (false, true) => JxlColorType::Rgba,
    };
    let channels = color_type.samples_per_pixel();
    let data_format = match depth {
        Out::U8 => JxlDataFormat::U8 { bit_depth: 8 },
        Out::U16 => JxlDataFormat::U16 {
            endianness: Endianness::native(),
            bit_depth: 16,
        },
        Out::F32 => JxlDataFormat::F32 {
            endianness: Endianness::native(),
        },
    };
    decoder
        .set_pixel_format(JxlPixelFormat {
            color_type,
            color_data_format: Some(data_format),
            extra_channel_format: vec![None; info.extra_channels.len()],
        })
        .map_err(jxl_err)?;
    // Only report a profile when it is not plain sRGB, so sRGB files don't
    // gain a synthesized ICC block when converted.
    let icc = {
        use jxl::api::{JxlColorEncoding, JxlColorProfile};
        let profile = decoder.output_color_profile();
        let srgb = |gray| JxlColorProfile::Simple(JxlColorEncoding::srgb(gray));
        if *profile == srgb(true) || *profile == srgb(false) {
            None
        } else {
            profile.try_as_icc().map(|p| p.into_owned())
        }
    };

    let samples = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| DecodeError::Invalid("image too large".into()))?;
    let bytes_per_sample = match depth {
        Out::U8 => 1,
        Out::U16 => 2,
        Out::F32 => 4,
    };
    // Decoding writes raw bytes; 16-bit and float samples are converted after.
    if (samples * bytes_per_sample) as u64 > MAX_ALLOC {
        return Err(DecodeError::Invalid("image too large".into()));
    }

    let mut frames: Vec<Frame> = Vec::new();
    let mut still: Option<DynamicImage> = None;
    let mut exif = None;
    loop {
        let frame_decoder = match decoder
            .process(&mut input, Some(&mut runner))
            .map_err(jxl_err)?
        {
            ProcessingResult::Complete { result } => result,
            ProcessingResult::NeedsMoreInput { .. } => return Err(truncated()),
        };
        let duration_ms = frame_decoder.frame_header().duration.unwrap_or(0.0);

        let image = match depth {
            Out::U8 => {
                let mut buf = vec![0u8; samples];
                decoder = decode_jxl_frame(frame_decoder, &mut input, &mut runner, &mut buf, h)?;
                buffer_to_dynamic(w32, h32, channels, buf)?
            }
            Out::U16 => {
                let mut raw = vec![0u8; samples * 2];
                decoder = decode_jxl_frame(frame_decoder, &mut input, &mut runner, &mut raw, h)?;
                let buf: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                    .collect();
                buffer16_to_dynamic(w32, h32, channels, buf)?
            }
            Out::F32 => {
                let mut raw = vec![0u8; samples * 4];
                decoder = decode_jxl_frame(frame_decoder, &mut input, &mut runner, &mut raw, h)?;
                let buf: Vec<f32> = raw
                    .chunks_exact(4)
                    .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let img = image::Rgba32FImage::from_raw(w32, h32, buf)
                    .ok_or_else(|| DecodeError::Invalid("unexpected JPEG XL buffer size".into()))?;
                DynamicImage::ImageRgba32F(img)
            }
        };
        if exif.is_none() {
            exif = jxl_exif(decoder.aux_boxes(JxlAuxBoxType::EXIF));
        }

        if animated {
            frames.push(Frame {
                image: image.into_rgba8(),
                delay: normalize_delay(duration_ms.round().max(0.0) as u32),
            });
            if !decoder.has_more_frames() || frames.len() > MAX_FRAMES {
                break;
            }
        } else {
            still = Some(image);
            break;
        }
    }
    if frames.len() > MAX_FRAMES {
        frames.truncate(1);
    }

    let image = match still {
        Some(img) => img,
        None => DynamicImage::ImageRgba8(
            frames
                .first()
                .map(|f| f.image.clone())
                .ok_or_else(|| DecodeError::Invalid("JPEG XL file has no frames".into()))?,
        ),
    };
    if frames.len() <= 1 {
        frames.clear();
    }
    Ok(Decoded {
        format: Format::JpegXl,
        image,
        frames,
        exif,
        icc,
    })
}

fn decode_jxl_frame(
    mut decoder: jxl::api::JxlDecoder<jxl::api::states::WithFrameInfo>,
    input: &mut &[u8],
    runner: &mut RayonRunner,
    buf: &mut [u8],
    height: usize,
) -> Result<jxl::api::JxlDecoder<jxl::api::states::WithImageInfo>, DecodeError> {
    use jxl::api::{JxlOutputBuffer, ProcessingResult};

    let bytes_per_row = buf.len() / height.max(1);
    let mut output = JxlOutputBuffer::new(buf, height, bytes_per_row);
    loop {
        match decoder
            .process(input, std::slice::from_mut(&mut output), Some(&mut *runner))
            .map_err(jxl_err)?
        {
            ProcessingResult::Complete { result } => return Ok(result),
            ProcessingResult::NeedsMoreInput { fallback, .. } => {
                if input.is_empty() {
                    return Err(truncated());
                }
                decoder = fallback;
            }
        }
    }
}

/// EXIF box payload is a 4-byte offset followed by the TIFF data.
fn jxl_exif(boxes: &[jxl::api::JxlAuxBox]) -> Option<Vec<u8>> {
    let b = boxes.iter().find(|b| !b.is_compressed())?;
    let raw = b.raw_data();
    let offset = u32::from_be_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let tiff = raw.get(4usize.checked_add(offset)?..)?;
    metadata::normalize_exif(tiff.to_vec())
}

fn buffer_to_dynamic(
    w: u32,
    h: u32,
    channels: usize,
    buf: Vec<u8>,
) -> Result<DynamicImage, DecodeError> {
    let bad = || DecodeError::Invalid("unexpected JPEG XL buffer size".into());
    Ok(match channels {
        1 => DynamicImage::ImageLuma8(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        2 => DynamicImage::ImageLumaA8(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        3 => DynamicImage::ImageRgb8(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        _ => DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
    })
}

fn buffer16_to_dynamic(
    w: u32,
    h: u32,
    channels: usize,
    buf: Vec<u16>,
) -> Result<DynamicImage, DecodeError> {
    let bad = || DecodeError::Invalid("unexpected JPEG XL buffer size".into());
    Ok(match channels {
        1 => DynamicImage::ImageLuma16(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        2 => DynamicImage::ImageLumaA16(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        3 => DynamicImage::ImageRgb16(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
        _ => DynamicImage::ImageRgba16(ImageBuffer::from_raw(w, h, buf).ok_or_else(bad)?),
    })
}

/// Read only the header to get dimensions cheaply (used by `chitra info`).
pub fn dimensions(path: &Path) -> Result<(u32, u32, Format), DecodeError> {
    let data = std::fs::read(path)?;
    let format = detect_format(&data, Some(path)).ok_or(DecodeError::UnknownFormat)?;
    if format == Format::JpegXl {
        use jxl::api::{JxlDecoder, JxlDecoderOptions, ProcessingResult, states};
        let mut input: &[u8] = &data;
        let decoder = match JxlDecoder::<states::Initialized>::new(JxlDecoderOptions::default())
            .process(&mut input, None)
            .map_err(jxl_err)?
        {
            ProcessingResult::Complete { result } => result,
            ProcessingResult::NeedsMoreInput { .. } => return Err(truncated()),
        };
        let (w, h) = decoder.basic_info().size;
        return Ok((w as u32, h as u32, format));
    }
    let reader = ImageReader::with_format(Cursor::new(&data), image_format(format));
    let (w, h) = reader.into_dimensions()?;
    Ok((w, h, format))
}

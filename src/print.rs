//! Printing without the UI: preparing a photo for paper, placing it on the
//! page, and writing a one-page PDF that the system print service can take.
//!
//! Sizes are in millimetres with the origin at the top-left of the paper.
//! The viewer hands the result to the platform (Windows GDI, the Linux print
//! portal, or CUPS on macOS and Linux).

use std::fmt::Write as _;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageEncoder, RgbImage};

use crate::encode::EncodeError;
use crate::resize;

/// Longest side, in pixels, of a photo prepared for printing: 300 dpi across
/// the long side of US Legal paper. More detail than that does not show on
/// paper and only makes the print job bigger.
pub const MAX_PRINT_SIDE: u32 = 4200;

/// Blank border kept around a photo printed with [`Size::Fit`] on CUPS, in mm,
/// so printers that cannot print to the edge do not cut it off.
pub const FIT_MARGIN_MM: f32 = 5.0;

/// How big the image comes out on paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// As large as fits inside the printable area, keeping the whole photo.
    Fit,
    /// Covers the whole printable area; the edges of the photo are cut off.
    Fill,
    /// Exactly this physical size (passport photos and sheets), centred on
    /// the paper and never scaled.
    Exact { width_mm: f32, height_mm: f32 },
}

/// A rectangle in mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// This rectangle with `m` mm taken off every side.
    pub fn inset(self, m: f32) -> Rect {
        let m = m.min(self.w / 2.0).min(self.h / 2.0).max(0.0);
        Rect::new(self.x + m, self.y + m, self.w - 2.0 * m, self.h - 2.0 * m)
    }
}

/// Where an image lands on the paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Turn the image a quarter turn clockwise first, because it matches the
    /// paper better that way round.
    pub rotate: bool,
    /// Where the (possibly turned) image is drawn. With [`Size::Fill`] it is
    /// larger than `clip`.
    pub dest: Rect,
    /// Nothing outside this is drawn.
    pub clip: Rect,
}

/// Place an image of `img_w` x `img_h` pixels on paper of `paper_w` x
/// `paper_h` mm whose printable part is `area`.
pub fn place(
    img_w: u32,
    img_h: u32,
    size: Size,
    paper_w: f32,
    paper_h: f32,
    area: Rect,
) -> Placement {
    let paper = Rect::new(0.0, 0.0, paper_w, paper_h);
    match size {
        Size::Fit | Size::Fill => {
            let (iw, ih) = (img_w.max(1) as f32, img_h.max(1) as f32);
            // Landscape photos go on landscape paper and portrait on portrait.
            let rotate = iw != ih && (iw > ih) != (area.w > area.h);
            let (iw, ih) = if rotate { (ih, iw) } else { (iw, ih) };
            let (sx, sy) = (area.w / iw, area.h / ih);
            let s = if size == Size::Fit {
                sx.min(sy)
            } else {
                sx.max(sy)
            };
            let (w, h) = (iw * s, ih * s);
            let dest = Rect::new(
                area.x + (area.w - w) / 2.0,
                area.y + (area.h - h) / 2.0,
                w,
                h,
            );
            Placement {
                rotate,
                dest,
                clip: area,
            }
        }
        Size::Exact {
            width_mm,
            height_mm,
        } => {
            // Turn it only when it fits better that way, e.g. a landscape
            // 6 x 4 in sheet on paper the printer reports as portrait 4 x 6.
            let fits = |w: f32, h: f32| w <= paper_w + 0.5 && h <= paper_h + 0.5;
            let rotate = !fits(width_mm, height_mm)
                && (fits(height_mm, width_mm) || (width_mm > height_mm) != (paper_w > paper_h));
            let (w, h) = if rotate {
                (height_mm, width_mm)
            } else {
                (width_mm, height_mm)
            };
            let dest = Rect::new((paper_w - w) / 2.0, (paper_h - h) / 2.0, w, h);
            Placement {
                rotate,
                dest,
                clip: paper,
            }
        }
    }
}

/// Flatten a decoded photo for paper: transparent parts become white, the
/// viewer's rotation (quarter turns clockwise) is applied, and it is shrunk to
/// at most [`MAX_PRINT_SIDE`] pixels on the long side.
pub fn prepare(image: &DynamicImage, quarter_turns: u8) -> RgbImage {
    let (w, h) = resize::fit_within(
        image.width(),
        image.height(),
        MAX_PRINT_SIDE,
        MAX_PRINT_SIDE,
    );
    let small = if w < image.width() {
        resize::resize(image, w, h)
    } else {
        image.clone()
    };
    let rgba = small.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = src[3] as u32;
        for c in 0..3 {
            dst[c] = ((src[c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    match quarter_turns % 4 {
        1 => image::imageops::rotate90(&rgb),
        2 => image::imageops::rotate180(&rgb),
        3 => image::imageops::rotate270(&rgb),
        _ => rgb,
    }
}

/// The image turned as `placement` says.
pub fn oriented(image: &RgbImage, placement: &Placement) -> Option<RgbImage> {
    placement.rotate.then(|| image::imageops::rotate90(image))
}

/// A one-page PDF of `paper_w` x `paper_h` mm with `image` placed on it.
pub fn pdf(
    image: &RgbImage,
    paper_w: f32,
    paper_h: f32,
    placement: &Placement,
) -> Result<Vec<u8>, EncodeError> {
    let turned = oriented(image, placement);
    let img = turned.as_ref().unwrap_or(image);
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 92)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| EncodeError(e.to_string()))?;
    Ok(pdf_with_jpeg(
        &jpeg,
        img.width(),
        img.height(),
        paper_w,
        paper_h,
        placement,
    ))
}

fn pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}

/// The PDF around an already encoded JPEG (DCTDecode), so the photo is stored
/// once and needs no other compression code.
fn pdf_with_jpeg(
    jpeg: &[u8],
    w: u32,
    h: u32,
    paper_w: f32,
    paper_h: f32,
    p: &Placement,
) -> Vec<u8> {
    let (pw, ph) = (pt(paper_w), pt(paper_h));
    // PDF puts the origin at the bottom left.
    let flip = |r: Rect| (pt(r.x), ph - pt(r.y + r.h), pt(r.w), pt(r.h));
    let (cx, cy, cw, ch) = flip(p.clip);
    let (dx, dy, dw, dh) = flip(p.dest);
    let content = format!(
        "q {cx:.3} {cy:.3} {cw:.3} {ch:.3} re W n {dw:.3} 0 0 {dh:.3} {dx:.3} {dy:.3} cm /Im0 Do Q\n"
    );

    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    let mut obj = |out: &mut Vec<u8>, head: String, stream: Option<&[u8]>| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{head}\n", offsets.len()).as_bytes());
        if let Some(s) = stream {
            out.extend_from_slice(b"stream\n");
            out.extend_from_slice(s);
            out.extend_from_slice(b"\nendstream\n");
        }
        out.extend_from_slice(b"endobj\n");
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".into(), None);
    obj(
        &mut out,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        None,
    );
    obj(
        &mut out,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.3} {ph:.3}] \
             /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>"
        ),
        None,
    );
    obj(
        &mut out,
        format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>",
            jpeg.len()
        ),
        Some(jpeg),
    );
    obj(
        &mut out,
        format!("<< /Length {} >>", content.len()),
        Some(content.as_bytes()),
    );
    let xref_at = out.len();
    let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for o in &offsets {
        let _ = writeln!(xref, "{o:010} 00000 n ");
    }
    let _ = write!(
        xref,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
        offsets.len() + 1
    );
    out.extend_from_slice(xref.as_bytes());
    out
}

/// Paper sizes offered when Chitra itself asks (CUPS on macOS and Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    A4,
    Letter,
    Legal,
    A5,
    Photo6x4,
    Photo15x10,
    Photo7x5,
}

impl Paper {
    pub const ALL: [Paper; 7] = [
        Paper::A4,
        Paper::Letter,
        Paper::Legal,
        Paper::A5,
        Paper::Photo6x4,
        Paper::Photo15x10,
        Paper::Photo7x5,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Paper::A4 => "A4",
            Paper::Letter => "US Letter",
            Paper::Legal => "US Legal",
            Paper::A5 => "A5",
            Paper::Photo6x4 => "6 x 4 in photo",
            Paper::Photo15x10 => "15 x 10 cm photo",
            Paper::Photo7x5 => "7 x 5 in photo",
        }
    }

    /// Portrait width and height in mm.
    pub fn size_mm(self) -> (f32, f32) {
        match self {
            Paper::A4 => (210.0, 297.0),
            Paper::Letter => (215.9, 279.4),
            Paper::Legal => (215.9, 355.6),
            Paper::A5 => (148.0, 210.0),
            Paper::Photo6x4 => (101.6, 152.4),
            Paper::Photo15x10 => (100.0, 150.0),
            Paper::Photo7x5 => (127.0, 177.8),
        }
    }

    /// The PWG media name CUPS understands.
    pub fn cups_media(self) -> &'static str {
        match self {
            Paper::A4 => "iso_a4_210x297mm",
            Paper::Letter => "na_letter_8.5x11in",
            Paper::Legal => "na_legal_8.5x14in",
            Paper::A5 => "iso_a5_148x210mm",
            Paper::Photo6x4 => "na_index-4x6_4x6in",
            Paper::Photo15x10 => "om_small-photo_100x150mm",
            Paper::Photo7x5 => "na_5x7_5x7in",
        }
    }

    /// The paper with this size either way round, within half a mm.
    pub fn matching(width_mm: f32, height_mm: f32) -> Option<Paper> {
        let (a, b) = (width_mm.min(height_mm), width_mm.max(height_mm));
        Paper::ALL.into_iter().find(|p| {
            let (w, h) = p.size_mm();
            (w - a).abs() < 0.5 && (h - b).abs() < 0.5
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: (f32, f32) = (210.0, 297.0);

    #[test]
    fn fit_keeps_the_whole_photo_and_turns_landscape_photos() {
        let area = Rect::new(0.0, 0.0, A4.0, A4.1).inset(5.0);
        let p = place(6000, 4000, Size::Fit, A4.0, A4.1, area);
        assert!(p.rotate);
        assert!((p.dest.h - 287.0).abs() < 0.01, "{:?}", p.dest);
        assert!((p.dest.w - 287.0 * 2.0 / 3.0).abs() < 0.01);
        assert!(p.dest.x >= area.x - 0.01 && p.dest.y >= area.y - 0.01);
        let q = place(3000, 4000, Size::Fit, A4.0, A4.1, area);
        assert!(!q.rotate);
    }

    #[test]
    fn fill_covers_the_area() {
        let area = Rect::new(0.0, 0.0, A4.0, A4.1);
        let p = place(1000, 1000, Size::Fill, A4.0, A4.1, area);
        assert!(!p.rotate);
        assert!((p.dest.w - 297.0).abs() < 0.01 && (p.dest.h - 297.0).abs() < 0.01);
        assert_eq!(p.clip, area);
    }

    #[test]
    fn exact_size_is_never_scaled() {
        let area = Rect::new(0.0, 0.0, A4.0, A4.1).inset(5.0);
        let p = place(
            413,
            531,
            Size::Exact {
                width_mm: 35.0,
                height_mm: 45.0,
            },
            A4.0,
            A4.1,
            area,
        );
        assert!(!p.rotate);
        assert_eq!((p.dest.w, p.dest.h), (35.0, 45.0));
        assert!((p.dest.x - 87.5).abs() < 0.01);
        // A landscape 6 x 4 in sheet on portrait 4 x 6 in paper is turned, and
        // still fills the paper exactly.
        let sheet = Size::Exact {
            width_mm: 152.4,
            height_mm: 101.6,
        };
        let p = place(
            1800,
            1200,
            sheet,
            101.6,
            152.4,
            Rect::new(0.0, 0.0, 101.6, 152.4),
        );
        assert!(p.rotate);
        assert_eq!(p.dest, Rect::new(0.0, 0.0, 101.6, 152.4));
    }

    #[test]
    fn prepare_turns_and_whitens() {
        let mut img = image::RgbaImage::new(4, 2);
        img.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        img.put_pixel(1, 0, image::Rgba([10, 20, 30, 255]));
        let out = prepare(&DynamicImage::ImageRgba8(img), 1);
        assert_eq!(out.dimensions(), (2, 4));
        // Top-left moved to the top-right with a quarter turn clockwise.
        assert_eq!(out.get_pixel(1, 0).0, [255, 255, 255]);
        assert_eq!(out.get_pixel(1, 1).0, [10, 20, 30]);
    }

    #[test]
    fn pdf_is_well_formed() {
        let img = RgbImage::from_pixel(30, 20, image::Rgb([200, 10, 10]));
        let area = Rect::new(0.0, 0.0, A4.0, A4.1).inset(FIT_MARGIN_MM);
        let p = place(30, 20, Size::Fit, A4.0, A4.1, area);
        let pdf = pdf(&img, A4.0, A4.1, &p).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(text.contains("/MediaBox [0 0 595.276 841.890]"), "{text}");
        assert!(text.contains("/Width 20 /Height 30"), "turned image");
        // Every xref offset points at its object.
        let xref = pdf.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 1;
        let tail = std::str::from_utf8(&pdf[xref..]).unwrap();
        let start: usize = tail[tail.find("startxref\n").unwrap() + 10..]
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(start, xref);
        for (i, line) in tail.lines().skip(3).take(5).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()));
        }
    }

    #[test]
    fn papers_match_either_way_round() {
        assert_eq!(Paper::matching(152.4, 101.6), Some(Paper::Photo6x4));
        assert_eq!(Paper::matching(297.0, 210.0), Some(Paper::A4));
        assert_eq!(Paper::matching(100.0, 100.0), None);
    }
}

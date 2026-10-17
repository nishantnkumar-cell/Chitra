//! Passport and visa photos: official sizes, cropping a photo to one from two
//! points the user marks (top of the head and bottom of the chin), and laying
//! copies out on a print sheet.
//!
//! The head is scaled to the middle of the allowed range, straightened so the
//! crown-chin line is vertical, centred, and placed at the spec's distance
//! from the top edge. Anything outside the source photo is filled with the
//! background colour.

use std::io::Write;
use std::str::FromStr;

use image::codecs::jpeg::{JpegEncoder, PixelDensity};
use image::{DynamicImage, ImageEncoder, Rgb, RgbImage};
use rayon::prelude::*;

use crate::encode::EncodeError;

/// One official photo size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhotoSpec {
    /// Short id for the command line, such as `uk`.
    pub id: &'static str,
    pub name: &'static str,
    pub width_mm: f32,
    pub height_mm: f32,
    /// Allowed head height, bottom of chin to top of head, in mm.
    pub head_mm: (f32, f32),
    /// Gap from the top edge of the photo to the top of the head, in mm.
    pub top_mm: f32,
    /// Exact pixel size for online applications, where the authority sets one.
    pub digital_px: Option<(u32, u32)>,
    /// Largest file the authority's upload form accepts, in KB.
    pub max_kb: Option<u32>,
    /// What the authority says about the background.
    pub background: &'static str,
}

impl PhotoSpec {
    pub fn head_target_mm(&self) -> f32 {
        (self.head_mm.0 + self.head_mm.1) / 2.0
    }

    pub fn size_label(&self) -> String {
        if self.width_mm == self.height_mm && (self.width_mm - 50.8).abs() < 0.1 {
            "2 x 2 in (51 x 51 mm)".into()
        } else {
            format!("{} x {} mm", self.width_mm, self.height_mm)
        }
    }

    /// Pixel size of one photo at `dpi`.
    pub fn pixels_at(&self, dpi: f32) -> (u32, u32) {
        (mm_to_px(self.width_mm, dpi), mm_to_px(self.height_mm, dpi))
    }

    /// Pixel size for a single photo file: the official digital size if there
    /// is one, otherwise 600 dpi, which meets every online minimum here.
    pub fn single_pixels(&self) -> (u32, u32) {
        self.digital_px.unwrap_or_else(|| self.pixels_at(600.0))
    }
}

/// Official sizes, checked against the issuing authorities' published rules
/// (October 2026). Heads are measured chin to crown.
pub const SPECS: &[PhotoSpec] = &[
    PhotoSpec {
        id: "uk",
        name: "United Kingdom passport",
        width_mm: 35.0,
        height_mm: 45.0,
        head_mm: (29.0, 34.0),
        top_mm: 5.0,
        digital_px: None,
        max_kb: None,
        background: "Plain light grey or cream",
    },
    PhotoSpec {
        id: "eu",
        name: "EU passport / Schengen visa",
        width_mm: 35.0,
        height_mm: 45.0,
        head_mm: (32.0, 36.0),
        top_mm: 4.0,
        digital_px: None,
        max_kb: None,
        background: "Plain white or very light grey",
    },
    PhotoSpec {
        id: "us",
        name: "United States passport / visa",
        width_mm: 50.8,
        height_mm: 50.8,
        head_mm: (25.4, 34.9),
        top_mm: 5.0,
        digital_px: Some((1200, 1200)),
        max_kb: None,
        background: "Plain white or off-white",
    },
    PhotoSpec {
        id: "india",
        name: "India passport",
        width_mm: 35.0,
        height_mm: 45.0,
        head_mm: (36.0, 38.0),
        top_mm: 3.0,
        digital_px: Some((630, 810)),
        max_kb: Some(250),
        background: "Plain white only",
    },
    PhotoSpec {
        id: "india-visa",
        name: "India visa / OCI",
        width_mm: 50.8,
        height_mm: 50.8,
        head_mm: (25.4, 34.9),
        top_mm: 5.0,
        digital_px: None,
        max_kb: None,
        background: "Plain white or off-white",
    },
    PhotoSpec {
        id: "canada",
        name: "Canada passport",
        width_mm: 50.0,
        height_mm: 70.0,
        head_mm: (31.0, 36.0),
        top_mm: 9.0,
        digital_px: None,
        max_kb: None,
        background: "Plain white or light colour",
    },
    PhotoSpec {
        id: "australia",
        name: "Australia passport",
        width_mm: 35.0,
        height_mm: 45.0,
        head_mm: (32.0, 36.0),
        top_mm: 4.0,
        digital_px: None,
        max_kb: None,
        background: "Plain white or light grey",
    },
    PhotoSpec {
        id: "china",
        name: "China passport / visa",
        width_mm: 33.0,
        height_mm: 48.0,
        head_mm: (28.0, 33.0),
        top_mm: 4.0,
        digital_px: None,
        max_kb: None,
        background: "Plain white",
    },
    PhotoSpec {
        id: "japan",
        name: "Japan passport",
        width_mm: 35.0,
        height_mm: 45.0,
        head_mm: (32.0, 36.0),
        top_mm: 4.0,
        digital_px: None,
        max_kb: None,
        background: "Plain, no pattern",
    },
];

/// A size the user types in. The head sits 40% of the spare height below the
/// top edge, which matches most official layouts.
pub fn custom(width_mm: f32, height_mm: f32, head_mm: (f32, f32)) -> Result<PhotoSpec, String> {
    let size_ok = |v: f32| (10.0..=200.0).contains(&v);
    if !size_ok(width_mm) || !size_ok(height_mm) {
        return Err("photo sides must be between 10 and 200 mm".into());
    }
    if !(head_mm.0 > 0.0 && head_mm.0 <= head_mm.1 && head_mm.1 < height_mm - 2.0) {
        return Err("head must be at least 2 mm shorter than the photo".into());
    }
    let spare = height_mm - (head_mm.0 + head_mm.1) / 2.0;
    Ok(PhotoSpec {
        id: "custom",
        name: "Custom size",
        width_mm,
        height_mm,
        head_mm,
        top_mm: (spare * 0.4).min(height_mm - head_mm.1 - 1.0),
        digital_px: None,
        max_kb: None,
        background: "As required",
    })
}

pub fn spec(id: &str) -> Option<&'static PhotoSpec> {
    let id = id.trim().to_ascii_lowercase();
    SPECS.iter().find(|s| s.id == id)
}

/// Paper sizes for print sheets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// 6 x 4 in, the common photo print.
    Photo6x4,
    /// 15 x 10 cm.
    Photo15x10,
    /// 7 x 5 in.
    Photo7x5,
    A4,
    Letter,
}

impl Paper {
    pub const ALL: [Paper; 5] = [
        Paper::Photo6x4,
        Paper::Photo15x10,
        Paper::Photo7x5,
        Paper::A4,
        Paper::Letter,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Paper::Photo6x4 => "6 x 4 in photo print",
            Paper::Photo15x10 => "15 x 10 cm photo print",
            Paper::Photo7x5 => "7 x 5 in photo print",
            Paper::A4 => "A4",
            Paper::Letter => "US Letter",
        }
    }

    /// Long side first, in mm.
    pub fn size_mm(self) -> (f32, f32) {
        match self {
            Paper::Photo6x4 => (152.4, 101.6),
            Paper::Photo15x10 => (150.0, 100.0),
            Paper::Photo7x5 => (177.8, 127.0),
            Paper::A4 => (297.0, 210.0),
            Paper::Letter => (279.4, 215.9),
        }
    }
}

impl FromStr for Paper {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "6x4" | "4x6" => Ok(Paper::Photo6x4),
            "15x10" | "10x15" => Ok(Paper::Photo15x10),
            "7x5" | "5x7" => Ok(Paper::Photo7x5),
            "a4" => Ok(Paper::A4),
            "letter" => Ok(Paper::Letter),
            _ => Err(format!(
                "unknown paper '{s}' (use 6x4, 15x10, 7x5, a4 or letter)"
            )),
        }
    }
}

/// The two points the user marks on the source photo, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    /// Top of the head (crown, not the top of the hair).
    pub crown: (f32, f32),
    /// Bottom of the chin.
    pub chin: (f32, f32),
}

impl Marks {
    /// Head height in source pixels; marks closer than this are rejected.
    const MIN_HEAD_PX: f32 = 8.0;

    fn head_px(&self) -> f32 {
        let (dx, dy) = (self.chin.0 - self.crown.0, self.chin.1 - self.crown.1);
        (dx * dx + dy * dy).sqrt()
    }

    pub fn valid(&self) -> bool {
        self.chin.1 > self.crown.1 && self.head_px() >= Self::MIN_HEAD_PX
    }

    pub fn scaled(&self, factor: f32) -> Marks {
        Marks {
            crown: (self.crown.0 * factor, self.crown.1 * factor),
            chin: (self.chin.0 * factor, self.chin.1 * factor),
        }
    }
}

/// How to crop one photo.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub marks: Marks,
    /// Head height to aim for, in mm (normally the middle of the spec's range).
    pub head_mm: f32,
    /// Rotate so the crown-chin line is vertical.
    pub straighten: bool,
    /// Fill for anything outside the source photo.
    pub background: [u8; 3],
}

impl Layout {
    pub fn new(spec: &PhotoSpec, marks: Marks) -> Self {
        Layout {
            marks,
            head_mm: spec.head_target_mm(),
            straighten: true,
            background: [255, 255, 255],
        }
    }
}

/// Where each output pixel comes from: `src = origin + x * right + y * down`,
/// with `(x, y)` the output pixel centre.
#[derive(Clone, Copy, Debug)]
struct Mapping {
    origin: (f32, f32),
    right: (f32, f32),
    down: (f32, f32),
}

impl Mapping {
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 + x * self.right.0 + y * self.down.0,
            self.origin.1 + x * self.right.1 + y * self.down.1,
        )
    }

    /// Source pixels per output pixel.
    fn step(&self) -> f32 {
        (self.right.0 * self.right.0 + self.right.1 * self.right.1).sqrt()
    }
}

fn mapping(spec: &PhotoSpec, layout: &Layout, out_w: u32, out_h: u32) -> Mapping {
    let m = layout.marks;
    let (anchor, dir, head_src) = if layout.straighten {
        let (dx, dy) = (m.chin.0 - m.crown.0, m.chin.1 - m.crown.1);
        let len = m.head_px();
        (m.crown, (dx / len, dy / len), len)
    } else {
        let mid_x = (m.crown.0 + m.chin.0) / 2.0;
        ((mid_x, m.crown.1), (0.0, 1.0), m.chin.1 - m.crown.1)
    };
    let px_per_mm = out_h as f32 / spec.height_mm;
    // Output pixels -> source pixels.
    let s = head_src / (layout.head_mm * px_per_mm);
    let down = (dir.0 * s, dir.1 * s);
    let right = (dir.1 * s, -dir.0 * s);
    // The crown lands at the horizontal centre, top_mm below the top edge.
    let (cx, cy) = (out_w as f32 / 2.0, spec.top_mm * px_per_mm);
    let origin = (
        anchor.0 - cx * right.0 - cy * down.0,
        anchor.1 - cx * right.1 - cy * down.1,
    );
    Mapping {
        origin,
        right,
        down,
    }
}

/// Crop, scale and straighten `src` into one `out_w` x `out_h` passport photo.
pub fn render(
    src: &DynamicImage,
    spec: &PhotoSpec,
    layout: &Layout,
    out_w: u32,
    out_h: u32,
) -> RgbImage {
    let mut map = mapping(spec, layout, out_w, out_h);

    // Only the part of the photo the output covers is needed.
    let corners = [
        map.apply(0.0, 0.0),
        map.apply(out_w as f32, 0.0),
        map.apply(0.0, out_h as f32),
        map.apply(out_w as f32, out_h as f32),
    ];
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    let x0 = (min_x.floor() as i64 - 1).clamp(0, src.width() as i64) as u32;
    let y0 = (min_y.floor() as i64 - 1).clamp(0, src.height() as i64) as u32;
    let x1 = (max_x.ceil() as i64 + 2).clamp(0, src.width() as i64) as u32;
    let y1 = (max_y.ceil() as i64 + 2).clamp(0, src.height() as i64) as u32;
    let bg = Rgb(layout.background);
    if x1 <= x0 || y1 <= y0 {
        return RgbImage::from_pixel(out_w, out_h, bg);
    }
    let mut part = src.crop_imm(x0, y0, x1 - x0, y1 - y0).to_rgb8();
    map.origin = (map.origin.0 - x0 as f32, map.origin.1 - y0 as f32);

    // Shrink with a proper filter first so the bilinear pass below doesn't alias.
    let step = map.step();
    if step > 1.5 {
        let f = 1.0 / step;
        let w = ((part.width() as f32 * f).round() as u32).max(1);
        let h = ((part.height() as f32 * f).round() as u32).max(1);
        let (sx, sy) = (
            w as f32 / part.width() as f32,
            h as f32 / part.height() as f32,
        );
        part = crate::resize::resize(&DynamicImage::ImageRgb8(part), w, h).into_rgb8();
        map.origin = (map.origin.0 * sx, map.origin.1 * sy);
        map.right = (map.right.0 * sx, map.right.1 * sy);
        map.down = (map.down.0 * sx, map.down.1 * sy);
    }

    let mut out = RgbImage::new(out_w, out_h);
    let row_len = out_w as usize * 3;
    out.par_chunks_mut(row_len)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let (sx, sy) = map.apply(x as f32 + 0.5, y as f32 + 0.5);
                px.copy_from_slice(&sample(&part, sx - 0.5, sy - 0.5, bg).0);
            }
        });
    out
}

/// Bilinear sample; pixels outside the image count as `bg`.
fn sample(img: &RgbImage, x: f32, y: f32, bg: Rgb<u8>) -> Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = (x - fx, y - fy);
    let (ix, iy) = (fx as i64, fy as i64);
    let get = |px: i64, py: i64| -> [f32; 3] {
        let p = if px >= 0 && py >= 0 && px < w && py < h {
            *img.get_pixel(px as u32, py as u32)
        } else {
            bg
        };
        [p.0[0] as f32, p.0[1] as f32, p.0[2] as f32]
    };
    let (a, b, c, d) = (
        get(ix, iy),
        get(ix + 1, iy),
        get(ix, iy + 1),
        get(ix + 1, iy + 1),
    );
    let mut o = [0u8; 3];
    for i in 0..3 {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bottom = c[i] + (d[i] - c[i]) * tx;
        o[i] = (top + (bottom - top) * ty).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(o)
}

/// Lines to draw over a preview of a rendered photo, as fractions of its
/// height: where the top of the head goes, and the band the chin may fall in.
pub fn guides(spec: &PhotoSpec) -> (f32, (f32, f32)) {
    let h = spec.height_mm;
    let top = spec.top_mm / h;
    (
        top,
        (
            (spec.top_mm + spec.head_mm.0) / h,
            (spec.top_mm + spec.head_mm.1) / h,
        ),
    )
}

/// Print sheet settings.
#[derive(Clone, Copy, Debug)]
pub struct SheetOptions {
    pub paper: Paper,
    pub dpi: f32,
    /// Blank border around the edge of the paper, in mm.
    pub margin_mm: f32,
    /// Space between photos, in mm.
    pub gap_mm: f32,
}

impl SheetOptions {
    pub fn new(paper: Paper) -> Self {
        SheetOptions {
            paper,
            dpi: 300.0,
            margin_mm: 3.0,
            gap_mm: 2.0,
        }
    }
}

/// Columns, rows and whether the paper is portrait, for the most copies.
pub fn sheet_grid(spec: &PhotoSpec, opts: &SheetOptions) -> (u32, u32, bool) {
    let (long, short) = opts.paper.size_mm();
    let fit = |avail: f32, size: f32| {
        (((avail - 2.0 * opts.margin_mm + opts.gap_mm) / (size + opts.gap_mm)).floor()).max(0.0)
            as u32
    };
    let landscape = (fit(long, spec.width_mm), fit(short, spec.height_mm), false);
    let portrait = (fit(short, spec.width_mm), fit(long, spec.height_mm), true);
    if portrait.0 * portrait.1 > landscape.0 * landscape.1 {
        portrait
    } else {
        landscape
    }
}

/// Lay out as many copies of `photo` as fit, with thin grey cutting guides.
/// `photo` should be rendered at `spec.pixels_at(opts.dpi)`.
pub fn print_sheet(photo: &RgbImage, spec: &PhotoSpec, opts: &SheetOptions) -> (RgbImage, u32) {
    let (cols, rows, portrait) = sheet_grid(spec, opts);
    let (long, short) = opts.paper.size_mm();
    let (paper_w, paper_h) = if portrait {
        (short, long)
    } else {
        (long, short)
    };
    let (sw, sh) = (mm_to_px(paper_w, opts.dpi), mm_to_px(paper_h, opts.dpi));
    let mut sheet = RgbImage::from_pixel(sw, sh, Rgb([255, 255, 255]));
    let (pw, ph) = (photo.width(), photo.height());
    let gap = mm_to_px(opts.gap_mm, opts.dpi);
    let grid_w = cols * pw + cols.saturating_sub(1) * gap;
    let grid_h = rows * ph + rows.saturating_sub(1) * gap;
    let left = sw.saturating_sub(grid_w) / 2;
    let top = sh.saturating_sub(grid_h) / 2;
    let guide = Rgb([170, 170, 170]);
    for r in 0..rows {
        for c in 0..cols {
            let x = left + c * (pw + gap);
            let y = top + r * (ph + gap);
            image::imageops::replace(&mut sheet, photo, x as i64, y as i64);
            outline(
                &mut sheet,
                x as i64 - 1,
                y as i64 - 1,
                pw + 2,
                ph + 2,
                guide,
            );
        }
    }
    (sheet, cols * rows)
}

fn outline(img: &mut RgbImage, x: i64, y: i64, w: u32, h: u32, color: Rgb<u8>) {
    let mut put = |px: i64, py: i64| {
        if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
            img.put_pixel(px as u32, py as u32, color);
        }
    };
    for i in 0..w as i64 {
        put(x + i, y);
        put(x + i, y + h as i64 - 1);
    }
    for j in 0..h as i64 {
        put(x, y + j);
        put(x + w as i64 - 1, y + j);
    }
}

fn mm_to_px(mm: f32, dpi: f32) -> u32 {
    ((mm / 25.4 * dpi).round() as u32).max(1)
}

/// Dots per inch of an image `px` pixels tall printed `mm` tall.
pub fn dpi_of(px: u32, mm: f32) -> f32 {
    px as f32 / (mm / 25.4)
}

/// Encode a JPEG for saving: quality 95, or the highest quality (down to 30)
/// that fits in `max_kb` when a limit is given.
pub fn encode_jpeg(img: &RgbImage, dpi: f32, max_kb: Option<u32>) -> Result<Vec<u8>, EncodeError> {
    let mut quality = 95;
    loop {
        let mut buf = Vec::new();
        write_jpeg(img, dpi, quality, &mut buf)?;
        match max_kb {
            Some(kb) if buf.len() > kb as usize * 1024 => {
                if quality <= 30 {
                    return Err(EncodeError(format!(
                        "the photo does not fit in {kb} KB even at low quality"
                    )));
                }
                quality -= 5;
            }
            _ => return Ok(buf),
        }
    }
}

/// Write a JPEG tagged with its print resolution, so printing at "actual
/// size" gives the right physical size. No EXIF is written.
pub fn write_jpeg<W: Write>(
    img: &RgbImage,
    dpi: f32,
    quality: u8,
    out: W,
) -> Result<(), EncodeError> {
    let mut enc = JpegEncoder::new_with_quality(out, quality.clamp(1, 100));
    enc.set_pixel_density(PixelDensity::dpi(dpi.round().clamp(1.0, 65535.0) as u16));
    enc.write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey photo with a dark "head" from y=100 (crown) to y=300 (chin).
    fn face() -> DynamicImage {
        let mut img = RgbImage::from_pixel(400, 500, Rgb([200, 200, 200]));
        for y in 100..300 {
            for x in 150..250 {
                img.put_pixel(x, y, Rgb([20, 20, 20]));
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    fn marks() -> Marks {
        Marks {
            crown: (200.0, 100.0),
            chin: (200.0, 300.0),
        }
    }

    /// First and last dark rows in the centre column.
    fn head_rows(img: &RgbImage) -> (u32, u32) {
        let x = img.width() / 2;
        let dark: Vec<u32> = (0..img.height())
            .filter(|&y| img.get_pixel(x, y).0[0] < 110)
            .collect();
        (dark[0], *dark.last().unwrap())
    }

    #[test]
    fn specs_are_sane() {
        for s in SPECS {
            assert!(s.head_mm.0 < s.head_mm.1, "{}", s.id);
            assert!(s.top_mm + s.head_mm.1 < s.height_mm, "{}", s.id);
            assert_eq!(spec(s.id), Some(s));
        }
        assert_eq!(spec("UK").unwrap().width_mm, 35.0);
        assert!(spec("mars").is_none());
    }

    #[test]
    fn head_lands_where_the_spec_says() {
        let s = spec("uk").unwrap();
        let (w, h) = s.pixels_at(300.0);
        assert_eq!((w, h), (413, 531));
        let out = render(&face(), s, &Layout::new(s, marks()), w, h);
        let px_per_mm = h as f32 / s.height_mm;
        let (top, bottom) = head_rows(&out);
        let head_mm = (bottom - top + 1) as f32 / px_per_mm;
        assert!(
            (head_mm - s.head_target_mm()).abs() < 0.3,
            "head {head_mm} mm"
        );
        assert!((top as f32 / px_per_mm - s.top_mm).abs() < 0.3, "top {top}");
        // Centred: the head's left and right edges are equally far from the sides.
        let row = top + 10;
        let dark: Vec<u32> = (0..w)
            .filter(|&x| out.get_pixel(x, row).0[0] < 110)
            .collect();
        let (l, r) = (dark[0], w - 1 - dark.last().unwrap());
        assert!(l.abs_diff(r) <= 1, "{l} vs {r}");
    }

    #[test]
    fn tilted_head_is_straightened() {
        // Rotate the face 90 degrees: the head now runs left to right.
        let img = face().rotate270();
        // A point (x, y) moves to (y, 400 - x) under rotate270.
        let m = Marks {
            crown: (100.0, 200.0),
            chin: (300.0, 200.0),
        };
        let s = spec("eu").unwrap();
        let (w, h) = s.pixels_at(300.0);
        let out = render(&img, s, &Layout::new(s, m), w, h);
        let (top, bottom) = head_rows(&out);
        let head_mm = (bottom - top + 1) as f32 / (h as f32 / s.height_mm);
        assert!(
            (head_mm - s.head_target_mm()).abs() < 0.3,
            "head {head_mm} mm"
        );
    }

    #[test]
    fn outside_the_photo_is_background() {
        let s = spec("canada").unwrap();
        let mut layout = Layout::new(s, marks());
        layout.background = [255, 0, 0];
        // Leave 20 px above the crown, less than Canada's 9 mm top margin needs.
        let mut m = marks();
        m.crown.1 -= 80.0;
        m.chin.1 -= 80.0;
        layout.marks = m;
        let out = render(&face().crop_imm(0, 80, 400, 420), s, &layout, 500, 700);
        assert_eq!(out.get_pixel(250, 0).0, [255, 0, 0]);
    }

    #[test]
    fn large_photos_are_shrunk_without_losing_placement() {
        let big = face().resize_exact(4000, 5000, image::imageops::FilterType::Nearest);
        let s = spec("us").unwrap();
        let (w, h) = s.single_pixels();
        let out = render(&big, s, &Layout::new(s, marks().scaled(10.0)), w, h);
        let (top, bottom) = head_rows(&out);
        let head_mm = (bottom - top + 1) as f32 / (h as f32 / s.height_mm);
        assert!(
            (head_mm - s.head_target_mm()).abs() < 0.3,
            "head {head_mm} mm"
        );
    }

    #[test]
    fn sheets() {
        let uk = spec("uk").unwrap();
        let opts = SheetOptions::new(Paper::Photo6x4);
        assert_eq!(sheet_grid(uk, &opts), (4, 2, false));
        let us = spec("us").unwrap();
        assert_eq!(sheet_grid(us, &opts), (2, 1, false));
        let a4 = SheetOptions::new(Paper::A4);
        let (c, r, _) = sheet_grid(uk, &a4);
        assert!(c * r >= 20, "{c}x{r}");

        let photo = RgbImage::from_pixel(413, 531, Rgb([0, 0, 0]));
        let (sheet, n) = print_sheet(&photo, uk, &opts);
        assert_eq!(n, 8);
        assert_eq!((sheet.width(), sheet.height()), (1800, 1200));
        let black = sheet.pixels().filter(|p| p.0 == [0, 0, 0]).count();
        assert_eq!(black, 8 * 413 * 531);
    }

    #[test]
    fn jpeg_carries_dpi() {
        let mut buf = Vec::new();
        write_jpeg(&RgbImage::new(10, 10), 300.0, 95, &mut buf).unwrap();
        // JFIF APP0: units = 1 (inches), then 300 x 300.
        let jfif = buf.windows(5).position(|w| w == b"JFIF\0").unwrap();
        assert_eq!(&buf[jfif + 7..jfif + 12], &[1, 1, 44, 1, 44]);
    }

    #[test]
    fn size_limit_lowers_quality() {
        // Noise compresses badly, so a tight limit forces a lower quality.
        let mut img = RgbImage::new(300, 300);
        for (i, p) in img.pixels_mut().enumerate() {
            let v = (i as u32).wrapping_mul(2_654_435_761) >> 24;
            *p = Rgb([v as u8, (v >> 1) as u8, (v >> 2) as u8]);
        }
        let free = encode_jpeg(&img, 300.0, None).unwrap();
        let kb = (free.len() / 1024 / 2) as u32;
        let small = encode_jpeg(&img, 300.0, Some(kb)).unwrap();
        assert!(small.len() <= kb as usize * 1024 && small.len() < free.len());
        assert!(encode_jpeg(&img, 300.0, Some(1)).is_err());
    }

    #[test]
    fn custom_sizes() {
        let c = custom(40.0, 50.0, (30.0, 34.0)).unwrap();
        assert!(c.top_mm > 0.0 && c.top_mm + c.head_mm.1 < c.height_mm);
        assert!(custom(5.0, 50.0, (30.0, 34.0)).is_err());
        assert!(custom(40.0, 50.0, (34.0, 30.0)).is_err());
        assert!(custom(40.0, 50.0, (30.0, 49.0)).is_err());
    }

    #[test]
    fn bad_marks() {
        let flipped = Marks {
            crown: (0.0, 50.0),
            chin: (0.0, 10.0),
        };
        assert!(!flipped.valid());
        assert!(marks().valid());
        assert_eq!("4x6".parse::<Paper>(), Ok(Paper::Photo6x4));
        assert!("b5".parse::<Paper>().is_err());
    }
}

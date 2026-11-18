//! Optional AI upscaling with Real-ESRGAN's compact general model
//! (realesr-general-x4v3, BSD-3-Clause, by Xintao Wang et al.).
//!
//! The model files are not part of Chitra: they are downloaded on first use
//! (about 10 MB) into the config folder, or picked by hand, and are only ever
//! accepted when their SHA-256 matches the published files. The network is a
//! small convolutional net (34 3x3 convolutions with PReLU, then a 4x pixel
//! shuffle), run here in plain Rust: the convolutions are matrix products on
//! `ndarray`/`matrixmultiply`, which pick AVX/FMA or NEON at run time. The
//! photo is processed in overlapping tiles, in parallel.
//!
//! Two variants are blended, as Real-ESRGAN does for its "denoise strength":
//! the general model removes noise, the "wdn" model keeps more of it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use image::{DynamicImage, ImageBuffer, Rgb, Rgba};
use ndarray::linalg::general_mat_mul;
use ndarray::{ArrayView2, ArrayViewMut2, ShapeBuilder};
use rayon::prelude::*;

/// One model file: name in the models folder, where to get it, its SHA-256.
pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

pub const GENERAL: ModelFile = ModelFile {
    name: "realesr-general-x4v3.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesr-general-x4v3.pth",
    sha256: "8dc7edb9ac80ccdc30c3a5dca6616509367f05fbc184ad95b731f05bece96292",
    bytes: 4_885_111,
};

pub const WEAK_DENOISE: ModelFile = ModelFile {
    name: "realesr-general-wdn-x4v3.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesr-general-wdn-x4v3.pth",
    sha256: "1641f8c4464b9f097c9fdda5589273713f67cf59f3d909e0bd688f0cee269dca",
    bytes: 4_885_111,
};

pub const FILES: [&ModelFile; 2] = [&GENERAL, &WEAK_DENOISE];

/// The model always enlarges 4x; other sizes are resampled from that.
pub const MODEL_SCALE: u32 = 4;

const FEAT: usize = 64;
const CONVS: usize = 34;
/// Input pixels per tile side, and the overlap read around each tile.
const TILE: usize = 256;
pub const PAD: usize = 20;

/// Chitra's config folder: `%APPDATA%\Chitra`, `~/Library/Application
/// Support/Chitra` or `$XDG_CONFIG_HOME/Chitra`.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(base.join("Chitra"))
}

/// Where the model files live. `CHITRA_MODELS` overrides it (for tests).
pub fn models_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CHITRA_MODELS") {
        return Some(PathBuf::from(d));
    }
    config_dir().map(|d| d.join("models"))
}

/// True when both model files are in place (their contents are checked on load).
pub fn installed() -> bool {
    models_dir().is_some_and(|d| FILES.iter().all(|f| d.join(f.name).is_file()))
}

/// Download the model files with the system's `curl` (part of Windows 10 and
/// later, macOS and most Linux desktops), checking each one's SHA-256.
/// Chitra itself has no network code.
pub fn download() -> Result<(), String> {
    let dir = models_dir().ok_or("no config folder")?;
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for f in FILES {
        let dest = dir.join(f.name);
        if dest.is_file() && file_sha256(&dest).ok().as_deref() == Some(f.sha256) {
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let status = Command::new("curl")
            .args([
                "-fsSL",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--retry",
                "2",
                "-o",
            ])
            .arg(&part)
            .arg(f.url)
            .status()
            .map_err(|e| {
                format!("cannot run curl ({e}); download the files by hand and choose them instead")
            })?;
        if !status.success() {
            let _ = fs::remove_file(&part);
            return Err(format!("download of {} failed ({status})", f.name));
        }
        let got = file_sha256(&part)?;
        if got != f.sha256 {
            let _ = fs::remove_file(&part);
            return Err(format!("{} did not match its checksum; not used", f.name));
        }
        fs::rename(&part, &dest).map_err(|e| format!("cannot save {}: {e}", dest.display()))?;
    }
    forget_loaded();
    Ok(())
}

/// Copy model files the user downloaded themselves into the models folder.
/// Files are recognised by checksum, whatever their name. Returns how many
/// were taken.
pub fn import(paths: &[PathBuf]) -> Result<usize, String> {
    let dir = models_dir().ok_or("no config folder")?;
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut taken = 0;
    for p in paths {
        let sum = file_sha256(p)?;
        let Some(f) = FILES.iter().find(|f| f.sha256 == sum) else {
            return Err(format!(
                "{} is not one of the expected model files",
                p.file_name().unwrap_or_default().to_string_lossy()
            ));
        };
        fs::copy(p, dir.join(f.name)).map_err(|e| format!("cannot copy: {e}"))?;
        taken += 1;
    }
    forget_loaded();
    Ok(taken)
}

// ----- the network -----------------------------------------------------------

/// One 3x3 convolution: `w[k]` is a `cin` x `cout` matrix for each of the
/// nine kernel positions, row-major.
#[derive(Clone)]
struct Conv {
    cin: usize,
    cout: usize,
    w: Vec<f32>,
    bias: Vec<f32>,
    /// PReLU slope per output channel; empty for the last layer.
    prelu: Vec<f32>,
}

#[derive(Clone)]
struct Net {
    convs: Vec<Conv>,
}

impl Net {
    fn blend(a: &Net, b: &Net, t: f32) -> Net {
        let mix = |x: &[f32], y: &[f32]| {
            x.iter()
                .zip(y)
                .map(|(p, q)| t * p + (1.0 - t) * q)
                .collect()
        };
        Net {
            convs: a
                .convs
                .iter()
                .zip(&b.convs)
                .map(|(p, q)| Conv {
                    cin: p.cin,
                    cout: p.cout,
                    w: mix(&p.w, &q.w),
                    bias: mix(&p.bias, &q.bias),
                    prelu: mix(&p.prelu, &q.prelu),
                })
                .collect(),
        }
    }
}

struct Loaded {
    general: Net,
    weak: Net,
    /// The last blend made, by noise-removal strength in hundredths.
    blended: Option<(u32, Arc<Net>)>,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

fn forget_loaded() {
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The network for a noise-removal strength from 0 (keep noise) to 1.
fn net(strength: f32) -> Result<Arc<Net>, String> {
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let dir = models_dir().ok_or("no config folder")?;
        *guard = Some(Loaded {
            general: load_net(&dir.join(GENERAL.name), &GENERAL)?,
            weak: load_net(&dir.join(WEAK_DENOISE.name), &WEAK_DENOISE)?,
            blended: None,
        });
    }
    let loaded = guard.as_mut().ok_or("model not loaded")?;
    let key = (strength.clamp(0.0, 1.0) * 100.0).round() as u32;
    if let Some((k, n)) = &loaded.blended
        && *k == key
    {
        return Ok(n.clone());
    }
    let n = Arc::new(Net::blend(
        &loaded.general,
        &loaded.weak,
        key as f32 / 100.0,
    ));
    loaded.blended = Some((key, n.clone()));
    Ok(n)
}

fn not_installed() -> String {
    "the AI model is not installed: choose Download in the Upscale and sharpen window, \
     or run chitra-cli ai-model download"
        .into()
}

fn load_net(path: &Path, file: &ModelFile) -> Result<Net, String> {
    let bytes = fs::read(path).map_err(|_| not_installed())?;
    if sha256_hex(&bytes) != file.sha256 {
        return Err(format!("{} is damaged; download it again", file.name));
    }
    let entries = zip_entries(&bytes)?;
    let tensor = |n: usize, len: usize| -> Result<Vec<f32>, String> {
        let suffix = format!("/data/{n}");
        let data = entries
            .iter()
            .find(|(name, _)| name.ends_with(&suffix))
            .map(|(_, d)| *d)
            .ok_or_else(|| format!("{}: tensor {n} missing", file.name))?;
        if data.len() != len * 4 {
            return Err(format!("{}: tensor {n} has the wrong size", file.name));
        }
        Ok(data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    };
    let mut convs = Vec::with_capacity(CONVS);
    for j in 0..CONVS {
        let cin = if j == 0 { 3 } else { FEAT };
        let cout = if j == CONVS - 1 { 3 * 16 } else { FEAT };
        // Stored as [cout][cin][3][3] (storages are numbered in layer order:
        // weight, bias, PReLU slope).
        let raw = tensor(3 * j, cout * cin * 9)?;
        let mut w = vec![0f32; 9 * cin * cout];
        for co in 0..cout {
            for ci in 0..cin {
                for k in 0..9 {
                    w[k * cin * cout + ci * cout + co] = raw[(co * cin + ci) * 9 + k];
                }
            }
        }
        let bias = tensor(3 * j + 1, cout)?;
        let prelu = if j < CONVS - 1 {
            tensor(3 * j + 2, cout)?
        } else {
            Vec::new()
        };
        convs.push(Conv {
            cin,
            cout,
            w,
            bias,
            prelu,
        });
    }
    Ok(Net { convs })
}

/// Run the net on one `w` x `h` RGB tile (values 0..1, row-major), giving the
/// 4x result.
fn run_tile(net: &Net, rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    let wp = w + 2;
    let hp = h + 2;
    // Padded HWC buffers with a zero border, as the convolutions pad with zeros.
    let mut cur = vec![0f32; hp * wp * 3];
    for y in 0..h {
        let dst = ((y + 1) * wp + 1) * 3;
        cur[dst..dst + w * 3].copy_from_slice(&rgb[y * w * 3..(y + 1) * w * 3]);
    }
    let mut next = vec![0f32; hp * wp * FEAT];
    // Output row o = y * wp + x for the pixel (x, y); its input for kernel
    // position (ky, kx) is padded pixel o + ky * wp + kx. The two rows
    // between image rows are computed too and then cleared.
    let m = h * wp - 2;
    for conv in &net.convs {
        let (cin, cout) = (conv.cin, conv.cout);
        let out_len = hp * wp * cout;
        if next.len() < out_len {
            next.resize(out_len, 0.0);
        }
        let out = &mut next[..out_len];
        out.iter_mut().for_each(|v| *v = 0.0);
        {
            let base = (wp + 1) * cout;
            let span = (m - 1) * cout + cout;
            let mut c = ArrayViewMut2::from_shape(
                (m, cout).strides((cout, 1)),
                &mut out[base..base + span],
            )
            .expect("output view");
            for k in 0..9 {
                let off = ((k / 3) * wp + k % 3) * cin;
                let a = ArrayView2::from_shape(
                    (m, cin).strides((cin, 1)),
                    &cur[off..off + (m - 1) * cin + cin],
                )
                .expect("input view");
                let b = ArrayView2::from_shape(
                    (cin, cout),
                    &conv.w[k * cin * cout..(k + 1) * cin * cout],
                )
                .expect("weight view");
                general_mat_mul(1.0, &a, &b, 1.0, &mut c);
            }
        }
        // Bias and PReLU on the image pixels; zero the border again.
        out.par_chunks_mut(wp * cout)
            .enumerate()
            .for_each(|(row, line)| {
                if row == 0 || row == hp - 1 {
                    line.iter_mut().for_each(|v| *v = 0.0);
                    return;
                }
                line[..cout].iter_mut().for_each(|v| *v = 0.0);
                line[(wp - 1) * cout..].iter_mut().for_each(|v| *v = 0.0);
                for px in line[cout..(wp - 1) * cout].chunks_exact_mut(cout) {
                    for (i, v) in px.iter_mut().enumerate() {
                        let x = *v + conv.bias[i];
                        *v = if conv.prelu.is_empty() || x >= 0.0 {
                            x
                        } else {
                            x * conv.prelu[i]
                        };
                    }
                }
            });
        std::mem::swap(&mut cur, &mut next);
    }
    // Pixel shuffle (channel c * 16 + i * 4 + j goes to sub-pixel (j, i) of
    // colour c) plus the nearest-neighbour enlargement of the input.
    let s = MODEL_SCALE as usize;
    let (ow, oh) = (w * s, h * s);
    let mut out = vec![0f32; ow * oh * 3];
    out.par_chunks_mut(ow * 3)
        .enumerate()
        .for_each(|(oy, line)| {
            let (y, i) = (oy / s, oy % s);
            for ox in 0..ow {
                let (x, j) = (ox / s, ox % s);
                let feat = &cur[((y + 1) * wp + x + 1) * 48..][..48];
                for c in 0..3 {
                    let v = feat[c * 16 + i * 4 + j] + rgb[(y * w + x) * 3 + c];
                    line[ox * 3 + c] = v.clamp(0.0, 1.0);
                }
            }
        });
    out
}

/// Enlarge 4x with the AI model. `noise` 0 keeps the photo's noise, 1
/// removes it (the model's default). Alpha is enlarged separately.
pub fn upscale(image: &DynamicImage, noise: f32) -> Result<DynamicImage, String> {
    if !installed() {
        return Err(not_installed());
    }
    let net = net(noise)?;
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.to_rgb32f();
    let src = src.as_raw();
    let s = MODEL_SCALE as usize;
    let (ow, oh) = (w * s, h * s);

    let tiles: Vec<(usize, usize)> = (0..h.div_ceil(TILE))
        .flat_map(|ty| (0..w.div_ceil(TILE)).map(move |tx| (tx * TILE, ty * TILE)))
        .collect();
    let done: Vec<(usize, usize, usize, usize, Vec<f32>)> = tiles
        .par_iter()
        .map(|&(x, y)| {
            let (tw, th) = (TILE.min(w - x), TILE.min(h - y));
            let x0 = x.saturating_sub(PAD);
            let y0 = y.saturating_sub(PAD);
            let x1 = (x + tw + PAD).min(w);
            let y1 = (y + th + PAD).min(h);
            let (pw, ph) = (x1 - x0, y1 - y0);
            let mut crop = Vec::with_capacity(pw * ph * 3);
            for yy in y0..y1 {
                crop.extend_from_slice(&src[(yy * w + x0) * 3..(yy * w + x1) * 3]);
            }
            let big = run_tile(&net, &crop, pw, ph);
            // Keep the tile's own area.
            let (bx, by) = ((x - x0) * s, (y - y0) * s);
            let mut part = Vec::with_capacity(tw * s * th * s * 3);
            for yy in 0..th * s {
                let row = (by + yy) * pw * s;
                part.extend_from_slice(&big[(row + bx) * 3..(row + bx + tw * s) * 3]);
            }
            (x * s, y * s, tw * s, th * s, part)
        })
        .collect();

    let mut out = vec![0f32; ow * oh * 3];
    for (x, y, tw, th, part) in done {
        for yy in 0..th {
            let d = ((y + yy) * ow + x) * 3;
            out[d..d + tw * 3].copy_from_slice(&part[yy * tw * 3..(yy + 1) * tw * 3]);
        }
    }
    let rgb = ImageBuffer::<Rgb<f32>, _>::from_raw(ow as u32, oh as u32, out)
        .ok_or("internal size error")?;
    let rgb = DynamicImage::ImageRgb32F(rgb);

    let deep = matches!(
        image.color(),
        image::ColorType::L16
            | image::ColorType::La16
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
            | image::ColorType::Rgb32F
            | image::ColorType::Rgba32F
    );
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        let alpha = image::GrayImage::from_fn(w as u32, h as u32, |x, y| {
            image::Luma([rgba.get_pixel(x, y)[3]])
        });
        let alpha = crate::resize::resize_with(
            &DynamicImage::ImageLuma8(alpha),
            ow as u32,
            oh as u32,
            crate::resize::FilterType::Lanczos3,
        )
        .to_luma8();
        let rgb8 = rgb.to_rgb8();
        let out = ImageBuffer::<Rgba<u8>, _>::from_fn(ow as u32, oh as u32, |x, y| {
            let p = rgb8.get_pixel(x, y);
            Rgba([p[0], p[1], p[2], alpha.get_pixel(x, y)[0]])
        });
        return Ok(DynamicImage::ImageRgba8(out));
    }
    Ok(if deep {
        DynamicImage::ImageRgb16(rgb.to_rgb16())
    } else {
        DynamicImage::ImageRgb8(rgb.to_rgb8())
    })
}

// ----- reading the model files --------------------------------------------

fn u16_at(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
        .ok_or_else(|| "model file is truncated".to_string())
}

fn u32_at(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| "model file is truncated".to_string())
}

/// The entries of an uncompressed zip (PyTorch's file format), by name.
fn zip_entries(b: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    let bad = || "model file is not a PyTorch archive".to_string();
    let eocd = (0..b.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| b[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(bad)?;
    let count = u16_at(b, eocd + 10)?;
    let mut p = u32_at(b, eocd + 16)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if b.get(p..p + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(bad());
        }
        let method = u16_at(b, p + 10)?;
        let size = u32_at(b, p + 20)?;
        let name_len = u16_at(b, p + 28)?;
        let extra_len = u16_at(b, p + 30)?;
        let comment_len = u16_at(b, p + 32)?;
        let local = u32_at(b, p + 42)?;
        let name = b.get(p + 46..p + 46 + name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).into_owned();
        p += 46 + name_len + extra_len + comment_len;
        if method != 0 {
            return Err(bad());
        }
        if b.get(local..local + 4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
            return Err(bad());
        }
        let start = local + 30 + u16_at(b, local + 26)? + u16_at(b, local + 28)?;
        let data = b.get(start..start + size).ok_or_else(bad)?;
        out.push((name, data));
    }
    Ok(out)
}

// ----- SHA-256 ----------------------------------------------------------------

fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

/// SHA-256 (FIPS 180-4) as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(v);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_values() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            sha256_hex(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    /// A net whose last layer is all zeros gives plain nearest-neighbour 4x.
    #[test]
    fn zero_net_is_nearest_neighbour() {
        let conv = |cin, cout, prelu: bool| Conv {
            cin,
            cout,
            w: vec![0.01; 9 * cin * cout],
            bias: vec![0.0; cout],
            prelu: if prelu { vec![0.1; cout] } else { Vec::new() },
        };
        let mut convs: Vec<Conv> = (0..CONVS - 1)
            .map(|j| conv(if j == 0 { 3 } else { FEAT }, FEAT, true))
            .collect();
        let mut last = conv(FEAT, 48, false);
        last.w.iter_mut().for_each(|v| *v = 0.0);
        convs.push(last);
        let net = Net { convs };
        let rgb: Vec<f32> = (0..5 * 3 * 3).map(|i| (i % 7) as f32 / 7.0).collect();
        let out = run_tile(&net, &rgb, 5, 3);
        assert_eq!(out.len(), 20 * 12 * 3);
        for oy in 0..12 {
            for ox in 0..20 {
                for c in 0..3 {
                    let want = rgb[((oy / 4) * 5 + ox / 4) * 3 + c];
                    assert!((out[(oy * 20 + ox) * 3 + c] - want).abs() < 1e-6);
                }
            }
        }
    }
}

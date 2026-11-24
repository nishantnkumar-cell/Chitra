//! Optional AI upscaling with the Real-ESRGAN models (BSD-3-Clause, by
//! Xintao Wang et al.).
//!
//! The model files are not part of Chitra: each is downloaded on first use
//! into the config folder, or picked by hand, and only ever accepted when its
//! SHA-256 matches the published file. The networks run in plain Rust, on
//! the graphics card when one is available (see `gpu`) and otherwise on the
//! processor, where the convolutions are matrix products (`conv`). The photo
//! is processed in overlapping tiles.

mod compact;
mod conv;
#[cfg(feature = "gpu")]
mod gpu;
mod pth;
mod rrdb;
mod sha;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use image::{DynamicImage, ImageBuffer, Rgb, Rgba};
use rayon::prelude::*;

pub use sha::{file_sha256, sha256_hex};

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

pub const X4PLUS: ModelFile = ModelFile {
    name: "RealESRGAN_x4plus.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.0/RealESRGAN_x4plus.pth",
    sha256: "4fa0d38905f75ac06eb49a7951b426670021be3018265fd191d2125df9d682f1",
    bytes: 67_040_989,
};

pub const X2PLUS: ModelFile = ModelFile {
    name: "RealESRGAN_x2plus.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.1/RealESRGAN_x2plus.pth",
    sha256: "49fafd45f8fd7aa8d31ab2a22d14d91b536c34494a5cfe31eb5d89c2fa266abb",
    bytes: 67_061_725,
};

pub const NET_X4PLUS: ModelFile = ModelFile {
    name: "RealESRNet_x4plus.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.1.1/RealESRNet_x4plus.pth",
    sha256: "a820b9bde89a874d7599d545567308ce6c128fc8754a53208eda016d40aa81df",
    bytes: 67_040_989,
};

pub const ANIME: ModelFile = ModelFile {
    name: "RealESRGAN_x4plus_anime_6B.pth",
    url: "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.2.4/RealESRGAN_x4plus_anime_6B.pth",
    sha256: "f872d837d3c90ed2e05227bed711af5671a6fd1c9f7d7e91c911a61f155e99da",
    bytes: 17_938_799,
};

/// Every file any model uses, for recognising hand-downloaded copies.
const ALL_FILES: [&ModelFile; 6] = [
    &GENERAL,
    &WEAK_DENOISE,
    &X4PLUS,
    &X2PLUS,
    &NET_X4PLUS,
    &ANIME,
];

/// The AI models Chitra can use. Each is a separate, optional download.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Model {
    /// realesr-general-x4v3 (+ its weak-denoise twin): small and quick.
    #[default]
    Fast,
    /// RealESRGAN_x4plus: the full model, best detail, about 15x slower.
    Best,
    /// RealESRGAN_x2plus: the full model trained for 2x, four times quicker
    /// than Best for 2x results.
    Best2x,
    /// RealESRNet_x4plus: the full model before its sharpening (GAN)
    /// training, so it invents less texture and looks softer.
    Faithful,
    /// RealESRGAN_x4plus_anime_6B: a smaller full model for drawings,
    /// cartoons and anime.
    Drawings,
}

impl Model {
    pub const ALL: [Model; 5] = [
        Model::Fast,
        Model::Best,
        Model::Best2x,
        Model::Faithful,
        Model::Drawings,
    ];

    /// The name used on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Model::Fast => "fast",
            Model::Best => "best",
            Model::Best2x => "best-2x",
            Model::Faithful => "faithful",
            Model::Drawings => "drawings",
        }
    }

    pub fn from_id(id: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|m| m.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Fast => "Fast",
            Model::Best => "Best",
            Model::Best2x => "Best 2x",
            Model::Faithful => "Faithful",
            Model::Drawings => "Drawings",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Model::Fast => "Quick general model; removes noise and JPEG artefacts.",
            Model::Best => "Most detail, especially faces and small photos. Much slower.",
            Model::Best2x => "The Best model trained for 2x: quicker than Best for 2x results.",
            Model::Faithful => "Invents less texture than Best; softer, closer to the photo.",
            Model::Drawings => "For drawings, cartoons and anime: clean lines and flat colour.",
        }
    }

    /// How much the model enlarges; other sizes are resampled from it.
    pub fn scale(self) -> u32 {
        match self {
            Model::Best2x => 2,
            _ => 4,
        }
    }

    pub fn files(self) -> &'static [&'static ModelFile] {
        match self {
            Model::Fast => &[&GENERAL, &WEAK_DENOISE],
            Model::Best => &[&X4PLUS],
            Model::Best2x => &[&X2PLUS],
            Model::Faithful => &[&NET_X4PLUS],
            Model::Drawings => &[&ANIME],
        }
    }

    pub fn download_bytes(self) -> u64 {
        self.files().iter().map(|f| f.bytes).sum()
    }

    /// Whether the noise-removal strength does anything.
    pub fn has_noise_control(self) -> bool {
        self == Model::Fast
    }

    /// Input pixels per tile side, and the overlap read around each tile.
    /// The large models keep tiles small to bound memory.
    fn tiling(self) -> (usize, usize) {
        match self {
            Model::Fast => (256, 20),
            _ => (128, 20),
        }
    }

    /// How far beyond an area the model reads, in input pixels, so a crop
    /// with this margin gives the same result inside as the whole photo.
    pub fn reach(self) -> u32 {
        self.tiling().1 as u32
    }
}

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

/// True when the model's files are in place (their contents are checked on load).
pub fn installed(model: Model) -> bool {
    missing(model).is_empty()
}

/// The model's files that are not in place yet.
pub fn missing(model: Model) -> Vec<&'static ModelFile> {
    let dir = models_dir();
    model
        .files()
        .iter()
        .copied()
        .filter(|f| !dir.as_ref().is_some_and(|d| d.join(f.name).is_file()))
        .collect()
}

/// Download a model's files with the system's `curl` (part of Windows 10 and
/// later, macOS and most Linux desktops), checking each one's SHA-256.
/// Chitra itself has no network code.
pub fn download(model: Model) -> Result<(), String> {
    let dir = models_dir().ok_or("no config folder")?;
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for f in model.files() {
        let dest = dir.join(f.name);
        if dest.is_file() && file_sha256(&dest).ok().as_deref() == Some(f.sha256) {
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let status = crate::net::curl()
            .arg("-o")
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
        let Some(f) = ALL_FILES.iter().find(|f| f.sha256 == sum) else {
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

/// Parses `bytes` as model weights and asks for a few tensors, for fuzzing.
#[doc(hidden)]
pub fn fuzz_pth(bytes: &[u8]) {
    if let Ok(w) = pth::Weights::parse("fuzz", bytes) {
        let _ = w.has("conv_first.weight");
        let _ = w.shape("conv_first.weight");
        let _ = w.get("conv_first.weight", &[64, 3, 3, 3]);
    }
}

// ----- where to run -------------------------------------------------------------

/// Where the AI runs: the best graphics card if there is one, the processor,
/// or one named graphics card. Saved in the settings as [`Compute::key`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Compute {
    #[default]
    Auto,
    Processor,
    Adapter(String),
}

impl Compute {
    /// The text kept in the settings file.
    pub fn key(&self) -> String {
        match self {
            Compute::Auto => "auto".into(),
            Compute::Processor => "cpu".into(),
            Compute::Adapter(n) => format!("gpu:{n}"),
        }
    }

    /// Reads [`Compute::key`]; anything else means Automatic.
    pub fn from_key(s: &str) -> Compute {
        match s.trim() {
            "cpu" => Compute::Processor,
            t => match t.strip_prefix("gpu:") {
                Some(n) if !n.is_empty() => Compute::Adapter(n.to_string()),
                _ => Compute::Auto,
            },
        }
    }
}

static COMPUTE: Mutex<Compute> = Mutex::new(Compute::Auto);

/// The choice in force.
pub fn compute() -> Compute {
    COMPUTE.lock().map(|c| c.clone()).unwrap_or_default()
}

/// Choose where the AI runs; takes effect with the next run.
pub fn set_compute(c: Compute) {
    if let Ok(mut g) = COMPUTE.lock() {
        *g = c;
    }
}

/// One graphics card (or software renderer) the system offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterEntry {
    pub name: String,
    /// "discrete", "integrated", "virtual", "software" or "other".
    pub kind: String,
    /// "Vulkan", "Dx12" or "Metal".
    pub backend: String,
}

/// What this computer can run the AI on.
#[derive(Clone, Debug, Default)]
pub struct ComputeReport {
    /// This build of Chitra can use a graphics card at all.
    pub gpu_built: bool,
    pub adapters: Vec<AdapterEntry>,
    /// The card Automatic uses; `None` means the processor.
    pub auto_pick: Option<String>,
    /// Why the graphics card is not in use, if it is not.
    pub why_not: String,
}

static REPORT: Mutex<Option<ComputeReport>> = Mutex::new(None);

/// The cards found (looked up once; `rescan` looks again, for example after
/// a driver install) and why the card is not used, if it is not.
pub fn compute_report(rescan: bool) -> ComputeReport {
    #[cfg(feature = "gpu")]
    {
        let mut slot = REPORT.lock().unwrap_or_else(|e| e.into_inner());
        if rescan {
            gpu::reset();
            *slot = None;
        }
        let mut r = slot
            .get_or_insert_with(|| {
                let (adapters, auto_pick) = gpu::adapters();
                ComputeReport {
                    gpu_built: true,
                    adapters,
                    auto_pick,
                    why_not: String::new(),
                }
            })
            .clone();
        r.why_not = if std::env::var_os("CHITRA_AI_CPU").is_some() {
            "the CHITRA_AI_CPU setting keeps the AI on the processor".into()
        } else {
            gpu::why_not()
        };
        r
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (rescan, &REPORT);
        ComputeReport {
            why_not: "this build of Chitra has no graphics card support".into(),
            ..Default::default()
        }
    }
}

/// Where the last run went: "graphics card (name)" or "processor".
pub fn last_device() -> String {
    LAST_DEVICE.lock().map(|d| d.clone()).unwrap_or_default()
}

static LAST_DEVICE: Mutex<String> = Mutex::new(String::new());

// ----- loaded networks ----------------------------------------------------------

enum Engine {
    Compact {
        general: compact::Net,
        weak: compact::Net,
        /// The last blend made, by noise-removal strength in hundredths.
        blended: Option<(u32, Arc<compact::Net>)>,
    },
    Rrdb(Arc<rrdb::Net>),
}

#[derive(Clone)]
enum Ready {
    Compact(Arc<compact::Net>),
    Rrdb(Arc<rrdb::Net>),
}

/// Only one model is kept in memory: the large ones are 64 MB each.
static LOADED: Mutex<Option<(Model, Engine)>> = Mutex::new(None);

fn forget_loaded() {
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    #[cfg(feature = "gpu")]
    gpu::forget();
}

fn not_installed(model: Model) -> String {
    format!(
        "the AI model '{}' is not installed: choose Download in the Upscale and sharpen \
         window, or run chitra-cli ai-model download {}",
        model.label(),
        model.id()
    )
}

fn read_weights(model: Model, file: &ModelFile) -> Result<Vec<u8>, String> {
    let dir = models_dir().ok_or("no config folder")?;
    let bytes = fs::read(dir.join(file.name)).map_err(|_| not_installed(model))?;
    if sha256_hex(&bytes) != file.sha256 {
        return Err(format!("{} is damaged; download it again", file.name));
    }
    Ok(bytes)
}

fn load(model: Model) -> Result<Engine, String> {
    let files = model.files();
    if model == Model::Fast {
        let a = read_weights(model, files[0])?;
        let b = read_weights(model, files[1])?;
        return Ok(Engine::Compact {
            general: compact::Net::load(&pth::Weights::parse(files[0].name, &a)?)?,
            weak: compact::Net::load(&pth::Weights::parse(files[1].name, &b)?)?,
            blended: None,
        });
    }
    let bytes = read_weights(model, files[0])?;
    let net = rrdb::Net::load(&pth::Weights::parse(files[0].name, &bytes)?)?;
    if net.scale() as u32 != model.scale() {
        return Err(format!("{} has the wrong scale", files[0].name));
    }
    Ok(Engine::Rrdb(Arc::new(net)))
}

/// The network for `model`, loading it if needed; `noise` blends the Fast pair.
fn ready(model: Model, noise: f32) -> Result<Ready, String> {
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_none_or(|(m, _)| *m != model) {
        *guard = None;
        *guard = Some((model, load(model)?));
    }
    let Some((_, engine)) = guard.as_mut() else {
        return Err("model not loaded".into());
    };
    Ok(match engine {
        Engine::Rrdb(n) => Ready::Rrdb(n.clone()),
        Engine::Compact {
            general,
            weak,
            blended,
        } => {
            let key = (noise.clamp(0.0, 1.0) * 100.0).round() as u32;
            match blended {
                Some((k, n)) if *k == key => Ready::Compact(n.clone()),
                _ => {
                    let n = Arc::new(compact::Net::blend(general, weak, key as f32 / 100.0));
                    *blended = Some((key, n.clone()));
                    Ready::Compact(n)
                }
            }
        }
    })
}

impl Ready {
    fn scale(&self) -> usize {
        match self {
            Ready::Compact(_) => 4,
            Ready::Rrdb(n) => n.scale(),
        }
    }

    /// Run on one tile on the processor.
    fn run_cpu(&self, rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
        match self {
            Ready::Compact(n) => n.run(rgb, w, h),
            Ready::Rrdb(n) => n.run(rgb, w, h),
        }
    }
}

// ----- upscaling ----------------------------------------------------------------

/// Enlarge by the model's own scale (see `Model::scale`). `noise` 0 keeps
/// the photo's noise and 1 removes it; only the Fast model uses it. Alpha is
/// enlarged separately.
pub fn upscale(image: &DynamicImage, model: Model, noise: f32) -> Result<DynamicImage, String> {
    if !installed(model) {
        return Err(not_installed(model));
    }
    let net = ready(model, noise)?;
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.to_rgb32f();
    let s = net.scale();
    let (tile, pad) = tile_override().unwrap_or(model.tiling());

    #[cfg(feature = "gpu")]
    let gpu_out = if std::env::var_os("CHITRA_AI_CPU").is_none() && compute() != Compute::Processor
    {
        // wgpu panics on some driver failures (a lost device, for one);
        // treat that like any other GPU failure and use the processor.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gpu::upscale(&net, src.as_raw(), w, h, tile_override().map(|t| t.0), pad)
        }))
        .unwrap_or_else(|_| {
            gpu::give_up();
            None
        })
    } else {
        None
    };
    #[cfg(not(feature = "gpu"))]
    let gpu_out: Option<(Vec<f32>, String)> = None;

    let out = match gpu_out {
        Some((out, name)) => {
            set_device(format!("graphics card ({name})"));
            out
        }
        None => {
            set_device("processor".into());
            tiled(src.as_raw(), w, h, s, tile, pad, model, |crop, pw, ph| {
                net.run_cpu(crop, pw, ph)
            })
        }
    };
    let (ow, oh) = (w * s, h * s);
    let rgb = ImageBuffer::<Rgb<f32>, _>::from_raw(ow as u32, oh as u32, out)
        .ok_or("internal size error")?;
    finish(image, DynamicImage::ImageRgb32F(rgb))
}

fn set_device(d: String) {
    if let Ok(mut g) = LAST_DEVICE.lock() {
        *g = d;
    }
}

/// `CHITRA_AI_TILE=tile,pad` (for tests and tuning).
fn tile_override() -> Option<(usize, usize)> {
    let v = std::env::var("CHITRA_AI_TILE").ok()?;
    let (t, p) = v.split_once(',')?;
    Some((t.parse::<usize>().ok()?.max(16), p.parse().ok()?))
}

/// One tile of the work: its place in the input, and the padded area read.
pub(crate) struct Tile {
    /// The tile, in input pixels.
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// The area read around it (clipped to the photo, widened to a multiple
    /// of `fold` when the model folds its input).
    pub x0: usize,
    pub y0: usize,
    pub pw: usize,
    pub ph: usize,
}

pub(crate) fn tiles(w: usize, h: usize, tile: usize, pad: usize, fold: usize) -> Vec<Tile> {
    let mut out = Vec::new();
    for ty in 0..h.div_ceil(tile) {
        for tx in 0..w.div_ceil(tile) {
            let (x, y) = (tx * tile, ty * tile);
            let (tw, th) = (tile.min(w - x), tile.min(h - y));
            let x0 = x.saturating_sub(pad);
            let y0 = y.saturating_sub(pad);
            let pw = ((x + tw + pad).min(w) - x0).next_multiple_of(fold);
            let ph = ((y + th + pad).min(h) - y0).next_multiple_of(fold);
            out.push(Tile {
                x,
                y,
                w: tw,
                h: th,
                x0,
                y0,
                pw,
                ph,
            });
        }
    }
    out
}

/// The padded area of a tile as RGB, repeating the last row and column
/// where it reaches past the photo (only for folding models).
pub(crate) fn crop(src: &[f32], w: usize, h: usize, t: &Tile) -> Vec<f32> {
    let mut out = Vec::with_capacity(t.pw * t.ph * 3);
    for yy in 0..t.ph {
        let sy = (t.y0 + yy).min(h - 1);
        for xx in 0..t.pw {
            let sx = (t.x0 + xx).min(w - 1);
            out.extend_from_slice(&src[(sy * w + sx) * 3..][..3]);
        }
    }
    out
}

/// Copy a tile's own area out of its enlarged padded result into `out`.
pub(crate) fn place(out: &mut [f32], ow: usize, s: usize, t: &Tile, big: &[f32]) {
    let (bx, by, bw) = ((t.x - t.x0) * s, (t.y - t.y0) * s, t.pw * s);
    for yy in 0..t.h * s {
        let src = ((by + yy) * bw + bx) * 3;
        let dst = ((t.y * s + yy) * ow + t.x * s) * 3;
        out[dst..dst + t.w * s * 3].copy_from_slice(&big[src..src + t.w * s * 3]);
    }
}

#[allow(clippy::too_many_arguments)]
fn tiled(
    src: &[f32],
    w: usize,
    h: usize,
    s: usize,
    tile: usize,
    pad: usize,
    model: Model,
    run: impl Fn(&[f32], usize, usize) -> Vec<f32> + Sync,
) -> Vec<f32> {
    let fold = if model == Model::Best2x { 2 } else { 1 };
    let list = tiles(w, h, tile, pad, fold);
    let one = |t: &Tile| run(&crop(src, w, h, t), t.pw, t.ph);
    let (ow, oh) = (w * s, h * s);
    let mut out = vec![0f32; ow * oh * 3];
    // A large model's tile needs about 300 MB while it runs, so only a few
    // run at once.
    let at_once = if model == Model::Fast { usize::MAX } else { 6 };
    for chunk in list.chunks(at_once.min(list.len().max(1))) {
        let done: Vec<Vec<f32>> = chunk.par_iter().map(one).collect();
        for (t, big) in chunk.iter().zip(done) {
            place(&mut out, ow, s, t, &big);
        }
    }
    out
}

/// The RGB result in the source's depth, with its alpha enlarged by Lanczos3.
fn finish(image: &DynamicImage, rgb: DynamicImage) -> Result<DynamicImage, String> {
    let (ow, oh) = (rgb.width(), rgb.height());
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
        let alpha = image::GrayImage::from_fn(image.width(), image.height(), |x, y| {
            image::Luma([rgba.get_pixel(x, y)[3]])
        });
        let alpha = crate::resize::resize_with(
            &DynamicImage::ImageLuma8(alpha),
            ow,
            oh,
            crate::resize::FilterType::Lanczos3,
        )
        .to_luma8();
        let rgb8 = rgb.to_rgb8();
        let out = ImageBuffer::<Rgba<u8>, _>::from_fn(ow, oh, |x, y| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_choice_round_trips() {
        for c in [
            Compute::Auto,
            Compute::Processor,
            Compute::Adapter("NVIDIA GeForce RTX 4070 Ti".into()),
        ] {
            assert_eq!(Compute::from_key(&c.key()), c);
        }
        // Anything unknown or empty means Automatic.
        assert_eq!(Compute::from_key(""), Compute::Auto);
        assert_eq!(Compute::from_key("gpu:"), Compute::Auto);
        assert_eq!(Compute::from_key("tpu"), Compute::Auto);
    }

    #[test]
    fn model_ids_round_trip() {
        for m in Model::ALL {
            assert_eq!(Model::from_id(m.id()), Some(m));
            assert!(!m.files().is_empty());
        }
        assert_eq!(Model::from_id("nope"), None);
    }

    #[test]
    fn tiles_cover_the_photo_once() {
        for fold in [1, 2] {
            let (w, h) = (301, 157);
            let list = tiles(w, h, 64, 10, fold);
            let mut hits = vec![0u8; w * h];
            for t in &list {
                assert!(t.pw % fold == 0 && t.ph % fold == 0);
                assert!(t.x0 <= t.x && t.y0 <= t.y);
                assert!(t.x0 + t.pw >= t.x + t.w && t.y0 + t.ph >= t.y + t.h);
                for y in t.y..t.y + t.h {
                    for x in t.x..t.x + t.w {
                        hits[y * w + x] += 1;
                    }
                }
            }
            assert!(hits.iter().all(|&n| n == 1));
        }
    }

    #[test]
    fn tiling_with_identity_matches_the_source() {
        let (w, h) = (37, 23);
        let src: Vec<f32> = (0..w * h * 3).map(|i| (i % 13) as f32 / 13.0).collect();
        // A 1x "model" that returns its crop: the tiles must reassemble it.
        let out = tiled(&src, w, h, 1, 8, 3, Model::Best2x, |c, _, _| c.to_vec());
        assert_eq!(out, src);
    }
}

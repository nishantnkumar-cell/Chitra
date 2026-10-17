//! Batch conversion: plan output names, then decode, resize and encode in
//! parallel, collecting a per-file report.

use std::collections::HashSet;
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::decode::{DecodeOptions, decode_file};
use crate::encode::{EncodeError, EncodeOptions, JpegRepack, encode, supports_exif};
use crate::folder::natural_cmp;
use crate::format::{Format, is_supported_path};
use crate::metadata;
use crate::resize::{ResizeSpec, resize, target_size};

/// What to do when the output file already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Pick a free name: "photo_1.webp", "photo_2.webp", ...
    #[default]
    Rename,
    Skip,
    Overwrite,
}

impl ConflictPolicy {
    pub const ALL: [ConflictPolicy; 3] = [
        ConflictPolicy::Rename,
        ConflictPolicy::Skip,
        ConflictPolicy::Overwrite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConflictPolicy::Rename => "Add a number",
            ConflictPolicy::Skip => "Skip",
            ConflictPolicy::Overwrite => "Overwrite",
        }
    }
}

impl FromStr for ConflictPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rename" => Ok(ConflictPolicy::Rename),
            "skip" => Ok(ConflictPolicy::Skip),
            "overwrite" => Ok(ConflictPolicy::Overwrite),
            other => Err(format!(
                "unknown conflict policy '{other}' (use rename, skip or overwrite)"
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub format: Format,
    pub quality: u8,
    /// Lossless WebP / JPEG XL even when lossy encoders are available.
    pub lossless: bool,
    /// JPEG to JPEG XL: repack the JPEG data losslessly (smaller than the JPEG,
    /// no quality loss) instead of re-encoding the pixels. Not used when resizing.
    pub jpeg_repack: bool,
    pub resize: Option<ResizeSpec>,
    pub upscale: bool,
    /// Output folder; `None` writes next to each input.
    pub out_dir: Option<PathBuf>,
    /// Name pattern without extension. Tokens: {name} {n} {n:03} {parent} {ext}.
    pub rename: String,
    pub conflict: ConflictPolicy,
    /// Copy EXIF and ICC profile into the output where the format allows.
    pub keep_metadata: bool,
    /// Keep all frames when both input and output can animate (GIF output).
    pub keep_animation: bool,
    pub recursive: bool,
    /// Worker threads; 0 means one per CPU core.
    pub jobs: usize,
}

impl ConvertOptions {
    pub fn new(format: Format) -> Self {
        ConvertOptions {
            format,
            quality: 90,
            lossless: false,
            jpeg_repack: true,
            resize: None,
            upscale: false,
            out_dir: None,
            rename: "{name}".into(),
            conflict: ConflictPolicy::Rename,
            keep_metadata: true,
            keep_animation: true,
            recursive: false,
            jobs: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Set when planning already decided not to convert this file.
    pub skip: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Converted {
        width: u32,
        height: u32,
        in_bytes: u64,
        out_bytes: u64,
    },
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct JobResult {
    pub job: Job,
    pub outcome: Outcome,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub results: Vec<JobResult>,
    pub elapsed: Duration,
}

impl Report {
    pub fn converted(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Converted { .. }))
            .count()
    }
    pub fn skipped(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Skipped(_)))
            .count()
    }
    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Failed(_)))
            .count()
    }
    pub fn bytes(&self) -> (u64, u64) {
        self.results
            .iter()
            .fold((0, 0), |(i, o), r| match r.outcome {
                Outcome::Converted {
                    in_bytes,
                    out_bytes,
                    ..
                } => (i + in_bytes, o + out_bytes),
                _ => (i, o),
            })
    }
}

/// One input file and the sub-folder (relative to its root) it came from, so
/// recursive conversions can mirror the folder structure.
#[derive(Clone, Debug)]
struct Source {
    path: PathBuf,
    rel_dir: PathBuf,
}

fn gather(roots: &[PathBuf], recursive: bool) -> Vec<Source> {
    let mut out = Vec::new();
    for root in roots {
        if root.is_dir() {
            gather_dir(root, root, recursive, &mut out);
        } else {
            out.push(Source {
                path: root.clone(),
                rel_dir: PathBuf::new(),
            });
        }
    }
    out
}

fn gather_dir(root: &Path, dir: &Path, recursive: bool, out: &mut Vec<Source>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            dirs.push(path);
        } else if kind.is_file() && is_supported_path(&path) {
            files.push(path);
        }
    }
    files.sort_by(|a, b| natural_cmp(a, b));
    let rel_dir = dir
        .strip_prefix(root)
        .unwrap_or(Path::new(""))
        .to_path_buf();
    out.extend(files.into_iter().map(|path| Source {
        path,
        rel_dir: rel_dir.clone(),
    }));
    if recursive {
        dirs.sort_by(|a, b| natural_cmp(a, b));
        for d in dirs {
            gather_dir(root, &d, true, out);
        }
    }
}

/// Fill in a rename pattern. Unknown tokens are left as written.
pub fn render_name(pattern: &str, input: &Path, n: usize, format: Format) -> String {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = input
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = &after[..end];
        let (name, spec) = token.split_once(':').unwrap_or((token, ""));
        match name {
            "name" => out.push_str(&stem),
            "parent" => out.push_str(&parent),
            "ext" => out.push_str(format.extension()),
            "n" => {
                let width = spec.trim_start_matches('0').parse::<usize>().unwrap_or(0);
                out.push_str(&format!("{n:0width$}"));
            }
            _ => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    // A file name must not escape its folder or contain characters Windows rejects.
    let cleaned: String = out
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() { stem } else { cleaned }
}

/// Case-insensitive key so "A.webp" and "a.webp" collide, as they do on Windows and macOS.
fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Decide every output path up front so names are deterministic and two inputs
/// never write the same file.
pub fn plan(roots: &[PathBuf], opts: &ConvertOptions) -> Vec<Job> {
    let sources = gather(roots, opts.recursive);
    let mut taken: HashSet<String> = HashSet::new();
    let ext = opts.format.extension();
    let mut jobs = Vec::with_capacity(sources.len());

    for (i, src) in sources.iter().enumerate() {
        let dir = match &opts.out_dir {
            Some(out) => out.join(&src.rel_dir),
            None => src.path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let base = render_name(&opts.rename, &src.path, i + 1, opts.format);
        let first = dir.join(format!("{base}.{ext}"));

        let exists = |p: &Path| p.exists() || key(p) == key(&src.path);
        let mut output = first.clone();
        let mut skip = None;
        let clash_in_batch = taken.contains(&key(&first));
        if clash_in_batch || exists(&first) {
            let rename = clash_in_batch || opts.conflict == ConflictPolicy::Rename;
            if rename {
                let mut k = 1;
                loop {
                    let candidate = dir.join(format!("{base}_{k}.{ext}"));
                    if !taken.contains(&key(&candidate)) && !exists(&candidate) {
                        output = candidate;
                        break;
                    }
                    k += 1;
                }
            } else if opts.conflict == ConflictPolicy::Skip {
                skip = Some("output already exists".to_string());
            }
        }
        taken.insert(key(&output));
        jobs.push(Job {
            input: src.path.clone(),
            output,
            skip,
        });
    }
    jobs
}

/// Convert every job in parallel. `progress` is called once per finished job,
/// from worker threads.
pub fn run<F>(jobs: Vec<Job>, opts: &ConvertOptions, progress: F) -> Report
where
    F: Fn(&JobResult) + Sync,
{
    let start = Instant::now();
    let threads = if opts.jobs == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        opts.jobs
    };
    let work = || {
        jobs.into_par_iter()
            .map(|job| {
                let t = Instant::now();
                let outcome = match &job.skip {
                    Some(reason) => Outcome::Skipped(reason.clone()),
                    None => convert_one(&job, opts).unwrap_or_else(Outcome::Failed),
                };
                let result = JobResult {
                    job,
                    outcome,
                    elapsed: t.elapsed(),
                };
                progress(&result);
                result
            })
            .collect::<Vec<_>>()
    };
    let results = match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(work),
        Err(_) => work(),
    };
    Report {
        results,
        elapsed: start.elapsed(),
    }
}

/// Convert a single file. Writes through a temporary file and renames it into
/// place, so a crash or error never leaves a half-written output (or destroys
/// an input being overwritten).
pub fn convert_one(job: &Job, opts: &ConvertOptions) -> Result<Outcome, String> {
    let in_bytes = fs::metadata(&job.input)
        .map(|m| m.len())
        .map_err(|e| format!("cannot read file: {e}"))?;
    if let Some(outcome) = repack_jpeg(job, opts, in_bytes)? {
        return Ok(outcome);
    }
    let animate = opts.keep_animation && opts.format == Format::Gif;
    let mut decoded =
        decode_file(&job.input, DecodeOptions { animation: animate }).map_err(|e| e.to_string())?;

    if let Some(spec) = opts.resize {
        let (w, h) = target_size(
            decoded.image.width(),
            decoded.image.height(),
            spec,
            opts.upscale,
        );
        if (w, h) != (decoded.image.width(), decoded.image.height()) {
            decoded.image = resize(&decoded.image, w, h);
            for frame in &mut decoded.frames {
                let img = image::DynamicImage::ImageRgba8(std::mem::take(&mut frame.image));
                frame.image = resize(&img, w, h).into_rgba8();
            }
        }
    }

    let mut enc = EncodeOptions::new(opts.format);
    enc.quality = opts.quality;
    enc.lossless = opts.lossless;
    if opts.keep_metadata {
        if supports_exif(opts.format) {
            enc.exif = decoded.exif.as_deref().map(metadata::exif_for_output);
        }
        enc.icc = decoded.icc.clone();
    }

    let out_bytes = write_output(&job.output, |file| {
        encode(&decoded.image, &decoded.frames, &enc, file)
    })?;
    Ok(Outcome::Converted {
        width: decoded.image.width(),
        height: decoded.image.height(),
        in_bytes,
        out_bytes,
    })
}

/// JPEG to JPEG XL without touching the pixels, when the options allow it.
/// `None` means convert the usual way.
fn repack_jpeg(job: &Job, opts: &ConvertOptions, in_bytes: u64) -> Result<Option<Outcome>, String> {
    if opts.format != Format::JpegXl || !opts.jpeg_repack || !crate::encode::JPEG_REPACK {
        return Ok(None);
    }
    if Format::from_path(&job.input) != Some(Format::Jpeg) {
        return Ok(None);
    }
    let bytes = fs::read(&job.input).map_err(|e| format!("cannot read file: {e}"))?;
    let Some(jpeg) = JpegRepack::parse(&bytes) else {
        return Ok(None);
    };
    let (width, height) = (jpeg.width(), jpeg.height());
    if let Some(spec) = opts.resize
        && target_size(width, height, spec, opts.upscale) != (width, height)
    {
        return Ok(None);
    }
    let out_bytes = write_output(&job.output, |file| jpeg.encode(opts.keep_metadata, file))?;
    Ok(Some(Outcome::Converted {
        width,
        height,
        in_bytes,
        out_bytes,
    }))
}

/// Write through a temporary file and rename it into place, so a crash or
/// error never leaves a half-written output. Returns the output's size.
fn write_output<F>(output: &Path, write: F) -> Result<u64, String>
where
    F: FnOnce(BufWriter<fs::File>) -> Result<(), EncodeError>,
{
    if let Some(dir) = output.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create folder {}: {e}", dir.display()))?;
    }
    let tmp = temp_path(output);
    let result = (|| -> Result<(), String> {
        let file =
            fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        write(BufWriter::new(file)).map_err(|e| e.to_string())?;
        fs::rename(&tmp, output).map_err(|e| format!("cannot write {}: {e}", output.display()))
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(fs::metadata(output).map(|m| m.len()).unwrap_or(0))
}

fn temp_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!(".{name}.{}.chitra-tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_tokens() {
        let p = Path::new("/photos/trip/IMG_0042.JPG");
        assert_eq!(render_name("{name}", p, 1, Format::WebP), "IMG_0042");
        assert_eq!(
            render_name("{parent}_{n:03}", p, 7, Format::WebP),
            "trip_007"
        );
        assert_eq!(
            render_name("{name}-{n}.{ext}", p, 12, Format::Png),
            "IMG_0042-12.png"
        );
        assert_eq!(render_name("{unknown}", p, 1, Format::Png), "{unknown}");
        assert_eq!(
            render_name("../evil/{name}", p, 1, Format::Png),
            "_evil_IMG_0042"
        );
        assert_eq!(render_name("", p, 1, Format::Png), "IMG_0042");
    }

    #[test]
    fn conflict_policy_parse() {
        assert_eq!(
            "SKIP".parse::<ConflictPolicy>().unwrap(),
            ConflictPolicy::Skip
        );
        assert!("nope".parse::<ConflictPolicy>().is_err());
    }
}

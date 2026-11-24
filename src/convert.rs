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
use crate::enhance::{self, EnhanceOptions};
use crate::folder::{list_subfolders, natural_cmp};
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
    /// Upscale by a whole factor and sharpen, after any `resize`.
    pub enhance: Option<EnhanceOptions>,
    /// Output folder; `None` writes next to each input.
    pub out_dir: Option<PathBuf>,
    /// Name pattern without extension. Tokens: {name} {n} {n:03} {parent} {ext}.
    pub rename: String,
    pub conflict: ConflictPolicy,
    /// Copy EXIF and ICC profile into the output where the format allows.
    pub keep_metadata: bool,
    /// With `keep_metadata`: still remove the GPS location from the EXIF.
    pub strip_location: bool,
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
            enhance: None,
            out_dir: None,
            rename: "{name}".into(),
            conflict: ConflictPolicy::Rename,
            keep_metadata: true,
            strip_location: false,
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
    /// Jobs that were never started because the batch was stopped, in their
    /// original order. Empty when it ran to the end.
    pub remaining: Vec<Job>,
}

impl Report {
    /// This report followed by `later` (the run that carried on from where this
    /// one was stopped): results in order, time added up, and only what `later`
    /// still has left.
    pub fn merged(mut self, later: Report) -> Report {
        self.results.extend(later.results);
        Report {
            results: self.results,
            elapsed: self.elapsed + later.elapsed,
            remaining: later.remaining,
        }
    }

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

fn gather(roots: &[PathBuf], recursive: bool, out_dir: Option<&Path>) -> Vec<Source> {
    // An output folder inside a source folder is left out, so a second run
    // does not convert the first run's results again.
    let skip = out_dir.and_then(|d| fs::canonicalize(d).ok());
    let mut out = Vec::new();
    for root in roots {
        if root.is_dir() {
            gather_dir(root, root, recursive, skip.as_deref(), &mut out);
        } else {
            out.push(Source {
                path: root.clone(),
                rel_dir: PathBuf::new(),
            });
        }
    }
    out
}

fn gather_dir(
    root: &Path,
    dir: &Path,
    recursive: bool,
    skip: Option<&Path>,
    out: &mut Vec<Source>,
) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        let is_file =
            kind.is_file() || (kind.is_symlink() && fs::metadata(&path).is_ok_and(|m| m.is_file()));
        if is_file && is_supported_path(&path) {
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
        // Hidden and system folders are skipped, and links to folders are not
        // followed, so a link back up the tree cannot loop.
        for d in list_subfolders(dir) {
            let is_out = skip.is_some_and(|s| fs::canonicalize(&d).is_ok_and(|c| c == s));
            if !is_out {
                gather_dir(root, &d, true, skip, out);
            }
        }
    }
}

/// How many files `plan` would take from `roots`, without checking outputs.
pub fn count_inputs(roots: &[PathBuf], recursive: bool, out_dir: Option<&Path>) -> usize {
    gather(roots, recursive, out_dir).len()
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
                // A padding width past 20 digits means nothing and could ask for
                // gigabytes of zeros.
                let width = spec
                    .trim_start_matches('0')
                    .parse::<usize>()
                    .unwrap_or(0)
                    .min(20);
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
    if cleaned.is_empty() {
        return stem;
    }
    // Windows reserves these names (with any extension) for devices.
    let base = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit()
            && base.as_bytes()[3] != b'0');
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// Case-insensitive key so "A.webp" and "a.webp" collide, as they do on Windows and macOS.
fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Decide every output path up front so names are deterministic and two inputs
/// never write the same file.
pub fn plan(roots: &[PathBuf], opts: &ConvertOptions) -> Vec<Job> {
    let sources = gather(roots, opts.recursive, opts.out_dir.as_deref());
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

/// What a batch is told to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// No new file starts; the ones being converted finish.
    Paused,
    /// No new file starts, and the batch ends with the rest handed back.
    Stopped,
}

/// Pause, resume or stop a batch that is running (from another thread).
///
/// The gate is before each file: a file being converted always finishes, so no
/// output is half written, and a file is either done or handed back untouched,
/// never both and never lost. A stopped batch returns the jobs it did not start
/// in [`Report::remaining`]; running those continues exactly where it left off.
#[derive(Debug)]
pub struct Control {
    state: std::sync::Mutex<State>,
    changed: std::sync::Condvar,
    active: std::sync::atomic::AtomicUsize,
}

impl Default for Control {
    fn default() -> Self {
        Control::new()
    }
}

impl Control {
    pub fn new() -> Control {
        Control {
            state: std::sync::Mutex::new(State::Running),
            changed: std::sync::Condvar::new(),
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn set(&self, to: State) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // A stopped batch stays stopped.
        if *s != State::Stopped {
            *s = to;
        }
        self.changed.notify_all();
    }

    pub fn pause(&self) {
        self.set(State::Paused);
    }

    pub fn resume(&self) {
        self.set(State::Running);
    }

    pub fn stop(&self) {
        self.set(State::Stopped);
    }

    pub fn state(&self) -> State {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Files being converted right now (they finish even when paused).
    pub fn active(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Waits while paused; false once stopped (the file must not start).
    fn gate(&self) -> bool {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while *s == State::Paused {
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        *s != State::Stopped
    }
}

/// Counts a file as being converted for as long as it lives.
struct Active<'a>(&'a Control);

impl<'a> Active<'a> {
    fn new(c: &'a Control) -> Self {
        c.active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Active(c)
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Convert every job in parallel. `progress` is called once per finished job,
/// from worker threads.
pub fn run<F>(jobs: Vec<Job>, opts: &ConvertOptions, progress: F) -> Report
where
    F: Fn(&JobResult) + Sync,
{
    run_with(jobs, opts, &Control::new(), progress)
}

/// [`run`] that can be paused, resumed and stopped through `control`. When it
/// is stopped, [`Report::remaining`] holds the jobs that were not started, in
/// their original order, for a later `run_with` to carry on from.
pub fn run_with<F>(jobs: Vec<Job>, opts: &ConvertOptions, control: &Control, progress: F) -> Report
where
    F: Fn(&JobResult) + Sync,
{
    let start = Instant::now();
    let threads = if opts.jobs == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        opts.jobs
    };
    let one = |(i, job): (usize, Job)| -> (usize, Result<JobResult, Job>) {
        if !control.gate() {
            return (i, Err(job));
        }
        let _active = Active::new(control);
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
        (i, Ok(result))
    };
    // Upscaled photos are large (a 4x copy is 16 times the pixels) and the
    // upscaler is itself multithreaded, so take them a couple at a time.
    let per_round = match opts.enhance {
        // The AI model runs its tiles on every core already.
        Some(e) if e.ai && opts.jobs == 0 => 1,
        Some(e) if e.scale > 1 && opts.jobs == 0 => 2,
        _ => usize::MAX,
    };
    let work = || {
        let mut results = Vec::with_capacity(jobs.len());
        let mut queue = jobs.into_iter().enumerate().peekable();
        while queue.peek().is_some() {
            let round: Vec<(usize, Job)> = queue.by_ref().take(per_round).collect();
            results.extend(round.into_par_iter().map(one).collect::<Vec<_>>());
        }
        results
    };
    let all = match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.install(work),
        Err(_) => work(),
    };
    let mut done = Vec::new();
    let mut remaining = Vec::new();
    for (_, r) in all {
        match r {
            Ok(result) => done.push(result),
            Err(job) => remaining.push(job),
        }
    }
    Report {
        results: done,
        elapsed: start.elapsed(),
        remaining,
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

    if let Some(e) = &opts.enhance {
        let (w, h) = enhance::output_size(decoded.image.width(), decoded.image.height(), e.scale)?;
        // Animated frames are all upscaled too; check they fit in memory.
        let frames = decoded.frames.len().max(1) as u64;
        if frames > 1 && w as u64 * h as u64 * frames > enhance::MAX_OUTPUT_PIXELS {
            return Err(format!(
                "{frames} frames at {w} x {h} is too large to upscale"
            ));
        }
        decoded.image = enhance::enhance(&decoded.image, e)?;
        for frame in &mut decoded.frames {
            let img = image::DynamicImage::ImageRgba8(std::mem::take(&mut frame.image));
            frame.image = enhance::enhance(&img, e)?.into_rgba8();
        }
    }

    let mut enc = EncodeOptions::new(opts.format);
    enc.quality = opts.quality;
    enc.lossless = opts.lossless;
    if opts.keep_metadata {
        if supports_exif(opts.format) {
            enc.exif = decoded.exif.as_deref().map(|e| {
                let e = metadata::exif_for_output(e);
                if opts.strip_location {
                    metadata::strip_gps(&e)
                } else {
                    e
                }
            });
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
    if opts.format != Format::JpegXl
        || !opts.jpeg_repack
        || !crate::encode::JPEG_REPACK
        || opts.enhance.is_some()
    {
        return Ok(None);
    }
    if Format::from_path(&job.input) != Some(Format::Jpeg) {
        return Ok(None);
    }
    // A lossless repack carries the JPEG's EXIF as it is: to remove the
    // location the photo takes the normal path.
    if opts.strip_location && opts.keep_metadata {
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

    fn png_jobs(dir: &Path, n: usize) -> (ConvertOptions, Vec<Job>) {
        let src = dir.join("in");
        fs::create_dir_all(&src).unwrap();
        for i in 0..n {
            image::RgbImage::from_pixel(8, 8, image::Rgb([i as u8, 1, 2]))
                .save(src.join(format!("p{i:02}.png")))
                .unwrap();
        }
        let mut opts = ConvertOptions::new(Format::Jpeg);
        opts.out_dir = Some(dir.join("out"));
        // One at a time so what has been done when it stops is exact.
        opts.jobs = 1;
        let jobs = plan(&[src], &opts);
        assert_eq!(jobs.len(), n);
        (opts, jobs)
    }

    #[test]
    fn a_stopped_batch_hands_back_exactly_what_it_did_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let (opts, jobs) = png_jobs(dir.path(), 12);
        let all: Vec<PathBuf> = jobs.iter().map(|j| j.output.clone()).collect();
        let control = Control::new();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let first = run_with(jobs, &opts, &control, |_| {
            if done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 5 {
                control.stop();
            }
        });
        // The five finished are done, the other seven are untouched and in order.
        assert_eq!(first.results.len(), 5);
        assert_eq!(first.remaining.len(), 7);
        assert_eq!(first.remaining[0].output, all[5]);
        assert!(all[..5].iter().all(|p| p.exists()));
        assert!(all[5..].iter().all(|p| !p.exists()));

        // Carrying on from there does the rest, once each.
        let second = run_with(first.remaining.clone(), &opts, &Control::new(), |_| {});
        assert!(second.remaining.is_empty());
        let whole = first.merged(second);
        assert_eq!(whole.results.len(), 12);
        assert_eq!(whole.converted(), 12);
        assert!(all.iter().all(|p| p.exists()));
        let inputs: Vec<_> = whole.results.iter().map(|r| r.job.input.clone()).collect();
        let mut sorted = inputs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 12, "no file twice, none missing");
    }

    #[test]
    fn a_paused_batch_starts_nothing_until_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let (opts, jobs) = png_jobs(dir.path(), 6);
        let control = Control::new();
        control.pause();
        let started = std::sync::atomic::AtomicUsize::new(0);
        let report = std::thread::scope(|s| {
            let h = s.spawn(|| {
                run_with(jobs, &opts, &control, |_| {
                    started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
            });
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(control.state(), State::Paused);
            control.resume();
            h.join().unwrap()
        });
        assert_eq!(report.converted(), 6);
        assert!(report.remaining.is_empty());
        // A stop wakes a batch that is paused, and nothing starts.
        let (opts, jobs) = png_jobs(&dir.path().join("again"), 4);
        let control = Control::new();
        control.pause();
        let report = std::thread::scope(|s| {
            let h = s.spawn(|| run_with(jobs, &opts, &control, |_| {}));
            std::thread::sleep(Duration::from_millis(100));
            control.stop();
            h.join().unwrap()
        });
        assert_eq!((report.results.len(), report.remaining.len()), (0, 4));
        // Once stopped it stays stopped.
        control.resume();
        assert_eq!(control.state(), State::Stopped);
    }

    #[test]
    fn rename_pattern_limits() {
        let p = Path::new("/tmp/photo.jpg");
        // A huge padding width is clamped, not allocated.
        let n = render_name("{n:99999999999999}", p, 7, Format::Png);
        assert_eq!(n.len(), 20);
        assert!(n.ends_with('7'));
        // Device names are not used as they are.
        assert_eq!(render_name("con", p, 1, Format::Png), "_con");
        assert_eq!(render_name("NUL.x", p, 1, Format::Png), "_NUL.x");
        assert_eq!(render_name("com1", p, 1, Format::Png), "_com1");
        assert_eq!(render_name("com0", p, 1, Format::Png), "com0");
        assert_eq!(render_name("console", p, 1, Format::Png), "console");
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

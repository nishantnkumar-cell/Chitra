//! Background decoding for the viewer.
//!
//! Requests go into a shared queue served by worker threads. Each request is
//! checked against the "wanted" set before decoding and again before upload,
//! so holding an arrow key skips photos the user has already moved past.
//! Workers turn pixels into GPU textures directly (`egui::Context` is thread
//! safe), so the UI thread never touches raw pixel buffers.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use image::DynamicImage;

use chtra::decode::{DecodeOptions, decode_file};
use chtra::format::Format;
use chtra::metadata;
use chtra::resize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Screen-sized version for normal viewing (animations keep all frames).
    Display,
    /// Full resolution (capped by the GPU texture limit) for zooming in.
    Full,
    /// Small preview for the grid.
    Thumb,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub path: PathBuf,
    pub kind: Kind,
    pub max_side: u32,
}

pub struct LoadedImage {
    /// One texture per frame; a still image has one.
    pub frames: Vec<(TextureHandle, Duration)>,
    /// Size of the texture in pixels.
    pub tex_size: [usize; 2],
    /// Size of the full image in pixels, after EXIF orientation.
    pub full_size: [u32; 2],
    pub format: Format,
    pub color: String,
    pub decode_time: Duration,
    pub exif: Vec<(String, String)>,
    pub has_icc: bool,
}

impl LoadedImage {
    pub fn is_downscaled(&self) -> bool {
        (self.tex_size[0] as u32) < self.full_size[0]
    }
    pub fn total_duration(&self) -> Duration {
        self.frames.iter().map(|f| f.1).sum()
    }
}

pub struct Loaded {
    pub path: PathBuf,
    pub kind: Kind,
    pub result: Result<Arc<LoadedImage>, String>,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Request>,
    wanted: HashSet<(PathBuf, Kind)>,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // A worker that panicked mid-update cannot leave the queue in an
        // invalid state, so a poisoned lock is still safe to use.
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Loader {
    shared: Arc<Shared>,
    rx: Receiver<Loaded>,
}

impl Loader {
    pub fn new(ctx: egui::Context, threads: usize, name: &str) -> Loader {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel();
        for i in 0..threads.max(1) {
            let shared = shared.clone();
            let tx: Sender<Loaded> = tx.clone();
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name(format!("{name}-{i}"))
                .spawn(move || worker(shared, tx, ctx));
        }
        Loader { shared, rx }
    }

    /// Ask for a decode. `urgent` requests jump the queue. Re-requesting
    /// something already wanted is a no-op (except for moving it forward).
    pub fn request(&self, req: Request, urgent: bool) {
        let mut q = self.shared.lock();
        let key = (req.path.clone(), req.kind);
        if q.wanted.contains(&key) {
            if urgent {
                if let Some(pos) = q
                    .items
                    .iter()
                    .position(|r| r.path == req.path && r.kind == req.kind)
                {
                    if let Some(item) = q.items.remove(pos) {
                        q.items.push_front(item);
                    }
                }
            }
            return;
        }
        q.wanted.insert(key);
        if urgent {
            q.items.push_front(req);
        } else {
            q.items.push_back(req);
        }
        drop(q);
        self.shared.ready.notify_one();
    }

    /// Drop every queued or in-flight request for which `keep` returns false.
    pub fn retain(&self, mut keep: impl FnMut(&Path, Kind) -> bool) {
        let mut q = self.shared.lock();
        q.wanted.retain(|(p, k)| keep(p, *k));
        let Queue { items, wanted, .. } = &mut *q;
        items.retain(|r| wanted.contains(&(r.path.clone(), r.kind)));
    }

    /// Mark a result as handled so the same path can be requested again later.
    fn finish(&self, path: &Path, kind: Kind) {
        self.shared
            .lock()
            .wanted
            .remove(&(path.to_path_buf(), kind));
    }

    /// Results that arrived since the last call.
    pub fn poll(&self) -> Vec<Loaded> {
        let out: Vec<Loaded> = self.rx.try_iter().collect();
        for l in &out {
            self.finish(&l.path, l.kind);
        }
        out
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.ready.notify_all();
    }
}

fn still_wanted(shared: &Shared, req: &Request) -> bool {
    shared.lock().wanted.contains(&(req.path.clone(), req.kind))
}

fn worker(shared: Arc<Shared>, tx: Sender<Loaded>, ctx: egui::Context) {
    loop {
        let req = {
            let mut q = shared.lock();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(r) = q.items.pop_front() {
                    break r;
                }
                q = shared.ready.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        if !still_wanted(&shared, &req) {
            continue;
        }
        // `None` means it was cancelled while decoding; `retain` already
        // forgot it, so it can be requested again.
        let Some(result) = load(&req, &ctx, &shared) else {
            continue;
        };
        if tx
            .send(Loaded {
                path: req.path,
                kind: req.kind,
                result,
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

/// Decode and upload. Returns `None` if the request was cancelled meanwhile.
fn load(
    req: &Request,
    ctx: &egui::Context,
    shared: &Shared,
) -> Option<Result<Arc<LoadedImage>, String>> {
    let start = Instant::now();
    let animation = req.kind == Kind::Display;
    let decoded = match decode_file(&req.path, DecodeOptions { animation }) {
        Ok(d) => d,
        Err(e) => return Some(Err(e.to_string())),
    };
    if !still_wanted(shared, req) {
        return None;
    }
    let full_size = [decoded.image.width(), decoded.image.height()];
    let max = req.max_side.max(16);
    let shrink = |img: DynamicImage| -> DynamicImage {
        if img.width() <= max && img.height() <= max {
            img
        } else {
            // Bilinear with an area-scaled kernel: no aliasing, and about
            // twice as fast as Lanczos on a 24 MP photo. The GPU's mipmaps
            // handle any further zooming out.
            resize::thumbnail(&img, max, max)
        }
    };

    let options = TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
    let name = req.path.to_string_lossy();
    let mut frames = Vec::new();
    let mut tex_size = [0, 0];
    if decoded.is_animated() {
        for (i, frame) in decoded.frames.iter().enumerate() {
            let img = shrink(DynamicImage::ImageRgba8(frame.image.clone())).into_rgba8();
            tex_size = [img.width() as usize, img.height() as usize];
            let color = ColorImage::from_rgba_unmultiplied(tex_size, img.as_raw());
            frames.push((
                ctx.load_texture(format!("{name}#{i}"), color, options),
                frame.delay,
            ));
        }
    } else {
        let img = shrink(decoded.image.clone()).into_rgba8();
        tex_size = [img.width() as usize, img.height() as usize];
        let color = ColorImage::from_rgba_unmultiplied(tex_size, img.as_raw());
        frames.push((
            ctx.load_texture(name.into_owned(), color, options),
            Duration::ZERO,
        ));
    }

    let exif = if req.kind == Kind::Thumb {
        Vec::new()
    } else {
        decoded
            .exif
            .as_deref()
            .map(metadata::readable_fields)
            .unwrap_or_default()
    };
    Some(Ok(Arc::new(LoadedImage {
        frames,
        tex_size,
        full_size,
        format: decoded.format,
        color: format!("{:?}", decoded.image.color()),
        decode_time: start.elapsed(),
        exif,
        has_icc: decoded.icc.is_some(),
    })))
}

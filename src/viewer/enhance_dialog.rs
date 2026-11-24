//! The "Upscale and sharpen" window: enlarge a photo 1x to 4x, sharpen it,
//! compare the original and the result side by side, and save this photo or
//! the whole folder as JPEG, JPEG XL or PNG.
//!
//! With the classic method the comparison only enhances the part of the
//! photo on screen, so moving the sliders stays quick even at 4x. The AI
//! method is too slow for that: its preview covers the whole photo and is
//! made only when asked, and moving around it shows the result already made.
//! Saving runs the same code on the whole photo through the batch converter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, Vec2};
use image::DynamicImage;

use chitra::convert::{self, ConflictPolicy, ConvertOptions, Outcome, Report};
use chitra::decode::{self, DecodeOptions};
use chitra::encode;
use chitra::enhance::{self, EnhanceOptions};
use chitra::format::Format;

use super::util::{TreeCount, human_bytes};

/// Output formats offered here.
const FORMATS: [Format; 3] = [Format::Jpeg, Format::JpegXl, Format::Png];
/// Largest side of a preview texture, in output pixels.
const MAX_PREVIEW_SIDE: f32 = 4096.0;
/// Largest preview, in output pixels.
const MAX_PREVIEW_PIXELS: f32 = 16_000_000.0;
const SIDE_W: f32 = 290.0;
const DEFAULT_DENOISE: f32 = 30.0;
/// Largest AI preview of the whole photo, in output pixels; above that the
/// preview covers the part in view.
const AI_PREVIEW_MAX: f64 = 50_000_000.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Current,
    Folder,
}

/// A part of the photo, in original pixels: x, y, width, height.
type Region = (u32, u32, u32, u32);

#[derive(Clone, Copy, PartialEq)]
struct Request {
    region: Region,
    opts: EnhanceOptions,
}

struct Preview {
    req: Request,
    before: TextureHandle,
    /// `None` while the AI method has no result yet (the original still shows).
    after: Option<TextureHandle>,
}

type PreviewResult = Result<(ColorImage, ColorImage), String>;
type Loaded = Result<(PathBuf, Arc<DynamicImage>), String>;

/// An AI preview made on request.
struct AiView {
    opts: EnhanceOptions,
    /// The part of the photo it covers (usually all of it).
    area: Region,
    out: Arc<DynamicImage>,
    seconds: f32,
}

struct AiPending {
    opts: EnhanceOptions,
    area: Region,
    started: Instant,
    rx: Receiver<Result<DynamicImage, String>>,
}

struct Source {
    path: PathBuf,
    full: Arc<DynamicImage>,
}

struct Running {
    total: usize,
    done: Arc<AtomicUsize>,
    report: Arc<Mutex<Option<Report>>>,
}

pub struct EnhanceDialog {
    pub open: bool,
    source: Option<Source>,
    loading: Option<(PathBuf, Receiver<Loaded>)>,
    error: Option<String>,

    scale: u32,
    /// Sharpening in percent.
    amount: f32,
    radius: f32,
    radius_auto: bool,
    threshold: f32,
    /// Noise reduction, 0 to 100.
    denoise: f32,
    /// Enlarge with the AI model.
    ai: bool,
    ai_model: chitra::ai::Model,
    /// AI noise removal, 0 to 100.
    ai_noise: f32,
    /// The AI model files are in place.
    ai_ready: bool,
    /// A model download or import running in the background.
    ai_job: Option<Receiver<Result<(), String>>>,
    ai_msg: Option<String>,

    /// Points on screen per output pixel; `None` until the first frame picks one.
    zoom: Option<f32>,
    /// Centre of the view, in original pixels.
    center: Pos2,
    preview: Option<Preview>,
    pending: Option<(Request, Receiver<PreviewResult>)>,
    preview_error: Option<String>,
    /// AI method: the last preview, and one being made.
    ai_view: Option<AiView>,
    ai_pending: Option<AiPending>,

    scope: Scope,
    /// With "All in folder": take sub-folders too, mirroring them in the output.
    subfolders: bool,
    tree: TreeCount,
    format: Format,
    quality: u8,
    lossless: bool,
    out_dir: Option<PathBuf>,
    rename: String,
    /// The file name follows the scale until the user types their own.
    rename_auto: bool,
    conflict: ConflictPolicy,
    keep_metadata: bool,
    running: Option<Running>,
    last: Option<Report>,
    /// Set when saving finished, so the viewer can rescan its folder.
    pub finished: bool,
}

impl Default for EnhanceDialog {
    fn default() -> Self {
        EnhanceDialog {
            open: false,
            source: None,
            loading: None,
            error: None,
            scale: 2,
            amount: 80.0,
            radius: enhance::default_radius(2),
            radius_auto: true,
            threshold: 3.0,
            denoise: DEFAULT_DENOISE,
            ai: false,
            ai_model: chitra::ai::Model::Fast,
            ai_noise: 50.0,
            ai_ready: false,
            ai_job: None,
            ai_msg: None,
            zoom: None,
            center: Pos2::ZERO,
            preview: None,
            pending: None,
            preview_error: None,
            ai_view: None,
            ai_pending: None,
            scope: Scope::Current,
            subfolders: true,
            tree: TreeCount::default(),
            format: Format::Jpeg,
            quality: 92,
            lossless: false,
            out_dir: None,
            rename: auto_name(2, false),
            rename_auto: true,
            conflict: ConflictPolicy::Rename,
            keep_metadata: true,
            running: None,
            last: None,
            finished: false,
        }
    }
}

fn auto_name(scale: u32, ai: bool) -> String {
    let base = if scale == 1 {
        "{name}_sharp".to_string()
    } else {
        format!("{{name}}_{scale}x")
    };
    if ai { base + "_ai" } else { base }
}

/// Largest preview, in output pixels. The AI models are much slower, so
/// their preview covers less.
fn max_preview_pixels(ai: Option<chitra::ai::Model>) -> f32 {
    match ai {
        None => MAX_PREVIEW_PIXELS,
        Some(chitra::ai::Model::Fast) => 2_500_000.0,
        Some(_) => 600_000.0,
    }
}

impl EnhanceDialog {
    fn opts(&self) -> EnhanceOptions {
        EnhanceOptions {
            scale: self.scale,
            amount: self.amount / 100.0,
            radius: self.radius,
            threshold: self.threshold,
            denoise: self.denoise,
            ai: self.ai,
            ai_model: self.ai_model,
            ai_noise: self.ai_noise / 100.0,
        }
    }

    /// Open the window on `path`, loading it unless it is already loaded.
    pub fn open_on(&mut self, path: Option<PathBuf>) {
        self.open = true;
        self.ai_ready = chitra::ai::installed(self.ai_model);
        self.follow(path);
    }

    /// Load `path` if it is not the photo already shown.
    fn follow(&mut self, path: Option<PathBuf>) {
        let Some(path) = path else { return };
        let loaded = self.source.as_ref().map(|s| &s.path) == Some(&path)
            || self.loading.as_ref().map(|l| &l.0) == Some(&path);
        if loaded {
            return;
        }
        self.error = None;
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let r = decode::decode_file(&p, DecodeOptions::default())
                .map(|d| (p, Arc::new(d.image)))
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.loading = Some((path, rx));
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        current: Option<&PathBuf>,
        dir: Option<&Path>,
        folder: &[PathBuf],
    ) {
        if !self.open {
            return;
        }
        // Follow the viewer to the next photo.
        self.follow(current.cloned());
        self.poll(ctx);
        let mut open = self.open;
        egui::Window::new("Upscale and sharpen")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([1100.0, 680.0])
            .show(ctx, |ui| self.contents(ui, dir, folder));
        self.open = open;
        if !self.open {
            // Free the photo and the preview textures.
            self.source = None;
            self.preview = None;
            self.loading = None;
            self.pending = None;
            self.ai_view = None;
            self.ai_pending = None;
        }
        if self.loading.is_some()
            || self.pending.is_some()
            || self.ai_pending.is_some()
            || self.running.is_some()
            || self.ai_job.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.ai_job {
            match rx.try_recv() {
                Ok(r) => {
                    self.ai_job = None;
                    self.ai_ready = chitra::ai::installed(self.ai_model);
                    // A partly installed model is shown by ai_setup itself.
                    self.ai_msg = r.err();
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.ai_job = None,
            }
        }
        if let Some((_, rx)) = &self.loading {
            match rx.try_recv() {
                Ok(Ok((path, full))) => {
                    self.center = Pos2::new(full.width() as f32 / 2.0, full.height() as f32 / 2.0);
                    self.source = Some(Source { path, full });
                    self.preview = None;
                    self.preview_error = None;
                    self.ai_view = None;
                    self.ai_pending = None;
                    self.loading = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.source = None;
                    self.preview = None;
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.loading = None,
            }
        }
        if let Some((req, rx)) = &self.pending {
            match rx.try_recv() {
                Ok(Ok((before, after))) => {
                    let opts = egui::TextureOptions::LINEAR;
                    self.preview = Some(Preview {
                        req: *req,
                        before: ctx.load_texture("enhance-before", before, opts),
                        after: Some(ctx.load_texture("enhance-after", after, opts)),
                    });
                    self.preview_error = None;
                    self.pending = None;
                }
                Ok(Err(e)) => {
                    self.preview_error = Some(e);
                    self.pending = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending = None,
            }
        }
        if let Some(p) = &self.ai_pending {
            match p.rx.try_recv() {
                Ok(Ok(out)) => {
                    self.ai_view = Some(AiView {
                        opts: p.opts,
                        area: p.area,
                        out: Arc::new(out),
                        seconds: p.started.elapsed().as_secs_f32(),
                    });
                    self.preview = None;
                    self.preview_error = None;
                    self.ai_pending = None;
                }
                Ok(Err(e)) => {
                    self.preview_error = Some(e);
                    self.ai_pending = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.ai_pending = None,
            }
        }
        let done = self
            .running
            .as_ref()
            .and_then(|r| r.report.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(report) = done {
            self.running = None;
            self.last = Some(report);
            self.finished = true;
            // The tree may hold new files now.
            self.tree.reset();
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, dir: Option<&Path>, folder: &[PathBuf]) {
        ui.horizontal_top(|ui| {
            let avail = ui.available_size();
            let size = Vec2::new((avail.x - SIDE_W - 40.0).max(300.0), avail.y.max(320.0));
            ui.allocate_ui(size, |ui| {
                ui.vertical(|ui| self.compare(ui));
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(SIDE_W);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.settings(ui);
                    ui.separator();
                    self.save_controls(ui, dir, folder);
                });
            });
        });
    }

    // ----- the comparison ---------------------------------------------------

    fn compare(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Cannot open photo: {e}"),
            );
            return;
        }
        let Some(src) = &self.source else {
            ui.horizontal(|ui| {
                if self.loading.is_some() {
                    ui.spinner();
                    ui.label("Loading photo…");
                } else {
                    ui.label("Open a photo first.");
                }
            });
            return;
        };
        let full = src.full.clone();
        let (w, h) = (full.width() as f32, full.height() as f32);
        let s = self.scale as f32;
        let ppp = ui.ctx().pixels_per_point();
        if self.ai {
            self.ai_bar(ui, &full);
        }

        // Zoom bar.
        let bar_h = ui.spacing().interact_size.y;
        let label_h = ui.text_style_height(&egui::TextStyle::Body);
        let avail = ui.available_size();
        let gap = 6.0;
        let pane = Vec2::new(
            ((avail.x - gap) / 2.0).max(100.0),
            (avail.y - bar_h - label_h - 3.0 * ui.spacing().item_spacing.y).max(150.0),
        );

        // Limits: from fit-to-pane (or the preview size cap) up to 800%.
        let fit = (pane.x / (w * s)).min(pane.y / (h * s));
        let mut z_min = fit;
        if w * s > MAX_PREVIEW_SIDE {
            z_min = z_min.max(pane.x / MAX_PREVIEW_SIDE);
        }
        if h * s > MAX_PREVIEW_SIDE {
            z_min = z_min.max(pane.y / MAX_PREVIEW_SIDE);
        }
        // The AI preview is made once for the whole photo, so the view is
        // not limited by how much can be enhanced quickly.
        let cap = max_preview_pixels((self.ai && self.ai_view.is_none()).then_some(self.ai_model));
        if w * s * h * s > cap {
            z_min = z_min.max((pane.x * pane.y / cap).sqrt());
        }
        let z_max = (8.0 / ppp).max(z_min);
        let actual = 1.0 / ppp;
        let mut zoom = self.zoom.unwrap_or(actual).clamp(z_min, z_max);

        ui.horizontal(|ui| {
            ui.label("Zoom");
            if ui
                .selectable_label((zoom - z_min).abs() < 1e-4, "Fit")
                .clicked()
            {
                zoom = z_min;
            }
            for (label, f) in [("50%", 0.5), ("100%", 1.0), ("200%", 2.0), ("400%", 4.0)] {
                let z = f * actual;
                if ui
                    .add_enabled(
                        z >= z_min - 1e-4,
                        egui::Button::selectable((zoom - z).abs() < 1e-4, label),
                    )
                    .on_hover_text("Percent of the result's pixels")
                    .clicked()
                {
                    zoom = z;
                }
            }
            ui.weak(format!("{:.0}%", zoom * ppp * 100.0));
            ui.separator();
            let busy = self.pending.is_some() && !self.ai;
            if busy {
                ui.spinner();
            }
            ui.weak("Drag to move, wheel to zoom, double-click for fit / 100%");
        });

        // Labels over the panes.
        let (out_w, out_h) = (full.width() * self.scale, full.height() * self.scale);
        let mut what = if self.amount > 0.0 {
            format!("{}x, sharpen {:.0}%", self.scale, self.amount)
        } else {
            format!("{}x, no sharpening", self.scale)
        };
        if self.ai {
            // Describe the preview on screen, which may predate the settings.
            let o = self.ai_view.as_ref().map_or(self.opts(), |v| v.opts);
            what = if o.amount > 0.0 {
                format!("{}x, sharpen {:.0}%", o.scale, o.amount * 100.0)
            } else {
                format!("{}x, no sharpening", o.scale)
            };
            what += &format!(", AI {}", o.ai_model.label());
            if o.ai_model.has_noise_control() {
                what += &format!(" noise {:.0}", o.ai_noise * 100.0);
            }
            let dev = chitra::ai::last_device();
            if !dev.is_empty() {
                what += &format!(", on the {dev}");
            }
        } else if self.denoise > 0.0 {
            what += &format!(", noise {:.0}", self.denoise);
        }
        let (labels, _) =
            ui.allocate_exact_size(Vec2::new(pane.x * 2.0 + gap, label_h), Sense::hover());
        let font = egui::TextStyle::Body.resolve(ui.style());
        let color = ui.visuals().strong_text_color();
        for (x, text) in [
            (
                0.0,
                format!("Original  {} × {}", full.width(), full.height()),
            ),
            (pane.x + gap, format!("Result  {out_w} × {out_h}  ({what})")),
        ] {
            ui.painter_at(labels).text(
                labels.min + Vec2::new(x, 0.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                color,
            );
        }

        let (area, _) =
            ui.allocate_exact_size(Vec2::new(pane.x * 2.0 + gap, pane.y), Sense::hover());
        let left = Rect::from_min_size(area.min, pane);
        let right = Rect::from_min_size(area.min + Vec2::new(pane.x + gap, 0.0), pane);

        // Input on either pane moves both.
        let mut k = zoom * s; // points per original pixel
        for (i, rect) in [left, right].into_iter().enumerate() {
            let resp = ui.interact(
                rect,
                ui.make_persistent_id(("enhance-pane", i)),
                Sense::click_and_drag(),
            );
            if resp.dragged() {
                self.center -= resp.drag_delta() / k;
            }
            if resp.double_clicked() {
                zoom = if (zoom - z_min).abs() < 1e-4 {
                    actual.max(z_min)
                } else {
                    z_min
                };
            }
            if resp.hovered() {
                let (scroll, pinch, pointer) = ui.input(|i| {
                    (
                        i.smooth_scroll_delta.y,
                        i.zoom_delta(),
                        i.pointer.hover_pos(),
                    )
                });
                let factor = (scroll * 0.0025).exp() * pinch;
                if factor != 1.0 {
                    let new_zoom = (zoom * factor).clamp(z_min, z_max);
                    // Keep the point under the pointer still.
                    if let Some(p) = pointer {
                        let under = self.center + (p - rect.center()) / k;
                        let nk = new_zoom * s;
                        self.center = under - (p - rect.center()) / nk;
                    }
                    zoom = new_zoom;
                }
            }
            k = zoom * s;
        }
        self.zoom = Some(zoom);

        // Keep the photo in view.
        let half = pane / (2.0 * k);
        self.center.x = if half.x * 2.0 >= w {
            w / 2.0
        } else {
            self.center.x.clamp(half.x, w - half.x)
        };
        self.center.y = if half.y * 2.0 >= h {
            h / 2.0
        } else {
            self.center.y.clamp(half.y, h - half.y)
        };

        let visible = region(self.center, half, w, h);
        if self.ai {
            self.ai_show(ui.ctx(), &full, visible);
        } else {
            self.request(&full, visible);
        }

        let bg = ui.visuals().extreme_bg_color;
        for (rect, after) in [(left, false), (right, true)] {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, bg);
            if let Some(p) = &self.preview {
                let (rx, ry, rw, rh) = p.req.region;
                let map =
                    |x: f32, y: f32| rect.center() + (Vec2::new(x, y) - self.center.to_vec2()) * k;
                let dst = Rect::from_min_max(
                    map(rx as f32, ry as f32),
                    map((rx + rw) as f32, (ry + rh) as f32),
                );
                let tex = if after {
                    p.after.as_ref()
                } else {
                    Some(&p.before)
                };
                let Some(tex) = tex else { continue };
                painter.image(
                    tex.id(),
                    dst,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        if self.ai && self.ai_view.is_none() && self.preview_error.is_none() {
            let msg = if self.ai_pending.is_some() {
                "Making the AI preview…"
            } else if self.ai_ready {
                "Click Make AI preview (next to Reset) to see the result"
            } else {
                "Get the AI model first (on the right)"
            };
            ui.painter_at(right).text(
                right.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
        }
        if let Some(e) = &self.preview_error {
            ui.painter_at(right).text(
                right.center(),
                egui::Align2::CENTER_CENTER,
                e,
                egui::FontId::proportional(14.0),
                ui.visuals().error_fg_color,
            );
        }
    }

    /// AI method: say whether the AI runs on the processor or a graphics
    /// card, and let the user choose when Automatic picks wrongly.
    fn ai_device_picker(&mut self, ui: &mut egui::Ui) {
        use chitra::ai::{Compute, compute, compute_report, set_compute};
        let report = compute_report(false);
        let current = compute();
        let describe = |name: &str| {
            report
                .adapters
                .iter()
                .find(|a| a.name == name)
                .map_or(name.to_string(), |a| {
                    format!("{} ({}, {})", a.name, a.kind, a.backend)
                })
        };
        let auto_text = match &report.auto_pick {
            Some(n) => format!("Automatic: {}", describe(n)),
            None => "Automatic: processor".to_string(),
        };
        let selected = match &current {
            Compute::Auto => auto_text.clone(),
            Compute::Processor => "Processor only".to_string(),
            Compute::Adapter(n) => describe(n),
        };
        ui.label("Run the AI on");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ai-device")
                .width(330.0)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    let mut pick = current.clone();
                    ui.selectable_value(&mut pick, Compute::Auto, auto_text);
                    ui.selectable_value(&mut pick, Compute::Processor, "Processor only");
                    for a in report.adapters.iter().filter(|a| a.kind != "software") {
                        ui.selectable_value(
                            &mut pick,
                            Compute::Adapter(a.name.clone()),
                            describe(&a.name),
                        );
                    }
                    if pick != current {
                        set_compute(pick);
                    }
                })
                .response
                .on_hover_text(
                    "Graphics cards are much faster than the processor for the AI. \
                     Pick one here if Automatic chose the wrong one or the previews \
                     are slow.",
                );
            if ui
                .small_button("Look again")
                .on_hover_text(
                    "Search for graphics cards again, for example after installing a driver",
                )
                .clicked()
            {
                compute_report(true);
            }
        });
        // What really happened, not just what was asked for.
        let last = chitra::ai::last_device();
        let (text, warn) = if last.is_empty() {
            let will = match &current {
                Compute::Processor => "the processor".to_string(),
                Compute::Adapter(n) => format!("the graphics card {n}"),
                Compute::Auto => report
                    .auto_pick
                    .as_ref()
                    .map_or("the processor".to_string(), |n| {
                        format!("the graphics card {n}")
                    }),
            };
            (
                format!("Not used yet. The next preview will use {will}."),
                false,
            )
        } else {
            let on_gpu = last.starts_with("graphics card");
            (
                format!("Last preview ran on the {last}."),
                !on_gpu && current != Compute::Processor && report.auto_pick.is_some(),
            )
        };
        if warn {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{text} A graphics card was available but was not used."),
            );
        } else {
            ui.weak(text);
        }
        if !report.why_not.is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Graphics card: {}.", report.why_not),
            );
        } else if report.adapters.is_empty() {
            ui.weak("No graphics card was found, so the AI will use the processor (slow).");
        }
    }

    /// AI method: how the preview is going, over the comparison. The button
    /// that makes it sits next to Reset in the settings.
    fn ai_bar(&mut self, ui: &mut egui::Ui, full: &Arc<DynamicImage>) {
        ui.horizontal(|ui| {
            if let Some(p) = &self.ai_pending {
                ui.spinner();
                let what = if p.area == (0, 0, full.width(), full.height()) {
                    "the whole photo"
                } else {
                    "the part in view"
                };
                ui.label(format!(
                    "Making the AI preview of {what}… {:.0} s",
                    p.started.elapsed().as_secs_f32()
                ));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                return;
            }
            match &self.ai_view {
                Some(_) if self.ai_stale() => {
                    ui.weak("Settings changed since this preview: click Update AI preview.");
                }
                Some(v) => {
                    ui.weak(format!("AI preview made in {:.0} s.", v.seconds));
                }
                None => {
                    ui.weak("Change the settings, then click Make AI preview.");
                }
            }
        });
    }

    /// The AI preview was made with other settings than the current ones.
    fn ai_stale(&self) -> bool {
        let opts = self.opts();
        self.ai_view.as_ref().is_some_and(|v| v.opts != opts)
    }

    /// The Make AI preview button, next to Reset.
    fn ai_button(&mut self, ui: &mut egui::Ui) {
        if self.ai_pending.is_some() {
            ui.spinner();
            ui.label("Making the AI preview…");
            return;
        }
        let stale = self.ai_stale();
        let label = match (&self.ai_view, stale) {
            (None, _) => "Make AI preview",
            (Some(_), true) => "Update AI preview",
            (Some(_), false) => "Preview is up to date",
        };
        let enabled = self.ai_ready && (self.ai_view.is_none() || stale);
        let button = egui::Button::new(egui::RichText::new(label).strong());
        if ui
            .add_enabled(enabled, button)
            .on_hover_text(
                "Runs the AI on the whole photo with these settings. On the \
                 processor this can take a minute or more; moving around the \
                 result afterwards is instant.",
            )
            .on_disabled_hover_text(if self.ai_ready {
                "The preview already shows these settings."
            } else {
                "Get the AI model first."
            })
            .clicked()
            && let Some(full) = self.source.as_ref().map(|s| s.full.clone())
        {
            let opts = self.opts();
            self.start_ai_preview(&full, opts);
        }
    }

    /// Run the AI on the whole photo (or, for a very large result, the part
    /// in view) in the background.
    fn start_ai_preview(&mut self, full: &Arc<DynamicImage>, opts: EnhanceOptions) {
        let (w, h) = (full.width(), full.height());
        let s = opts.scale as f64;
        let area = if w as f64 * h as f64 * s * s <= AI_PREVIEW_MAX {
            (0, 0, w, h)
        } else {
            let (fw, fh) = (w as f32, h as f32);
            let k = self.zoom.unwrap_or(1.0) * opts.scale as f32;
            // Half the pane in original pixels; the pane is about 500 points.
            let half = Vec2::splat(400.0 / k.max(1e-3));
            region(self.center, half, fw, fh)
        };
        let (tx, rx) = mpsc::channel();
        let full = full.clone();
        std::thread::spawn(move || {
            let _ = tx.send(enhance::enhance_region(&full, &opts, area));
        });
        self.preview_error = None;
        self.ai_pending = Some(AiPending {
            opts,
            area,
            started: Instant::now(),
            rx,
        });
    }

    /// Show the part of the AI preview in view (with room to pan), uploading
    /// it only when the view leaves what is already on the card.
    fn ai_show(&mut self, ctx: &egui::Context, full: &Arc<DynamicImage>, visible: Region) {
        let tex = egui::TextureOptions::LINEAR;
        let Some(v) = &self.ai_view else {
            // No AI result yet: still show the original.
            let shown = self
                .preview
                .as_ref()
                .is_some_and(|p| p.after.is_none() && contains(p.req.region, visible));
            if !shown {
                let r = widen(visible, full.width(), full.height(), self.scale);
                self.preview = Some(Preview {
                    req: Request {
                        region: r,
                        opts: self.opts(),
                    },
                    before: ctx.load_texture(
                        "enhance-before",
                        rgba(&full.crop_imm(r.0, r.1, r.2, r.3)),
                        tex,
                    ),
                    after: None,
                });
            }
            return;
        };
        // Outside the previewed area (a very large photo) only the original shows.
        let Some(r) = intersect(visible, v.area) else {
            self.preview = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.after.is_some() && p.req.opts == v.opts && contains(p.req.region, r))
        {
            return;
        }
        let s = v.opts.scale;
        let r = intersect(widen(r, full.width(), full.height(), s), v.area).unwrap_or(r);
        let a = v.area;
        let before = rgba(&full.crop_imm(r.0, r.1, r.2, r.3));
        let after = rgba(
            &v.out
                .crop_imm((r.0 - a.0) * s, (r.1 - a.1) * s, r.2 * s, r.3 * s),
        );
        self.preview = Some(Preview {
            req: Request {
                region: r,
                opts: v.opts,
            },
            before: ctx.load_texture("enhance-before", before, tex),
            after: Some(ctx.load_texture("enhance-after", after, tex)),
        });
    }

    /// Start enhancing the visible part (with some room to pan) unless the
    /// current or pending preview already covers it.
    fn request(&mut self, full: &Arc<DynamicImage>, visible: Region) {
        let opts = self.opts();
        let covers = |r: &Request| r.opts == opts && contains(r.region, visible);
        if self.preview.as_ref().is_some_and(|p| covers(&p.req)) {
            return;
        }
        if let Some((req, _)) = &self.pending {
            // One at a time: a newer request starts when this one is done.
            let _ = req;
            return;
        }
        let region = widen(visible, full.width(), full.height(), self.scale);
        let req = Request { region, opts };
        let (tx, rx) = mpsc::channel();
        let full = full.clone();
        std::thread::spawn(move || {
            let (x, y, w, h) = req.region;
            let before = rgba(&full.crop_imm(x, y, w, h));
            let r =
                enhance::enhance_region(&full, &req.opts, req.region).map(|a| (before, rgba(&a)));
            let _ = tx.send(r);
        });
        self.pending = Some((req, rx));
    }

    // ----- settings -----------------------------------------------------------

    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Enhance");
        ui.add_space(4.0);
        ui.label("Size");
        ui.horizontal(|ui| {
            for s in 1..=4u32 {
                if ui
                    .selectable_label(self.scale == s, format!("{s}x"))
                    .on_hover_text(match s {
                        1 => "Keep the size, sharpen only",
                        _ => "Enlarge with Lanczos3",
                    })
                    .clicked()
                    && self.scale != s
                {
                    self.scale = s;
                    if self.radius_auto {
                        self.radius = enhance::default_radius(s);
                    }
                    if self.rename_auto {
                        self.rename = auto_name(s, self.ai);
                    }
                }
            }
        });
        if let Some(src) = &self.source {
            let (w, h) = (src.full.width(), src.full.height());
            match enhance::output_size(w, h, self.scale) {
                Ok((ow, oh)) => ui.weak(format!(
                    "{w} × {h} to {ow} × {oh} ({:.1} MP)",
                    ow as f64 * oh as f64 / 1e6
                )),
                Err(e) => ui.colored_label(ui.visuals().error_fg_color, e),
            };
        }
        ui.add_space(6.0);

        ui.label("Method");
        ui.horizontal(|ui| {
            let classic = ui
                .selectable_label(!self.ai, "Classic")
                .on_hover_text("Lanczos3: fast, faithful, adds no detail");
            let ai = ui.selectable_label(self.ai, "AI").on_hover_text(
                "Real-ESRGAN: draws plausible fine detail and removes JPEG artefacts. \
                 Much slower than Classic, unless a graphics card can run it, and \
                 each model is a one-time download.",
            );
            let want = if ai.clicked() {
                Some(true)
            } else if classic.clicked() {
                Some(false)
            } else {
                None
            };
            if let Some(want) = want
                && want != self.ai
            {
                self.ai = want;
                // The AI result is already crisp; classic needs sharpening.
                self.amount = if want { 0.0 } else { 80.0 };
                if self.rename_auto {
                    self.rename = auto_name(self.scale, self.ai);
                }
                self.ai_ready = chitra::ai::installed(self.ai_model);
            }
        });
        if self.ai {
            ui.label("AI model");
            let before = self.ai_model;
            egui::ComboBox::from_id_salt("ai-model")
                .selected_text(self.ai_model.label())
                .show_ui(ui, |ui| {
                    for m in chitra::ai::Model::ALL {
                        let mut text = m.label().to_string();
                        if !chitra::ai::installed(m) {
                            text += &format!(" ({} MB download)", mb(m.download_bytes()));
                        }
                        ui.selectable_value(&mut self.ai_model, m, text)
                            .on_hover_text(m.about());
                    }
                });
            ui.weak(self.ai_model.about());
            if self.ai_model != before {
                self.ai_ready = chitra::ai::installed(self.ai_model);
                self.ai_msg = None;
            }
        }
        if self.ai {
            self.ai_device_picker(ui);
        }
        if self.ai && !self.ai_ready {
            self.ai_setup(ui);
        }
        ui.add_space(6.0);

        if self.ai && self.ai_model.has_noise_control() {
            ui.label("AI noise removal");
            ui.add(egui::Slider::new(&mut self.ai_noise, 0.0..=100.0).step_by(5.0))
                .on_hover_text(
                    "How much grain and JPEG noise the model removes. Lower keeps \
                     more texture; higher is cleaner but smoother.",
                );
        } else if !self.ai {
            ui.label("Reduce noise");
            ui.add(egui::Slider::new(&mut self.denoise, 0.0..=100.0).step_by(5.0))
                .on_hover_text(
                    "Smooths grain and the colour blotches of old JPEGs before enlarging, \
                     keeping edges sharp. 0 turns it off; too high looks waxy.",
                );
        }
        ui.label("Sharpen");
        ui.add(
            egui::Slider::new(&mut self.amount, 0.0..=300.0)
                .suffix("%")
                .step_by(5.0),
        )
        .on_hover_text("How strongly edges and detail are brought out. 0 turns it off.");
        ui.label("Radius");
        ui.horizontal(|ui| {
            ui.add_enabled(
                !self.radius_auto,
                egui::Slider::new(&mut self.radius, 0.3..=5.0)
                    .suffix(" px")
                    .step_by(0.1),
            )
            .on_hover_text("Size of the detail to sharpen, in result pixels");
            if ui
                .checkbox(&mut self.radius_auto, "Auto")
                .on_hover_text("Pick a radius that suits the size")
                .changed()
                && self.radius_auto
            {
                self.radius = enhance::default_radius(self.scale);
            }
        });
        ui.label("Ignore noise below");
        ui.add(egui::Slider::new(&mut self.threshold, 0.0..=20.0).step_by(0.5))
            .on_hover_text(
                "Small changes in brightness (grain, JPEG blocks) are sharpened less. \
                 Raise it for noisy or heavily compressed photos.",
            );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Reset").clicked() {
                self.amount = if self.ai { 0.0 } else { 80.0 };
                self.ai_noise = 50.0;
                self.radius_auto = true;
                self.radius = enhance::default_radius(self.scale);
                self.threshold = 3.0;
                self.denoise = DEFAULT_DENOISE;
            }
            if self.ai && self.source.is_some() {
                self.ai_button(ui);
            }
        });
    }

    /// Getting the AI model: download it, or pick files downloaded by hand.
    fn ai_setup(&mut self, ui: &mut egui::Ui) {
        let m = self.ai_model;
        let missing = chitra::ai::missing(m);
        let partial = missing.len() < m.files().len();
        let bytes: u64 = missing.iter().map(|f| f.bytes).sum();
        // The Flatpak has no network access, so files come in by hand.
        let offline = chitra::update::Install::current() == chitra::update::Install::Flatpak;
        ui.add(
            egui::Label::new(if partial {
                format!(
                    "The {} model is partly installed; about {} MB is still needed.",
                    m.label(),
                    mb(bytes)
                )
            } else {
                format!(
                    "The {} model is not installed. It is downloaded once (about {} MB, \
                     Real-ESRGAN by Xintao Wang et al., BSD licence) and checked before use.",
                    m.label(),
                    mb(bytes)
                )
            })
            .wrap(),
        );
        if offline {
            ui.add(
                egui::Label::new(
                    "This Flatpak cannot reach the internet. Download the file(s) \
                     below in your browser, then click Choose files… and pick them.",
                )
                .wrap(),
            );
        }
        if self.ai_job.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Getting the model…");
            });
            return;
        }
        ui.horizontal(|ui| {
            if !offline && ui.button("Download").clicked() {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(chitra::ai::download(m));
                });
                self.ai_job = Some(rx);
                self.ai_msg = None;
            }
            if ui
                .button("Choose files…")
                .on_hover_text(format!(
                    "If the download does not work here, get the model's file(s) in a \
                     browser and choose them:\n{}",
                    m.files()
                        .iter()
                        .map(|f| f.url)
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
                .clicked()
                && let Some(files) = rfd::FileDialog::new()
                    .add_filter("Model", &["pth"])
                    .pick_files()
            {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(chitra::ai::import(&files).map(|_| ()));
                });
                self.ai_job = Some(rx);
                self.ai_msg = None;
            }
        });
        if let Some(m) = &self.ai_msg {
            ui.colored_label(ui.visuals().error_fg_color, m);
        }
        // Links for getting the files in a browser, still missing ones only.
        ui.label(
            egui::RichText::new(if partial { "Still needed:" } else { "Files:" })
                .small()
                .weak(),
        );
        for f in &missing {
            ui.horizontal(|ui| {
                if ui
                    .link(egui::RichText::new(f.name).small())
                    .on_hover_text(format!("Open in your browser:\n{}", f.url))
                    .clicked()
                {
                    super::links::open(f.url);
                }
                // If no browser opens (a desktop without a default one), the
                // address can still be pasted into one.
                if ui.small_button("Copy link").on_hover_text(f.url).clicked() {
                    ui.ctx().copy_text(f.url.to_string());
                }
            });
        }
    }

    // ----- saving -----------------------------------------------------------

    fn save_controls(&mut self, ui: &mut egui::Ui, dir: Option<&Path>, folder: &[PathBuf]) {
        ui.heading("Save");
        ui.add_space(4.0);
        let busy = self.running.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("enhance-save-grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Photos");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut self.scope, Scope::Current, "This photo");
                        ui.radio_value(
                            &mut self.scope,
                            Scope::Folder,
                            if self.subfolders && dir.is_some() {
                                "All in folder".to_string()
                            } else {
                                format!("All {} in folder", folder.len())
                            },
                        );
                        ui.add_enabled(
                            self.scope == Scope::Folder && dir.is_some(),
                            egui::Checkbox::new(&mut self.subfolders, "Include sub-folders"),
                        )
                        .on_hover_text(
                            "Also save photos in every sub-folder. With a Save to folder, \
                             the same sub-folders are made there.",
                        );
                    });
                    ui.end_row();

                    ui.label("Format");
                    ui.horizontal(|ui| {
                        for f in FORMATS {
                            ui.selectable_value(&mut self.format, f, f.name());
                        }
                    });
                    ui.end_row();

                    ui.label("Quality");
                    ui.horizontal(|ui| {
                        if encode::is_lossy(self.format, self.lossless) {
                            ui.add(egui::Slider::new(&mut self.quality, 1..=100));
                        } else {
                            ui.weak("Lossless");
                        }
                        if encode::LOSSY_ENCODERS && self.format == Format::JpegXl {
                            ui.checkbox(&mut self.lossless, "Lossless");
                        }
                    });
                    ui.end_row();

                    ui.label("Save to");
                    ui.vertical(|ui| {
                        let label = match &self.out_dir {
                            Some(d) => d.display().to_string(),
                            None => "Same folder as each photo".to_string(),
                        };
                        ui.add(egui::Label::new(label).wrap());
                        ui.horizontal(|ui| {
                            if ui.button("Choose…").clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.out_dir = Some(dir);
                                }
                            }
                            if self.out_dir.is_some() && ui.small_button("Reset").clicked() {
                                self.out_dir = None;
                            }
                        });
                    });
                    ui.end_row();

                    ui.label("File name");
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.rename).hint_text("{name}_2x"))
                        .on_hover_text("Tokens: {name} {parent} {n} {n:03}")
                        .changed()
                    {
                        self.rename_auto = false;
                    }
                    ui.end_row();

                    ui.label("If file exists");
                    egui::ComboBox::from_id_salt("enhance-conflict")
                        .selected_text(self.conflict.label())
                        .show_ui(ui, |ui| {
                            for c in ConflictPolicy::ALL {
                                ui.selectable_value(&mut self.conflict, c, c.label());
                            }
                        });
                    ui.end_row();

                    ui.label("Metadata");
                    ui.checkbox(&mut self.keep_metadata, "Keep EXIF and colour profile");
                    ui.end_row();
                });
        });
        ui.add_space(6.0);

        if let Some(run) = &self.running {
            let done = run.done.load(Ordering::Relaxed);
            ui.add(
                egui::ProgressBar::new(done as f32 / run.total.max(1) as f32)
                    .text(format!("{done} / {}", run.total))
                    .animate(true),
            );
            return;
        }

        let tree = match (self.scope, dir) {
            (Scope::Folder, Some(dir)) if self.subfolders => Some(dir),
            _ => None,
        };
        let (inputs, count): (Vec<PathBuf>, Option<usize>) = match (self.scope, tree) {
            (_, Some(dir)) => (
                vec![dir.to_path_buf()],
                self.tree.get(ui.ctx(), dir, self.out_dir.as_ref()),
            ),
            (Scope::Current, None) => {
                let v: Vec<PathBuf> = self.source.iter().map(|s| s.path.clone()).collect();
                let n = v.len();
                (v, Some(n))
            }
            (Scope::Folder, None) => (folder.to_vec(), Some(folder.len())),
        };
        let label = match count {
            Some(n) => format!("Save {n} photo{}", if n == 1 { "" } else { "s" }),
            None => "Counting photos…".to_string(),
        };
        if ui
            .add_enabled(
                count.unwrap_or(0) > 0 && (!self.ai || self.ai_ready),
                egui::Button::new(label),
            )
            .clicked()
        {
            self.start(inputs, tree.is_some());
        }

        if let Some(report) = &self.last {
            let (bin, bout) = report.bytes();
            ui.label(format!(
                "Done in {:.1} s: {} saved, {} skipped, {} failed ({} to {}).",
                report.elapsed.as_secs_f64(),
                report.converted(),
                report.skipped(),
                report.failed(),
                human_bytes(bin),
                human_bytes(bout),
            ));
            if report.failed() > 0 {
                egui::ScrollArea::vertical()
                    .id_salt("enhance-failures")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for r in &report.results {
                            if let Outcome::Failed(why) = &r.outcome {
                                let name = r
                                    .job
                                    .input
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy();
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    format!("{name}: {why}"),
                                );
                            }
                        }
                    });
            }
        }
    }

    fn start(&mut self, inputs: Vec<PathBuf>, recursive: bool) {
        let mut opts = ConvertOptions::new(self.format);
        opts.quality = self.quality;
        opts.lossless = self.lossless;
        opts.enhance = Some(self.opts());
        opts.out_dir = self.out_dir.clone();
        opts.rename = if self.rename.trim().is_empty() {
            auto_name(self.scale, self.ai)
        } else {
            self.rename.clone()
        };
        opts.conflict = self.conflict;
        opts.keep_metadata = self.keep_metadata;
        opts.recursive = recursive;

        let jobs = convert::plan(&inputs, &opts);
        let done = Arc::new(AtomicUsize::new(0));
        let report = Arc::new(Mutex::new(None));
        self.running = Some(Running {
            total: jobs.len(),
            done: done.clone(),
            report: report.clone(),
        });
        self.last = None;
        std::thread::spawn(move || {
            let r = convert::run(jobs, &opts, |_| {
                done.fetch_add(1, Ordering::Relaxed);
            });
            *report.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
    }
}

/// The original pixels around `center` that fill a pane, clipped to the photo.
fn region(center: Pos2, half: Vec2, w: f32, h: f32) -> Region {
    let x0 = (center.x - half.x).floor().clamp(0.0, w - 1.0);
    let y0 = (center.y - half.y).floor().clamp(0.0, h - 1.0);
    let x1 = (center.x + half.x).ceil().clamp(x0 + 1.0, w);
    let y1 = (center.y + half.y).ceil().clamp(y0 + 1.0, h);
    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn intersect(a: Region, b: Region) -> Option<Region> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

fn contains(outer: Region, inner: Region) -> bool {
    outer.0 <= inner.0
        && outer.1 <= inner.1
        && outer.0 + outer.2 >= inner.0 + inner.2
        && outer.1 + outer.3 >= inner.1 + inner.3
}

/// Add room to pan around `r` (a quarter of its size each way), within the
/// photo and the preview size limits.
fn widen(r: Region, w: u32, h: u32, scale: u32) -> Region {
    let s = scale as f32;
    let fits = |rw: u32, rh: u32| {
        let (ow, oh) = (rw as f32 * s, rh as f32 * s);
        ow <= MAX_PREVIEW_SIDE && oh <= MAX_PREVIEW_SIDE && ow * oh <= MAX_PREVIEW_PIXELS
    };
    let (mx, my) = (r.2 / 4, r.3 / 4);
    let x0 = r.0.saturating_sub(mx);
    let y0 = r.1.saturating_sub(my);
    let x1 = (r.0 + r.2 + mx).min(w);
    let y1 = (r.1 + r.3 + my).min(h);
    if fits(x1 - x0, y1 - y0) {
        (x0, y0, x1 - x0, y1 - y0)
    } else {
        r
    }
}

fn rgba(img: &DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}

/// Bytes as whole megabytes.
fn mb(bytes: u64) -> u64 {
    (bytes as f64 / 1e6).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions() {
        let r = region(Pos2::new(50.0, 50.0), Vec2::new(10.0, 5.0), 100.0, 100.0);
        assert_eq!(r, (40, 45, 20, 10));
        // Clipped at the edges.
        let r = region(Pos2::new(2.0, 98.0), Vec2::new(10.0, 10.0), 100.0, 100.0);
        assert_eq!(r, (0, 88, 12, 12));
        assert!(contains((0, 0, 100, 100), (10, 10, 5, 5)));
        assert!(!contains((10, 10, 5, 5), (0, 0, 100, 100)));
        assert_eq!(
            intersect((0, 0, 10, 10), (5, 5, 10, 10)),
            Some((5, 5, 5, 5))
        );
        assert_eq!(intersect((0, 0, 5, 5), (5, 5, 10, 10)), None);
        assert_eq!(widen((40, 40, 20, 20), 100, 100, 1), (35, 35, 30, 30));
        // Too big to widen at 4x.
        assert_eq!(widen((0, 0, 1000, 1000), 4000, 4000, 4), (0, 0, 1000, 1000));
    }
}

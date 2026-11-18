//! The "Upscale and sharpen" window: enlarge a photo 1x to 4x, sharpen it,
//! compare the original and the result side by side, and save this photo or
//! the whole folder as JPEG, JPEG XL or PNG.
//!
//! The comparison only enhances the part of the photo on screen, so moving
//! the sliders stays quick even at 4x; saving runs the same code on the whole
//! photo through the batch converter.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, Vec2};
use image::DynamicImage;

use chitra::convert::{self, ConflictPolicy, ConvertOptions, Outcome, Report};
use chitra::decode::{self, DecodeOptions};
use chitra::encode;
use chitra::enhance::{self, EnhanceOptions};
use chitra::format::Format;

use super::util::human_bytes;

/// Output formats offered here.
const FORMATS: [Format; 3] = [Format::Jpeg, Format::JpegXl, Format::Png];
/// Largest side of a preview texture, in output pixels.
const MAX_PREVIEW_SIDE: f32 = 4096.0;
/// Largest preview, in output pixels.
const MAX_PREVIEW_PIXELS: f32 = 16_000_000.0;
const SIDE_W: f32 = 290.0;
const DEFAULT_DENOISE: f32 = 30.0;

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
    after: TextureHandle,
}

type PreviewResult = Result<(ColorImage, ColorImage), String>;
type Loaded = Result<(PathBuf, Arc<DynamicImage>), String>;

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

    /// Points on screen per output pixel; `None` until the first frame picks one.
    zoom: Option<f32>,
    /// Centre of the view, in original pixels.
    center: Pos2,
    preview: Option<Preview>,
    pending: Option<(Request, Receiver<PreviewResult>)>,
    preview_error: Option<String>,

    scope: Scope,
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
            zoom: None,
            center: Pos2::ZERO,
            preview: None,
            pending: None,
            preview_error: None,
            scope: Scope::Current,
            format: Format::Jpeg,
            quality: 92,
            lossless: false,
            out_dir: None,
            rename: auto_name(2),
            rename_auto: true,
            conflict: ConflictPolicy::Rename,
            keep_metadata: true,
            running: None,
            last: None,
            finished: false,
        }
    }
}

fn auto_name(scale: u32) -> String {
    if scale == 1 {
        "{name}_sharp".into()
    } else {
        format!("{{name}}_{scale}x")
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
        }
    }

    /// Open the window on `path`, loading it unless it is already loaded.
    pub fn open_on(&mut self, path: Option<PathBuf>) {
        self.open = true;
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

    pub fn show(&mut self, ctx: &egui::Context, current: Option<&PathBuf>, folder: &[PathBuf]) {
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
            .show(ctx, |ui| self.contents(ui, folder));
        self.open = open;
        if !self.open {
            // Free the photo and the preview textures.
            self.source = None;
            self.preview = None;
            self.loading = None;
            self.pending = None;
        }
        if self.loading.is_some() || self.pending.is_some() || self.running.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((_, rx)) = &self.loading {
            match rx.try_recv() {
                Ok(Ok((path, full))) => {
                    self.center = Pos2::new(full.width() as f32 / 2.0, full.height() as f32 / 2.0);
                    self.source = Some(Source { path, full });
                    self.preview = None;
                    self.preview_error = None;
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
                        after: ctx.load_texture("enhance-after", after, opts),
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
        let done = self
            .running
            .as_ref()
            .and_then(|r| r.report.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(report) = done {
            self.running = None;
            self.last = Some(report);
            self.finished = true;
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, folder: &[PathBuf]) {
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
                    self.save_controls(ui, folder);
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
        if w * s * h * s > MAX_PREVIEW_PIXELS {
            z_min = z_min.max((pane.x * pane.y / MAX_PREVIEW_PIXELS).sqrt());
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
            let busy = self.pending.is_some();
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
        if self.denoise > 0.0 {
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
        self.request(&full, visible);

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
                let tex = if after { &p.after } else { &p.before };
                painter.image(
                    tex.id(),
                    dst,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
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
                        self.rename = auto_name(s);
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

        ui.label("Reduce noise");
        ui.add(egui::Slider::new(&mut self.denoise, 0.0..=100.0).step_by(5.0))
            .on_hover_text(
                "Smooths grain and the colour blotches of old JPEGs before enlarging, \
                 keeping edges sharp. 0 turns it off; too high looks waxy.",
            );
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
        if ui.small_button("Reset").clicked() {
            self.amount = 80.0;
            self.radius_auto = true;
            self.radius = enhance::default_radius(self.scale);
            self.threshold = 3.0;
            self.denoise = DEFAULT_DENOISE;
        }
    }

    // ----- saving -----------------------------------------------------------

    fn save_controls(&mut self, ui: &mut egui::Ui, folder: &[PathBuf]) {
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
                            format!("All {} in folder", folder.len()),
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

        let inputs: Vec<PathBuf> = match self.scope {
            Scope::Current => self.source.iter().map(|s| s.path.clone()).collect(),
            Scope::Folder => folder.to_vec(),
        };
        let label = format!(
            "Save {} photo{}",
            inputs.len(),
            if inputs.len() == 1 { "" } else { "s" }
        );
        if ui
            .add_enabled(!inputs.is_empty(), egui::Button::new(label))
            .clicked()
        {
            self.start(inputs);
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

    fn start(&mut self, inputs: Vec<PathBuf>) {
        let mut opts = ConvertOptions::new(self.format);
        opts.quality = self.quality;
        opts.lossless = self.lossless;
        opts.enhance = Some(self.opts());
        opts.out_dir = self.out_dir.clone();
        opts.rename = if self.rename.trim().is_empty() {
            auto_name(self.scale)
        } else {
            self.rename.clone()
        };
        opts.conflict = self.conflict;
        opts.keep_metadata = self.keep_metadata;

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
        assert_eq!(widen((40, 40, 20, 20), 100, 100, 1), (35, 35, 30, 30));
        // Too big to widen at 4x.
        assert_eq!(widen((0, 0, 1000, 1000), 4000, 4000, 4), (0, 0, 1000, 1000));
    }
}

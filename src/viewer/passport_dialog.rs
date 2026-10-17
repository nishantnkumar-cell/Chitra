//! The "Passport photo" window: mark the top of the head and the chin on the
//! current photo, pick a country, and save one photo or a print sheet.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use image::{DynamicImage, RgbImage};

use chitra::decode::{self, DecodeOptions};
use chitra::passport::{self, Layout, Marks, Paper, PhotoSpec, SPECS, SheetOptions};
use chitra::resize;

/// Longest side of the copy used for the on-screen photo and live preview.
const PREVIEW_SRC_PX: u32 = 1600;
/// Height of the live preview of the result, in points.
const RESULT_H: f32 = 260.0;
const MARK: Color32 = Color32::from_rgb(0, 200, 120);

struct Source {
    path: PathBuf,
    full: Arc<DynamicImage>,
    small: DynamicImage,
    /// `small` pixels per `full` pixel.
    scale: f32,
    texture: TextureHandle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Single,
    Sheet,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grab {
    Crown,
    Chin,
}

type Loaded = Result<(PathBuf, Arc<DynamicImage>, DynamicImage), String>;

pub struct PassportDialog {
    pub open: bool,
    source: Option<Source>,
    loading: Option<(PathBuf, Receiver<Loaded>)>,
    error: Option<String>,
    crown: Option<(f32, f32)>,
    chin: Option<(f32, f32)>,
    grab: Option<Grab>,
    /// Index into SPECS; SPECS.len() means the custom size below.
    spec: usize,
    custom_size: (f32, f32),
    custom_head: (f32, f32),
    head_mm: f32,
    /// Largest file for "One photo", in KB.
    max_kb: Option<u32>,
    straighten: bool,
    background: [u8; 3],
    output: Output,
    paper: Paper,
    result: Option<TextureHandle>,
    result_dirty: bool,
    saving: Option<Receiver<Result<PathBuf, String>>>,
    message: Option<String>,
    /// Set after a save, so the viewer can rescan its folder.
    pub saved: bool,
}

impl Default for PassportDialog {
    fn default() -> Self {
        PassportDialog {
            open: false,
            source: None,
            loading: None,
            error: None,
            crown: None,
            chin: None,
            grab: None,
            spec: 0,
            custom_size: (35.0, 45.0),
            custom_head: (32.0, 36.0),
            head_mm: SPECS[0].head_target_mm(),
            max_kb: SPECS[0].max_kb,
            straighten: true,
            background: [255, 255, 255],
            output: Output::Sheet,
            paper: Paper::Photo6x4,
            result: None,
            result_dirty: true,
            saving: None,
            message: None,
            saved: false,
        }
    }
}

impl PassportDialog {
    /// Open the window on `path`, loading it unless it is already loaded.
    pub fn open_on(&mut self, path: Option<PathBuf>) {
        self.open = true;
        let Some(path) = path else { return };
        let loaded = self.source.as_ref().map(|s| &s.path) == Some(&path)
            || self.loading.as_ref().map(|l| &l.0) == Some(&path);
        if loaded {
            return;
        }
        self.source = None;
        self.result = None;
        self.crown = None;
        self.chin = None;
        self.error = None;
        self.message = None;
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let r = decode::decode_file(&p, DecodeOptions::default())
                .map(|d| {
                    let small = resize::thumbnail(&d.image, PREVIEW_SRC_PX, PREVIEW_SRC_PX);
                    (p, Arc::new(d.image), small)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
        });
        self.loading = Some((path, rx));
    }

    fn spec(&self) -> PhotoSpec {
        match SPECS.get(self.spec) {
            Some(s) => *s,
            // The custom fields are kept valid by the controls; fall back to
            // the first preset just in case.
            None => passport::custom(self.custom_size.0, self.custom_size.1, self.custom_head)
                .unwrap_or(SPECS[0]),
        }
    }

    fn marks(&self) -> Option<Marks> {
        let m = Marks {
            crown: self.crown?,
            chin: self.chin?,
        };
        m.valid().then_some(m)
    }

    fn layout(&self, marks: Marks) -> Layout {
        let mut l = Layout::new(&self.spec(), marks);
        l.head_mm = self.head_mm;
        l.straighten = self.straighten;
        l.background = self.background;
        l
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        self.poll(ctx);
        let mut open = self.open;
        egui::Window::new("Passport photo")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([860.0, 600.0])
            .show(ctx, |ui| self.contents(ui));
        self.open = open;
        if self.loading.is_some() || self.saving.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((_, rx)) = &self.loading {
            match rx.try_recv() {
                Ok(Ok((path, full, small))) => {
                    let texture = ctx.load_texture(
                        "passport-source",
                        to_color_image(&small.to_rgba8().into_raw(), small.width(), small.height()),
                        egui::TextureOptions::LINEAR,
                    );
                    let scale = small.width() as f32 / full.width().max(1) as f32;
                    self.source = Some(Source {
                        path,
                        full,
                        small,
                        scale,
                        texture,
                    });
                    self.loading = None;
                    self.result_dirty = true;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.loading = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.loading = None,
            }
        }
        if let Some(rx) = &self.saving {
            match rx.try_recv() {
                Ok(r) => {
                    self.message = Some(match r {
                        Ok(p) => {
                            self.saved = true;
                            format!("Saved {}", p.display())
                        }
                        Err(e) => format!("Could not save: {e}"),
                    });
                    self.saving = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.saving = None,
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Cannot open photo: {e}"),
            );
            return;
        }
        if self.source.is_none() {
            if self.loading.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading photo…");
                });
            } else {
                ui.label("Open a photo first, then choose Passport.");
            }
            return;
        }
        let side_w = 280.0;
        ui.horizontal_top(|ui| {
            let avail = ui.available_size();
            let photo_size = Vec2::new((avail.x - side_w - 16.0).max(200.0), avail.y.max(300.0));
            ui.allocate_ui(photo_size, |ui| {
                ui.vertical(|ui| {
                    ui.label(self.hint());
                    self.photo(ui);
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(side_w);
                self.controls(ui);
            });
        });
    }

    fn hint(&self) -> &'static str {
        match (self.crown, self.chin) {
            (None, _) => "Click the top of the head (the crown, not the top of the hair).",
            (Some(_), None) => "Now click the bottom of the chin.",
            _ => "Drag the markers to adjust. Right-click to start again.",
        }
    }

    /// The source photo with the two markers; clicks place them, drags move them.
    fn photo(&mut self, ui: &mut egui::Ui) {
        let Some(src) = &self.source else { return };
        let (sw, sh) = (src.small.width() as f32, src.small.height() as f32);
        let avail = ui.available_size();
        let fit = (avail.x / sw).min(avail.y / sh);
        let size = Vec2::new(sw * fit, sh * fit);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.image(
            src.texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // Screen <-> full-resolution pixel coordinates.
        let k = fit * src.scale;
        let to_screen = |p: (f32, f32)| rect.min + Vec2::new(p.0 * k, p.1 * k);
        let to_image = |p: Pos2| {
            let v = (p - rect.min) / k;
            (v.x, v.y)
        };

        if response.secondary_clicked() {
            self.crown = None;
            self.chin = None;
            self.result_dirty = true;
        }
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let near = |p: Option<(f32, f32)>| {
                    p.map(|p| to_screen(p).distance(pos) < 14.0)
                        .unwrap_or(false)
                };
                self.grab = if near(self.chin) {
                    Some(Grab::Chin)
                } else if near(self.crown) {
                    Some(Grab::Crown)
                } else {
                    None
                };
            }
        }
        if response.dragged() {
            if let (Some(g), Some(pos)) = (self.grab, response.interact_pointer_pos()) {
                let p = to_image(rect.clamp(pos));
                match g {
                    Grab::Crown => self.crown = Some(p),
                    Grab::Chin => self.chin = Some(p),
                }
                self.result_dirty = true;
            }
        }
        if response.drag_stopped() {
            self.grab = None;
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_image(pos);
                if self.crown.is_none() {
                    self.crown = Some(p);
                } else if self.chin.is_none() {
                    self.chin = Some(p);
                }
                self.result_dirty = true;
            }
        }

        let stroke = Stroke::new(2.0, MARK);
        if let (Some(a), Some(b)) = (self.crown, self.chin) {
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(1.0, MARK));
        }
        for (p, label) in [(self.crown, "Top of head"), (self.chin, "Chin")] {
            if let Some(p) = p {
                let c = to_screen(p);
                painter.line_segment([c - Vec2::new(30.0, 0.0), c + Vec2::new(30.0, 0.0)], stroke);
                painter.circle_stroke(c, 5.0, stroke);
                painter.text(
                    c + Vec2::new(36.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(13.0),
                    MARK,
                );
            }
        }
        if self.marks().is_none() && self.crown.is_some() && self.chin.is_some() {
            painter.text(
                rect.center_top() + Vec2::new(0.0, 12.0),
                egui::Align2::CENTER_TOP,
                "The chin must be below the top of the head",
                egui::FontId::proportional(14.0),
                Color32::from_rgb(230, 80, 60),
            );
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let before = (
            self.spec,
            self.custom_size,
            self.custom_head,
            self.head_mm,
            self.straighten,
            self.background,
        );
        let mut picked = None;
        egui::ComboBox::from_id_salt("passport-country")
            .width(260.0)
            .selected_text(self.spec().name)
            .show_ui(ui, |ui| {
                for (i, s) in SPECS.iter().enumerate() {
                    if ui.selectable_value(&mut self.spec, i, s.name).changed() {
                        picked = Some(i);
                    }
                }
                if ui
                    .selectable_value(&mut self.spec, SPECS.len(), "Custom size")
                    .changed()
                {
                    picked = Some(SPECS.len());
                }
            });
        if self.spec == SPECS.len() {
            ui.horizontal(|ui| {
                let (w, h) = &mut self.custom_size;
                ui.add(egui::DragValue::new(w).range(10.0..=200.0).suffix(" mm"));
                ui.label("x");
                ui.add(egui::DragValue::new(h).range(10.0..=200.0).suffix(" mm"));
            });
            ui.horizontal(|ui| {
                ui.label("Head");
                let max = self.custom_size.1 - 3.0;
                let (lo, hi) = &mut self.custom_head;
                ui.add(egui::DragValue::new(lo).range(5.0..=max).speed(0.5));
                ui.label("to");
                ui.add(
                    egui::DragValue::new(hi)
                        .range(5.0..=max)
                        .speed(0.5)
                        .suffix(" mm"),
                );
                *hi = hi.min(max).max(*lo);
                *lo = lo.min(*hi);
            });
        }
        let s = self.spec();
        if picked.is_some() || self.custom_head != before.2 {
            self.head_mm = s.head_target_mm();
            if picked.is_some() {
                self.max_kb = s.max_kb;
            }
        }
        self.head_mm = self.head_mm.clamp(s.head_mm.0, s.head_mm.1);
        ui.label(format!(
            "{}, head {}-{} mm",
            s.size_label(),
            s.head_mm.0,
            s.head_mm.1
        ));
        ui.label(format!("Background: {}", s.background));
        ui.add_space(4.0);
        ui.add(
            egui::Slider::new(&mut self.head_mm, s.head_mm.0..=s.head_mm.1)
                .step_by(0.5)
                .text("head mm"),
        );
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.straighten, "Straighten");
            ui.label("Fill");
            ui.color_edit_button_srgb(&mut self.background)
                .on_hover_text("Colour for any area outside the photo");
        });
        let after = (
            self.spec,
            self.custom_size,
            self.custom_head,
            self.head_mm,
            self.straighten,
            self.background,
        );
        if before != after {
            self.result_dirty = true;
        }

        ui.add_space(6.0);
        self.result_preview(ui);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.radio_value(&mut self.output, Output::Sheet, "Print sheet");
            ui.radio_value(&mut self.output, Output::Single, "One photo");
        });
        match self.output {
            Output::Sheet => {
                egui::ComboBox::from_id_salt("passport-paper")
                    .width(200.0)
                    .selected_text(self.paper.name())
                    .show_ui(ui, |ui| {
                        for p in Paper::ALL {
                            ui.selectable_value(&mut self.paper, p, p.name());
                        }
                    });
                let (c, r, _) = passport::sheet_grid(&s, &SheetOptions::new(self.paper));
                ui.label(format!("{} copies at 300 dpi, with cutting guides", c * r));
            }
            Output::Single => {
                let (w, h) = s.single_pixels();
                let note = if s.digital_px.is_some() {
                    " (the official digital size)"
                } else {
                    ""
                };
                ui.label(format!("{w} x {h} px JPEG{note}"));
                ui.horizontal(|ui| {
                    let mut limit = self.max_kb.is_some();
                    if ui.checkbox(&mut limit, "At most").changed() {
                        self.max_kb = limit.then_some(s.max_kb.unwrap_or(250));
                    }
                    if let Some(kb) = &mut self.max_kb {
                        ui.add(egui::DragValue::new(kb).range(10..=10_000).suffix(" KB"));
                    }
                });
            }
        }
        ui.add_space(6.0);
        let ready = self.marks().is_some() && self.saving.is_none();
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("Save…")).clicked() {
                self.save();
            }
            if self.saving.is_some() {
                ui.spinner();
            }
        });
        if let Some(m) = &self.message {
            ui.label(m);
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Use a well-lit, front-facing photo with a plain background, \
                 and check the rules of the office you apply to.",
            )
            .small()
            .weak(),
        );
    }

    /// Live preview of the cropped photo with the head guides drawn over it.
    fn result_preview(&mut self, ui: &mut egui::Ui) {
        let s = self.spec();
        let size = Vec2::new(RESULT_H * s.width_mm / s.height_mm, RESULT_H);
        let marks = self.marks();
        if self.result_dirty {
            self.result_dirty = false;
            self.result = match (marks, &self.source) {
                (Some(m), Some(src)) => {
                    let ppp = ui.ctx().pixels_per_point();
                    let (w, h) = ((size.x * ppp).round() as u32, (size.y * ppp).round() as u32);
                    let layout = self.layout(m.scaled(src.scale));
                    let img = passport::render(&src.small, &s, &layout, w, h);
                    Some(ui.ctx().load_texture(
                        "passport-result",
                        rgb_to_color_image(&img),
                        egui::TextureOptions::LINEAR,
                    ))
                }
                _ => None,
            };
        }
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        match &self.result {
            Some(t) => {
                painter.image(
                    t.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Mark the head",
                    egui::FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        // Where the crown goes and the band the chin must fall in.
        let (top, chin) = passport::guides(&s);
        let y = |f: f32| rect.top() + f * rect.height();
        let guide = Color32::from_rgba_unmultiplied(0, 200, 120, 200);
        painter.hline(rect.x_range(), y(top), Stroke::new(1.0, guide));
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.x_range(), y(chin.0)..=y(chin.1)),
            0.0,
            Color32::from_rgba_unmultiplied(0, 200, 120, 40),
        );
        painter.vline(rect.center().x, rect.y_range(), Stroke::new(1.0, guide));
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, ui.visuals().weak_text_color()),
            egui::StrokeKind::Outside,
        );
    }

    fn save(&mut self) {
        let (Some(marks), Some(src)) = (self.marks(), &self.source) else {
            return;
        };
        let spec = self.spec();
        let kind = match self.output {
            Output::Sheet => "sheet",
            Output::Single => "passport",
        };
        let stem = src.path.file_stem().unwrap_or_default().to_string_lossy();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("JPEG", &["jpg", "jpeg"])
            .set_file_name(format!("{stem}-{kind}-{}.jpg", spec.id));
        if let Some(dir) = src.path.parent() {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut out) = dialog.save_file() else {
            return;
        };
        if out.extension().is_none() {
            out.set_extension("jpg");
        }
        let full = Arc::clone(&src.full);
        let layout = self.layout(marks);
        let sheet = (self.output == Output::Sheet).then_some(self.paper);
        let max_kb = if sheet.is_none() { self.max_kb } else { None };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(write(&full, &spec, &layout, sheet, max_kb, &out).map(|()| out));
        });
        self.message = None;
        self.saving = Some(rx);
    }
}

fn write(
    full: &DynamicImage,
    spec: &PhotoSpec,
    layout: &Layout,
    sheet: Option<Paper>,
    max_kb: Option<u32>,
    out: &Path,
) -> Result<(), String> {
    let (image, dpi): (RgbImage, f32) = match sheet {
        Some(paper) => {
            let opts = SheetOptions::new(paper);
            let (w, h) = spec.pixels_at(opts.dpi);
            let photo = passport::render(full, spec, layout, w, h);
            let (sheet, n) = passport::print_sheet(&photo, spec, &opts);
            if n == 0 {
                return Err(format!("the photo does not fit on {}", paper.name()));
            }
            (sheet, opts.dpi)
        }
        None => {
            let (w, h) = spec.single_pixels();
            let photo = passport::render(full, spec, layout, w, h);
            (photo, passport::dpi_of(h, spec.height_mm))
        }
    };
    let buf = passport::encode_jpeg(&image, dpi, max_kb).map_err(|e| e.to_string())?;
    std::fs::write(out, buf).map_err(|e| e.to_string())
}

fn to_color_image(rgba: &[u8], w: u32, h: u32) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba)
}

fn rgb_to_color_image(img: &RgbImage) -> ColorImage {
    ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw())
}

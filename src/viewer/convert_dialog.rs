//! The "Convert" window: the batch converter with the same engine as
//! `chtra convert`, running on a background thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use chtra::convert::{self, ConflictPolicy, ConvertOptions, Outcome, Report};
use chtra::encode;
use chtra::format::Format;
use chtra::resize::ResizeSpec;

use super::util::human_bytes;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Current,
    Folder,
}

struct Running {
    total: usize,
    done: Arc<AtomicUsize>,
    report: Arc<Mutex<Option<Report>>>,
}

pub struct ConvertDialog {
    pub open: bool,
    scope: Scope,
    format: Format,
    quality: u8,
    lossless: bool,
    resize_on: bool,
    resize_px: u32,
    out_dir: Option<PathBuf>,
    rename: String,
    conflict: ConflictPolicy,
    keep_metadata: bool,
    running: Option<Running>,
    last: Option<Report>,
    /// Set when a conversion finished, so the viewer can rescan its folder.
    pub finished: bool,
}

impl Default for ConvertDialog {
    fn default() -> Self {
        ConvertDialog {
            open: false,
            scope: Scope::Current,
            format: Format::WebP,
            quality: 90,
            lossless: false,
            resize_on: false,
            resize_px: 2048,
            out_dir: None,
            rename: "{name}".into(),
            conflict: ConflictPolicy::Rename,
            keep_metadata: true,
            running: None,
            last: None,
            finished: false,
        }
    }
}

impl ConvertDialog {
    pub fn show(&mut self, ctx: &egui::Context, current: Option<&PathBuf>, folder: &[PathBuf]) {
        if !self.open {
            return;
        }
        self.poll();
        let mut open = self.open;
        egui::Window::new("Convert")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| self.contents(ui, current, folder));
        // Closing the window while converting just hides it; work continues.
        self.open = open;
        if self.running.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn poll(&mut self) {
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

    fn contents(&mut self, ui: &mut egui::Ui, current: Option<&PathBuf>, folder: &[PathBuf]) {
        let busy = self.running.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("convert-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Files");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.scope, Scope::Current, "This photo");
                        ui.radio_value(
                            &mut self.scope,
                            Scope::Folder,
                            format!("All {} in folder", folder.len()),
                        );
                    });
                    ui.end_row();

                    ui.label("Format");
                    egui::ComboBox::from_id_salt("convert-format")
                        .selected_text(self.format.name())
                        .show_ui(ui, |ui| {
                            for f in Format::WRITABLE {
                                ui.selectable_value(&mut self.format, f, f.name());
                            }
                        });
                    ui.end_row();

                    ui.label("Quality");
                    ui.horizontal(|ui| {
                        let lossy = encode::is_lossy(self.format, self.lossless);
                        if lossy || self.format == Format::Gif {
                            ui.add(egui::Slider::new(&mut self.quality, 1..=100));
                        } else {
                            ui.weak(match self.format {
                                Format::Bmp => "Uncompressed",
                                _ => "Lossless",
                            });
                        }
                        if encode::LOSSY_ENCODERS
                            && matches!(self.format, Format::WebP | Format::JpegXl)
                        {
                            ui.checkbox(&mut self.lossless, "Lossless");
                        }
                    });
                    ui.end_row();

                    ui.label("Resize");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.resize_on, "Longest side");
                        ui.add_enabled(
                            self.resize_on,
                            egui::DragValue::new(&mut self.resize_px)
                                .range(16..=65_535)
                                .suffix(" px"),
                        );
                    });
                    ui.end_row();

                    ui.label("Save to");
                    ui.horizontal(|ui| {
                        let label = match &self.out_dir {
                            Some(d) => d.display().to_string(),
                            None => "Same folder as each photo".to_string(),
                        };
                        ui.label(label);
                        if ui.button("Choose…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.out_dir = Some(dir);
                            }
                        }
                        if self.out_dir.is_some() && ui.small_button("Reset").clicked() {
                            self.out_dir = None;
                        }
                    });
                    ui.end_row();

                    ui.label("File name");
                    ui.add(egui::TextEdit::singleline(&mut self.rename).hint_text("{name}"))
                        .on_hover_text("Tokens: {name} {parent} {n} {n:03}");
                    ui.end_row();

                    ui.label("If file exists");
                    egui::ComboBox::from_id_salt("convert-conflict")
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

        ui.separator();
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
            Scope::Current => current.cloned().into_iter().collect(),
            Scope::Folder => folder.to_vec(),
        };
        ui.horizontal(|ui| {
            let label = format!(
                "Convert {} file{}",
                inputs.len(),
                if inputs.len() == 1 { "" } else { "s" }
            );
            if ui
                .add_enabled(!inputs.is_empty(), egui::Button::new(label))
                .clicked()
            {
                self.start(inputs);
            }
        });

        if let Some(report) = &self.last {
            let (bin, bout) = report.bytes();
            ui.label(format!(
                "Done in {:.1} s: {} converted, {} skipped, {} failed ({} to {}).",
                report.elapsed.as_secs_f64(),
                report.converted(),
                report.skipped(),
                report.failed(),
                human_bytes(bin),
                human_bytes(bout),
            ));
            if report.failed() > 0 {
                egui::ScrollArea::vertical()
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
        opts.resize = self
            .resize_on
            .then_some(ResizeSpec::LongestSide(self.resize_px));
        opts.out_dir = self.out_dir.clone();
        opts.rename = if self.rename.trim().is_empty() {
            "{name}".into()
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

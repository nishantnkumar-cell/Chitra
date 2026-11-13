//! Printing the current photo or a passport sheet.
//!
//! Windows uses its own Print dialog and GDI; Linux uses the desktop's print
//! dialog through the portal (Flatpak included). macOS, and Linux desktops
//! without a print portal, get Chitra's own small dialog and CUPS.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod cups;
#[cfg(target_os = "linux")]
mod portal;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use image::RgbImage;

use chitra::print::{self as layout, Paper, Rect, Size};

/// Something to print.
pub struct Job {
    /// Name of the print job, shown in the printer queue.
    pub title: String,
    /// Upright and ready for paper (see [`chitra::print::prepare`]).
    pub image: RgbImage,
    pub size: Size,
}

enum Msg {
    /// Finished; the text goes in a toast.
    Done(Result<String, String>),
    /// Chitra's own dialog is needed: the job and the printers CUPS knows.
    #[cfg_attr(windows, allow(dead_code))]
    Ask(Job, Vec<String>, Option<String>),
}

/// Chitra's own print dialog (macOS, and Linux without a portal).
struct Dialog {
    job: Job,
    printers: Vec<String>,
    printer: usize,
    paper: Paper,
    fill: bool,
    copies: u32,
}

#[derive(Default)]
pub struct Printing {
    rx: Option<Receiver<Msg>>,
    dialog: Option<Dialog>,
    /// The main window, so the Windows Print dialog opens over it.
    pub owner: isize,
}

impl Printing {
    /// Printing is under way or its dialog is open.
    pub fn busy(&self) -> bool {
        self.rx.is_some() || self.dialog.is_some()
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Prepare the job on a background thread with `build`, then print it.
    pub fn start(
        &mut self,
        ctx: &egui::Context,
        build: impl FnOnce() -> Result<Job, String> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let owner = self.owner;
        std::thread::spawn(move || {
            let msg = match build() {
                Ok(job) => run(job, owner),
                Err(e) => Msg::Done(Err(e)),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
        self.rx = Some(rx);
    }

    /// Draw the dialog, if open. Returns a message for a toast when a job ends.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<String> {
        let mut toast = None;
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Msg::Done(r)) => {
                    self.rx = None;
                    toast = Some(r.unwrap_or_else(|e| format!("Could not print: {e}")));
                }
                Ok(Msg::Ask(job, printers, default)) => {
                    self.rx = None;
                    self.dialog = Some(Dialog::new(job, printers, default));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.rx = None,
            }
        }
        if let Some(d) = &mut self.dialog {
            match d.show(ctx) {
                Action::None => {}
                Action::Cancel => self.dialog = None,
                Action::SavePdf => {
                    if let Some(d) = self.dialog.take() {
                        toast = Some(d.save_pdf());
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                Action::Print => {
                    if let Some(d) = self.dialog.take() {
                        let (tx, rx) = mpsc::channel();
                        let ctx = ctx.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Done(d.print()));
                            ctx.request_repaint();
                        });
                        self.rx = Some(rx);
                    }
                }
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                Action::Print => {}
            }
        }
        toast
    }
}

/// Print `job` with the platform's own dialog where there is one.
#[cfg(windows)]
fn run(job: Job, owner: isize) -> Msg {
    Msg::Done(
        crate::platform::print_windows::print(owner, &job.title, &job.image, job.size).map(
            |printed| {
                if printed {
                    "Sent to the printer"
                } else {
                    "Printing cancelled"
                }
                .into()
            },
        ),
    )
}

#[cfg(target_os = "linux")]
fn run(job: Job, _owner: isize) -> Msg {
    let result = portal::print(&job.title, |setup| {
        let area = Rect::new(0.0, 0.0, setup.width_mm, setup.height_mm).inset(setup.margin_mm);
        pdf_for(&job, setup.width_mm, setup.height_mm, area)
    });
    match result {
        Ok(portal::Outcome::Printed) => Msg::Done(Ok("Sent to the printer".into())),
        Ok(portal::Outcome::Cancelled) => Msg::Done(Ok("Printing cancelled".into())),
        Err(portal::Error::Failed(e)) => Msg::Done(Err(e)),
        Err(portal::Error::Unavailable) => {
            let (printers, default) = cups::printers();
            Msg::Ask(job, printers, default)
        }
    }
}

#[cfg(target_os = "macos")]
fn run(job: Job, _owner: isize) -> Msg {
    let (printers, default) = cups::printers();
    Msg::Ask(job, printers, default)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn run(job: Job, _owner: isize) -> Msg {
    Msg::Ask(job, Vec::new(), None)
}

#[cfg(target_os = "linux")]
fn pdf_for(job: &Job, paper_w: f32, paper_h: f32, area: Rect) -> Result<Vec<u8>, String> {
    let p = layout::place(
        job.image.width(),
        job.image.height(),
        job.size,
        paper_w,
        paper_h,
        area,
    );
    layout::pdf(&job.image, paper_w, paper_h, &p).map_err(|e| e.to_string())
}

enum Action {
    None,
    Cancel,
    Print,
    SavePdf,
}

impl Dialog {
    fn new(job: Job, printers: Vec<String>, default: Option<String>) -> Dialog {
        let printer = default
            .and_then(|d| printers.iter().position(|p| *p == d))
            .unwrap_or(0);
        let paper = match job.size {
            Size::Exact {
                width_mm,
                height_mm,
            } => Paper::matching(width_mm, height_mm),
            _ => None,
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let paper = paper.unwrap_or_else(cups::local_paper);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let paper = paper.unwrap_or(Paper::A4);
        Dialog {
            job,
            printers,
            printer,
            paper,
            fill: false,
            copies: 1,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn exact(&self) -> bool {
        matches!(self.job.size, Size::Exact { .. })
    }

    fn show(&mut self, ctx: &egui::Context) -> Action {
        let mut action = Action::None;
        let mut open = true;
        egui::Window::new("Print")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("print-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Printer");
                        if self.printers.is_empty() {
                            ui.label(egui::RichText::new("No printers found").weak());
                        } else {
                            egui::ComboBox::from_id_salt("print-printer")
                                .width(220.0)
                                .selected_text(&self.printers[self.printer])
                                .show_index(ui, &mut self.printer, self.printers.len(), |i| {
                                    self.printers[i].clone()
                                });
                        }
                        ui.end_row();

                        ui.label("Paper");
                        egui::ComboBox::from_id_salt("print-paper")
                            .width(220.0)
                            .selected_text(self.paper.name())
                            .show_ui(ui, |ui| {
                                for p in Paper::ALL {
                                    ui.selectable_value(&mut self.paper, p, p.name());
                                }
                            });
                        ui.end_row();

                        ui.label("Size");
                        match self.job.size {
                            Size::Exact {
                                width_mm,
                                height_mm,
                            } => {
                                ui.label(format!("Actual size, {width_mm:.0} x {height_mm:.0} mm"));
                            }
                            _ => {
                                ui.horizontal(|ui| {
                                    ui.radio_value(&mut self.fill, false, "Fit to page")
                                        .on_hover_text("The whole photo, with a small border");
                                    ui.radio_value(&mut self.fill, true, "Fill page")
                                        .on_hover_text(
                                            "Edge to edge; the sides of the photo may be cut off",
                                        );
                                });
                            }
                        }
                        ui.end_row();

                        ui.label("Copies");
                        ui.add(egui::DragValue::new(&mut self.copies).range(1..=99));
                        ui.end_row();
                    });
                if let Size::Exact {
                    width_mm,
                    height_mm,
                } = self.job.size
                {
                    let (w, h) = self.paper.size_mm();
                    let fits = (width_mm <= w + 0.5 && height_mm <= h + 0.5)
                        || (height_mm <= w + 0.5 && width_mm <= h + 0.5);
                    if !fits {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "This does not fit on that paper.",
                        );
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can_print = !self.printers.is_empty();
                    if ui
                        .add_enabled(can_print, egui::Button::new("Print"))
                        .clicked()
                    {
                        action = Action::Print;
                    }
                    if ui.button("Save as PDF…").clicked() {
                        action = Action::SavePdf;
                    }
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                });
            });
        if !open || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            action = Action::Cancel;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            && !self.printers.is_empty()
        {
            action = Action::Print;
        }
        action
    }

    fn pdf(&self) -> Result<Vec<u8>, String> {
        let (w, h) = self.paper.size_mm();
        let page = Rect::new(0.0, 0.0, w, h);
        let size = match self.job.size {
            Size::Fit if self.fill => Size::Fill,
            s => s,
        };
        let area = if size == Size::Fit {
            page.inset(layout::FIT_MARGIN_MM)
        } else {
            page
        };
        let p = layout::place(
            self.job.image.width(),
            self.job.image.height(),
            size,
            w,
            h,
            area,
        );
        layout::pdf(&self.job.image, w, h, &p).map_err(|e| e.to_string())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn print(self) -> Result<String, String> {
        let pdf = self.pdf()?;
        let path = std::env::temp_dir().join(format!("chitra-print-{}.pdf", std::process::id()));
        std::fs::write(&path, pdf).map_err(|e| e.to_string())?;
        let printer = &self.printers[self.printer];
        let r = cups::submit(
            &path,
            printer,
            self.paper,
            self.copies,
            self.exact(),
            &self.job.title,
        );
        let _ = std::fs::remove_file(&path);
        r.map(|()| format!("Sent to {printer}"))
    }

    fn save_pdf(self) -> String {
        let stem = self
            .job
            .title
            .rsplit_once('.')
            .map_or(&*self.job.title, |(s, _)| s);
        let Some(mut out): Option<PathBuf> = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"))
            .save_file()
        else {
            return "Not saved".into();
        };
        if out.extension().is_none() {
            out.set_extension("pdf");
        }
        match self
            .pdf()
            .and_then(|pdf| std::fs::write(&out, pdf).map_err(|e| e.to_string()))
        {
            Ok(()) => format!(
                "Saved {}",
                out.file_name().unwrap_or_default().to_string_lossy()
            ),
            Err(e) => format!("Could not save the PDF: {e}"),
        }
    }
}

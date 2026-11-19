//! The About window (version, project links, licences) and software updates
//! (checking GitHub for a newer release, downloading and installing it).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{SystemTime, UNIX_EPOCH};

use chitra::update::{self, Install, Release};
use eframe::egui::{self, RichText};

use super::util::human_bytes;

/// Automatic checks happen at most this often.
const CHECK_EVERY_SECS: u64 = 24 * 60 * 60;

const COPYRIGHT: &str = "Copyright © 2026 Nishant";

/// Libraries built into Chitra, and other software it uses. Every other
/// library in the build is under a permissive licence (MIT, Apache-2.0,
/// BSD, Zlib or similar).
const CREDITS: &[(&str, &str, &str)] = &[
    ("egui / eframe", "User interface", "MIT or Apache-2.0"),
    (
        "image",
        "JPEG, PNG, GIF, BMP, WebP decoding",
        "MIT or Apache-2.0",
    ),
    ("jxl-rs", "JPEG XL decoding", "BSD-3-Clause"),
    (
        "zune-jpegxl",
        "Lossless JPEG XL encoding",
        "MIT, Apache-2.0 or Zlib",
    ),
    (
        "jxl-encoder, zenwebp",
        "Lossy JPEG XL and WebP encoding",
        "AGPL-3.0",
    ),
    ("heifer", "HEIC decoding", "MIT or Apache-2.0"),
    ("oxideav-avif", "AVIF decoding", "MIT"),
    ("fast_image_resize", "Resizing", "MIT or Apache-2.0"),
    ("kamadak-exif", "EXIF metadata", "BSD-2-Clause"),
    (
        "wgpu",
        "Graphics card access for AI upscaling",
        "MIT or Apache-2.0",
    ),
    (
        "ndarray",
        "AI upscaling on the processor",
        "MIT or Apache-2.0",
    ),
    ("rayon", "Multi-core processing", "MIT or Apache-2.0"),
    ("rfd", "File dialogs", "MIT"),
    ("trash", "Recycle Bin", "MIT"),
    ("arboard", "Clipboard", "MIT or Apache-2.0"),
    (
        "Ubuntu, Noto, Hack fonts",
        "Text",
        "Ubuntu Font Licence, OFL-1.1",
    ),
    (
        "Real-ESRGAN models",
        "AI upscaling (optional download)",
        "BSD-3-Clause",
    ),
    (
        "System video decoders",
        "GStreamer (LGPL-2.1), Media Foundation or AVFoundation",
        "part of the system",
    ),
    ("curl", "Downloads", "part of the system"),
];

enum State {
    Idle,
    Checking(Receiver<Result<Release, String>>),
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        rx: Receiver<Result<PathBuf, String>>,
        part: PathBuf,
    },
    /// The Windows installer is running; Chitra closes.
    Installing,
    /// Saved to the Downloads folder (Linux tarball).
    Saved(PathBuf),
    Failed(String),
}

pub struct About {
    pub open: bool,
    /// The "Software update" window.
    pub updates_open: bool,
    state: State,
    install: Install,
    icon: Option<egui::TextureHandle>,
}

impl Default for About {
    fn default() -> About {
        About {
            open: false,
            updates_open: false,
            state: State::Idle,
            install: Install::current(),
            icon: None,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "Chitra 0.3.0 (Windows x86_64)".
pub fn version_line() -> String {
    format!(
        "Chitra {} ({} {})",
        update::VERSION,
        match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "macOS",
            "linux" => "Linux",
            other => other,
        },
        std::env::consts::ARCH
    )
}

fn open_link(url: &str) {
    let _ = update::open(url);
}

impl About {
    /// The newer version, once an automatic or manual check found one.
    pub fn available(&self) -> Option<&str> {
        match &self.state {
            State::Available(r) => Some(&r.version),
            _ => None,
        }
    }

    pub fn can_check(&self) -> bool {
        self.install.checks()
    }

    /// Check now and show the result.
    pub fn check_now(&mut self, ctx: &egui::Context) {
        self.updates_open = true;
        if matches!(self.state, State::Checking(_) | State::Downloading { .. }) {
            return;
        }
        if !self.install.checks() {
            self.state = State::Failed(
                "This copy of Chitra is a Flatpak, which cannot reach the internet. \
                 Get new versions from the download page."
                    .into(),
            );
            return;
        }
        self.start(ctx);
    }

    /// Check quietly when the setting allows and a day has passed. Returns
    /// true when a check started (the caller saves the time).
    pub fn auto_check(&mut self, ctx: &egui::Context, enabled: bool, last: u64) -> bool {
        let due = now().saturating_sub(last) >= CHECK_EVERY_SECS;
        if !enabled || !due || !self.install.checks() || !matches!(self.state, State::Idle) {
            return false;
        }
        self.start(ctx);
        true
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::latest());
            ctx.request_repaint();
        });
        self.state = State::Checking(rx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        match &self.state {
            State::Checking(rx) => match rx.try_recv() {
                Ok(Ok(r)) if update::is_newer(&r.version, update::VERSION) => {
                    self.state = State::Available(r);
                }
                Ok(Ok(_)) => self.state = State::UpToDate,
                Ok(Err(e)) => self.state = State::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.state = State::Idle,
            },
            State::Downloading { rx, .. } => match rx.try_recv() {
                Ok(Ok(path)) => {
                    if self.install == Install::WindowsInstaller {
                        match std::process::Command::new(&path).spawn() {
                            Ok(_) => {
                                self.state = State::Installing;
                                // The installer closes Chitra itself if it is
                                // still running, but going first is tidier.
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            Err(e) => {
                                self.state =
                                    State::Failed(format!("cannot start the installer: {e}"));
                            }
                        }
                    } else {
                        self.state = State::Saved(path);
                    }
                }
                Ok(Err(e)) => self.state = State::Failed(e),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
                Err(TryRecvError::Disconnected) => self.state = State::Idle,
            },
            _ => {}
        }
    }

    fn start_download(&mut self, ctx: &egui::Context, release: Release) {
        let Some(pkg) = release.package.clone() else {
            return;
        };
        let dir = update::download_dir(self.install);
        let part = update::part_path(&pkg, &dir);
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(update::download(&pkg, &dir));
            ctx.request_repaint();
        });
        self.state = State::Downloading { release, rx, part };
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        self.about_window(ctx);
        self.updates_window(ctx);
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let icon = self
            .icon
            .get_or_insert_with(|| {
                let data = super::util::icon();
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [data.width as usize, data.height as usize],
                    &data.rgba,
                );
                ctx.load_texture("about-icon", img, egui::TextureOptions::LINEAR)
            })
            .clone();
        let mut open = self.open;
        let mut check = false;
        egui::Window::new("About Chitra")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .max_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(&icon).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Chitra").size(28.0).strong());
                        ui.label(format!("Version {}", update::VERSION));
                        ui.label(
                            RichText::new("Photo viewer, enhancer and batch converter").weak(),
                        );
                    });
                });
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Check for updates…").clicked() {
                        check = true;
                    }
                    if ui
                        .button("Website")
                        .on_hover_text(update::HOMEPAGE)
                        .clicked()
                    {
                        open_link(update::HOMEPAGE);
                    }
                    if ui.button("User guide").clicked() {
                        open_link(update::USER_GUIDE);
                    }
                    if ui.button("Report a problem").clicked() {
                        open_link(update::ISSUES);
                    }
                });
                ui.add_space(10.0);
                ui.separator();
                ui.label(COPYRIGHT);
                ui.label(
                    "Chitra is free software under the GNU Affero General Public \
                     Licence, version 3 or later. It comes with no warranty.",
                );
                ui.horizontal(|ui| {
                    if ui.link("Licence text").clicked() {
                        open_link("https://www.gnu.org/licenses/agpl-3.0.html");
                    }
                    ui.label("·");
                    if ui.link("Source code").clicked() {
                        open_link(update::HOMEPAGE);
                    }
                    ui.label("·");
                    if ui.link("Release notes").clicked() {
                        open_link(update::RELEASES);
                    }
                });
                ui.add_space(6.0);
                egui::CollapsingHeader::new("Third-party software")
                    .id_salt("about-credits")
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                for (name, what, licence) in CREDITS {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.strong(*name);
                                        ui.label(
                                            RichText::new(format!("{what} · {licence}")).weak(),
                                        );
                                    });
                                }
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(
                                        "With thanks to the authors of the several hundred \
                                         smaller Rust libraries Chitra is built on, all under \
                                         permissive licences (MIT, Apache-2.0, BSD, Zlib and \
                                         similar).",
                                    )
                                    .small()
                                    .weak(),
                                );
                            });
                    });
                egui::CollapsingHeader::new("System information")
                    .id_salt("about-system")
                    .show(ui, |ui| {
                        let info = system_info();
                        egui::Grid::new("sysinfo")
                            .num_columns(2)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                for (k, v) in &info {
                                    ui.label(RichText::new(*k).weak());
                                    ui.label(v);
                                    ui.end_row();
                                }
                            });
                        if ui
                            .button("Copy")
                            .on_hover_text("For a problem report")
                            .clicked()
                        {
                            let text: Vec<String> =
                                info.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                            ui.ctx().copy_text(text.join("\n"));
                        }
                    });
            });
        self.open = open;
        if check {
            self.check_now(ctx);
        }
    }

    fn updates_window(&mut self, ctx: &egui::Context) {
        if !self.updates_open {
            return;
        }
        let mut open = true;
        let mut action = None;
        egui::Window::new("Software update")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(400.0);
                match &self.state {
                    State::Idle | State::Checking(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking for updates…");
                        });
                    }
                    State::UpToDate => {
                        ui.label(RichText::new("Chitra is up to date").size(18.0).strong());
                        ui.label(format!(
                            "Version {} is the latest version.",
                            update::VERSION
                        ));
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            action = Some(Action::Close);
                        }
                    }
                    State::Available(r) => {
                        ui.label(
                            RichText::new(format!("Chitra {} is available", r.version))
                                .size(18.0)
                                .strong(),
                        );
                        ui.label(format!("You have version {}.", update::VERSION));
                        if !r.notes.is_empty() {
                            ui.add_space(6.0);
                            egui::ScrollArea::vertical()
                                .max_height(160.0)
                                .show(ui, |ui| ui.label(&r.notes));
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            match (&r.package, self.install) {
                                (Some(p), Install::WindowsInstaller) => {
                                    if ui
                                        .button(format!(
                                            "Download and install ({})",
                                            human_bytes(p.size)
                                        ))
                                        .on_hover_text(
                                            "Chitra closes while the installer runs; \
                                             your settings and models are kept",
                                        )
                                        .clicked()
                                    {
                                        action = Some(Action::Download);
                                    }
                                }
                                (Some(p), _) => {
                                    if ui
                                        .button(format!("Download ({})", human_bytes(p.size)))
                                        .on_hover_text("Saves to your Downloads folder")
                                        .clicked()
                                    {
                                        action = Some(Action::Download);
                                    }
                                }
                                (None, _) => {
                                    if ui.button("Open download page").clicked() {
                                        open_link(&r.page);
                                    }
                                }
                            }
                            if ui.button("What's new").clicked() {
                                open_link(&r.page);
                            }
                            if ui.button("Later").clicked() {
                                action = Some(Action::Close);
                            }
                        });
                    }
                    State::Downloading { release, part, .. } => {
                        let total = release.package.as_ref().map_or(0, |p| p.size);
                        let done = std::fs::metadata(part).map_or(0, |m| m.len());
                        ui.label(format!("Downloading Chitra {}…", release.version));
                        let frac = if total > 0 {
                            (done as f32 / total as f32).min(1.0)
                        } else {
                            0.0
                        };
                        ui.add(egui::ProgressBar::new(frac).text(format!(
                            "{} of {}",
                            human_bytes(done),
                            human_bytes(total)
                        )));
                    }
                    State::Installing => {
                        ui.label("Starting the installer…");
                    }
                    State::Saved(path) => {
                        ui.label(RichText::new("Download complete").size(18.0).strong());
                        ui.label(format!("Saved to {}", path.display()));
                        ui.label("Extract it and run install.sh inside to replace this version.");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Show in folder").clicked() {
                                if let Some(dir) = path.parent() {
                                    open_link(&dir.to_string_lossy());
                                }
                            }
                            if ui.button("Close").clicked() {
                                action = Some(Action::Close);
                            }
                        });
                    }
                    State::Failed(e) => {
                        ui.label(RichText::new("Could not update").size(18.0).strong());
                        ui.label(e);
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if self.install.checks() && ui.button("Try again").clicked() {
                                action = Some(Action::Retry);
                            }
                            if ui.button("Open download page").clicked() {
                                open_link(update::RELEASES);
                            }
                            if ui.button("Close").clicked() {
                                action = Some(Action::Close);
                            }
                        });
                    }
                }
            });
        match action {
            Some(Action::Close) => open = false,
            Some(Action::Retry) => self.start(ctx),
            Some(Action::Download) => {
                if let State::Available(r) = &self.state {
                    let r = r.clone();
                    self.start_download(ctx, r);
                }
            }
            None => {}
        }
        if !open {
            self.updates_open = false;
            // Keep "available" so the toolbar still offers it; forget the rest.
            if matches!(
                self.state,
                State::UpToDate | State::Failed(_) | State::Saved(_)
            ) {
                self.state = State::Idle;
            }
        }
    }
}

enum Action {
    Close,
    Retry,
    Download,
}

fn system_info() -> Vec<(&'static str, String)> {
    let mut v = vec![("Version", version_line())];
    let mut features = Vec::new();
    if cfg!(feature = "gpu") {
        features.push("AI on graphics card");
    }
    if cfg!(feature = "video") {
        features.push("video");
    }
    if chitra::encode::LOSSY_ENCODERS {
        features.push("lossy WebP/JXL saving");
    }
    v.push(("Built with", features.join(", ")));
    let install = match Install::current() {
        Install::WindowsInstaller => "Windows",
        Install::LinuxTarball => "Linux tarball",
        Install::Flatpak => "Flatpak",
        Install::Other => "other",
    };
    v.push(("Package", install.to_string()));
    if let Some(p) = super::settings::Settings::path() {
        v.push(("Settings", p.display().to_string()));
    }
    if let Some(d) = chitra::ai::models_dir() {
        v.push(("AI models", d.display().to_string()));
    }
    v
}

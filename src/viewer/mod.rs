//! The viewer: single-photo view with zoom/pan/rotate, a thumbnail browser,
//! an info panel, the convert dialog and the passport photo maker, built on
//! egui/eframe (OpenGL).

mod convert_dialog;
mod desktop_theme;
mod headerbar;
mod loader;
mod passport_dialog;
mod settings;
mod util;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Vec2};

use chitra::folder::{self, Entry, SortKey};
use chitra::format::Format;

use convert_dialog::ConvertDialog;
use desktop_theme::DesktopTheme;
use loader::{Kind, LoadedImage, Loader, Request};
use passport_dialog::PassportDialog;
use settings::{SLIDESHOW_SECS, Settings, THUMB_SIZES, Theme, WheelMode};
use util::{format_time, human_bytes};

const PREFETCH_AHEAD: usize = 2;
const PREFETCH_BEHIND: usize = 1;
/// Decoded neighbours kept around (each side) so going back is instant too.
const KEEP_RADIUS: usize = 3;
const THUMB_PX: u32 = 320;
/// Photos kept per sub-folder row in the folder overview.
const OVERVIEW_PHOTOS: usize = 40;
/// Folders searched for photos when a sub-folder only holds more folders.
const OVERVIEW_MAX_DIRS: usize = 200;
/// After the wheel moves to another photo, ignore it briefly so one flick of
/// a free-spinning wheel doesn't skip a dozen photos.
const WHEEL_PAUSE: Duration = Duration::from_millis(120);

pub fn run(path: Option<PathBuf>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Chitra")
            // Matches the Linux .desktop file so docks and taskbars show the right icon.
            .with_app_id("io.github.nishantnkumar_cell.Chitra")
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([360.0, 260.0])
            .with_drag_and_drop(true)
            .with_decorations(!headerbar::wanted())
            .with_icon(util::icon()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Chitra",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, path)))),
    )
    .map_err(|e| e.to_string())
}

enum Slot {
    Pending,
    Ready(Arc<LoadedImage>),
    Failed(String),
}

#[derive(Default)]
struct Cached {
    display: Option<Slot>,
    full: Option<Slot>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Zoom {
    /// Shrink large photos to fit the window; small ones stay at 100%.
    Fit,
    /// Screen pixels per image pixel.
    Scale(f32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Single,
    Grid,
    /// One row per sub-folder showing its first photos.
    Overview,
}

struct Scan {
    dir: PathBuf,
    entries: Vec<Entry>,
    has_subfolders: bool,
}

/// A sub-folder row in the folder overview.
struct FolderRow {
    dir: PathBuf,
    name: String,
    /// The first photos in the current sort order; when the folder has none
    /// of its own, the first ones found in its sub-folders.
    photos: Vec<Entry>,
    /// Photos directly in the folder.
    total: usize,
    subfolders: usize,
}

enum OverviewAction {
    OpenPhoto(PathBuf),
    Browse(Option<PathBuf>),
}

struct App {
    entries: Vec<Entry>,
    index: usize,
    dir: Option<PathBuf>,
    settings: Settings,
    /// What is on disk, to save only when something changed.
    saved_settings: Settings,
    show_settings: bool,
    scan_rx: Option<Receiver<Scan>>,
    rows_rx: Option<Receiver<FolderRow>>,
    rows: Vec<FolderRow>,
    has_subfolders: bool,
    /// Show the overview once the scan finds sub-folders (set when a folder
    /// is opened, not on refresh).
    overview_auto: bool,
    ctx: egui::Context,

    loader: Loader,
    thumb_loader: Loader,
    cache: HashMap<PathBuf, Cached>,
    thumbs: HashMap<PathBuf, Slot>,

    mode: Mode,
    zoom: Zoom,
    pan: Vec2,
    /// Quarter turns clockwise applied to the view.
    rotation: u8,
    shown_at: Instant,

    show_info: bool,
    show_help: bool,
    slideshow: Option<Instant>,
    wheel_acc: f32,
    wheel_quiet_until: Instant,
    confirm_delete: Option<PathBuf>,
    toast: Option<(String, Instant)>,
    convert: ConvertDialog,
    passport: PassportDialog,

    grid_offset: f32,
    grid_view_h: f32,
    grid_follow: bool,

    grid_cols: usize,
    pending_zoom: Option<f32>,
    last_scale: f32,

    title: String,
    display_max: u32,
    max_texture: u32,
    desktop_theme: DesktopTheme,
    /// Our own title bar (GNOME on Wayland); see headerbar.rs.
    headerbar: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> App {
        let ctx = cc.egui_ctx.clone();
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let settings = Settings::load();
        ctx.set_theme(settings.theme.preference());
        let mut app = App {
            desktop_theme: DesktopTheme::watch(&ctx),
            headerbar: headerbar::wanted(),
            entries: Vec::new(),
            index: 0,
            dir: None,
            saved_settings: settings.clone(),
            settings,
            show_settings: false,
            scan_rx: None,
            rows_rx: None,
            rows: Vec::new(),
            has_subfolders: false,
            overview_auto: false,
            ctx: ctx.clone(),
            // Two workers for the photo being viewed and its neighbours (each
            // decode is itself multithreaded); the rest for thumbnails.
            loader: Loader::new(ctx.clone(), 2, "decode"),
            thumb_loader: Loader::new(ctx.clone(), cores.clamp(2, 8), "thumb"),
            cache: HashMap::new(),
            thumbs: HashMap::new(),
            mode: Mode::Single,
            zoom: Zoom::Fit,
            pan: Vec2::ZERO,
            rotation: 0,
            shown_at: Instant::now(),
            show_info: false,
            show_help: false,
            slideshow: None,
            wheel_acc: 0.0,
            wheel_quiet_until: Instant::now(),
            confirm_delete: None,
            toast: None,
            convert: ConvertDialog::default(),
            passport: PassportDialog::default(),
            grid_offset: 0.0,
            grid_view_h: 0.0,
            grid_follow: false,
            grid_cols: 1,
            pending_zoom: None,
            last_scale: 1.0,
            title: String::new(),
            display_max: 3840,
            max_texture: 8192,
        };
        app.update_limits(&ctx);
        if let Some(path) = path {
            app.open(path);
        }
        app
    }

    // ----- folder and navigation -------------------------------------------------

    fn current(&self) -> Option<&Entry> {
        self.entries.get(self.index)
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|e| e.path.clone())
    }

    fn open(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path)
            .map(simplify_path)
            .unwrap_or(path);
        self.cache.clear();
        self.thumbs.clear();
        self.slideshow = None;
        if path.is_dir() {
            self.entries.clear();
            self.index = 0;
            self.start_scan(path.clone());
            self.dir = Some(path);
            self.mode = Mode::Grid;
            self.overview_auto = self.settings.folder_overview;
        } else {
            self.entries = vec![Entry::from_path(path.clone())];
            self.index = 0;
            self.mode = Mode::Single;
            self.overview_auto = false;
            if let Some(dir) = path.parent() {
                self.start_scan(dir.to_path_buf());
                self.dir = Some(dir.to_path_buf());
            }
        }
        self.reset_view();
        self.schedule();
    }

    /// List the folder's photos, then its sub-folders' first photos for the
    /// overview, on a background thread. Replacing the receivers stops an
    /// older scan at its next send.
    fn start_scan(&mut self, dir: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let (row_tx, row_rx) = mpsc::channel();
        let (sort, desc) = (self.settings.sort, self.settings.sort_desc);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut entries = folder::list_images(&dir).unwrap_or_default();
            folder::sort(&mut entries, sort, desc);
            let subdirs = folder::list_subfolders(&dir);
            let has_subfolders = !subdirs.is_empty();
            if tx
                .send(Scan {
                    dir,
                    entries,
                    has_subfolders,
                })
                .is_err()
            {
                return;
            }
            ctx.request_repaint();
            for sub in subdirs {
                let mut photos = folder::list_images(&sub).unwrap_or_default();
                folder::sort(&mut photos, sort, desc);
                let total = photos.len();
                photos.truncate(OVERVIEW_PHOTOS);
                let subfolders = folder::list_subfolders(&sub).len();
                if total == 0 && subfolders > 0 {
                    photos =
                        folder::images_below(&sub, OVERVIEW_PHOTOS, OVERVIEW_MAX_DIRS, sort, desc);
                }
                if photos.is_empty() {
                    continue;
                }
                let row = FolderRow {
                    name: sub
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    subfolders,
                    dir: sub,
                    photos,
                    total,
                };
                if row_tx.send(row).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.scan_rx = Some(rx);
        self.rows_rx = Some(row_rx);
        self.rows.clear();
    }

    fn poll_rows(&mut self) {
        let Some(rx) = &self.rows_rx else { return };
        loop {
            match rx.try_recv() {
                Ok(row) => self.rows.push(row),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.rows_rx = None;
                    break;
                }
            }
        }
    }

    fn rescan(&mut self) {
        if let Some(dir) = self.dir.clone() {
            self.start_scan(dir);
        }
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else { return };
        let Ok(scan) = rx.try_recv() else { return };
        self.scan_rx = None;
        if Some(&scan.dir) != self.dir.as_ref() {
            return;
        }
        let current = self.current_path();
        // Keep a file opened directly even if its extension is unusual.
        let mut entries = scan.entries;
        if let Some(cur) = &current {
            if !entries.iter().any(|e| &e.path == cur) && cur.exists() {
                entries.push(Entry::from_path(cur.clone()));
                folder::sort(&mut entries, self.settings.sort, self.settings.sort_desc);
            }
        }
        self.entries = entries;
        self.index = current
            .and_then(|c| self.entries.iter().position(|e| e.path == c))
            .unwrap_or(0);
        self.has_subfolders = scan.has_subfolders;
        if std::mem::take(&mut self.overview_auto) && scan.has_subfolders {
            self.mode = Mode::Overview;
        } else if self.mode == Mode::Overview && !scan.has_subfolders {
            self.mode = Mode::Grid;
        }
        self.grid_follow = true;
        self.schedule();
    }

    fn resort(&mut self) {
        if self.has_subfolders {
            // Rows only hold each sub-folder's first photos, so list them again.
            self.rescan();
        }
        let current = self.current_path();
        folder::sort(
            &mut self.entries,
            self.settings.sort,
            self.settings.sort_desc,
        );
        self.index = current
            .and_then(|c| self.entries.iter().position(|e| e.path == c))
            .unwrap_or(0);
        self.grid_follow = true;
        self.schedule();
    }

    fn go_to(&mut self, index: usize) {
        if self.entries.is_empty() {
            return;
        }
        let index = index.min(self.entries.len() - 1);
        if index != self.index {
            self.index = index;
            self.reset_view();
            self.grid_follow = true;
        }
        self.schedule();
    }

    fn step(&mut self, delta: isize) {
        let n = self.entries.len() as isize;
        if n == 0 {
            return;
        }
        let next = (self.index as isize + delta).rem_euclid(n) as usize;
        self.go_to(next);
    }

    fn reset_view(&mut self) {
        self.zoom = Zoom::Fit;
        self.pan = Vec2::ZERO;
        self.rotation = 0;
        self.shown_at = Instant::now();
    }

    fn update_limits(&mut self, ctx: &egui::Context) {
        let (monitor, ppp, max_tex) = ctx.input(|i| {
            (
                i.viewport().monitor_size,
                i.pixels_per_point,
                i.max_texture_side,
            )
        });
        self.max_texture = (max_tex as u32).max(2048);
        let screen = monitor.map_or(3840.0, |m| m.x.max(m.y) * ppp);
        self.display_max = (screen.ceil() as u32).clamp(1024, self.max_texture);
    }

    /// Decide what should be decoded: the current photo first, then its
    /// neighbours. Anything else is cancelled or released.
    fn schedule(&mut self) {
        let n = self.entries.len();
        if n == 0 {
            self.loader.retain(|_, _| false);
            return;
        }
        let i = self.index;
        let mut want: Vec<PathBuf> = vec![self.entries[i].path.clone()];
        for k in 1..=PREFETCH_AHEAD {
            want.push(self.entries[(i + k) % n].path.clone());
        }
        for k in 1..=PREFETCH_BEHIND {
            want.push(self.entries[(i + n - k) % n].path.clone());
        }
        let mut seen = HashSet::new();
        want.retain(|p| seen.insert(p.clone()));

        let keep: HashSet<PathBuf> = (0..=2 * KEEP_RADIUS)
            .map(|k| {
                self.entries[(i + n + k - KEEP_RADIUS.min(n)) % n]
                    .path
                    .clone()
            })
            .chain(want.iter().cloned())
            .collect();
        let want_set: HashSet<&PathBuf> = want.iter().collect();
        let current = &self.entries[i].path;

        self.cache.retain(|p, _| keep.contains(p));
        for (p, c) in self.cache.iter_mut() {
            if p != current {
                c.full = None;
            }
            if matches!(c.display, Some(Slot::Pending)) && !want_set.contains(p) {
                c.display = None;
            }
        }
        self.loader.retain(|p, k| match k {
            Kind::Display => want_set.contains(&p.to_path_buf()),
            Kind::Full => p == current.as_path(),
            Kind::Thumb => false,
        });

        // While browsing the grid only the selected photo is prefetched.
        let count = if self.mode == Mode::Grid {
            1
        } else {
            want.len()
        };
        for (j, path) in want.into_iter().take(count).enumerate() {
            let entry = self.cache.entry(path.clone()).or_default();
            if matches!(entry.display, None | Some(Slot::Pending)) {
                entry.display = Some(Slot::Pending);
                self.loader.request(
                    Request {
                        path,
                        kind: Kind::Display,
                        max_side: self.display_max,
                    },
                    j == 0,
                );
            }
        }
    }

    fn request_full(&mut self, path: &Path) {
        let entry = self.cache.entry(path.to_path_buf()).or_default();
        if entry.full.is_none() {
            entry.full = Some(Slot::Pending);
            self.loader.request(
                Request {
                    path: path.to_path_buf(),
                    kind: Kind::Full,
                    max_side: self.max_texture,
                },
                true,
            );
        }
    }

    fn poll_loader(&mut self) {
        for loaded in self.loader.poll() {
            let slot = match loaded.result {
                Ok(img) => Slot::Ready(img),
                Err(e) => Slot::Failed(e),
            };
            if let Some(c) = self.cache.get_mut(&loaded.path) {
                match loaded.kind {
                    Kind::Display => c.display = Some(slot),
                    Kind::Full => c.full = Some(slot),
                    Kind::Thumb => {}
                }
            }
            if Some(&loaded.path) == self.current().map(|e| &e.path) {
                self.shown_at = Instant::now();
            }
        }
        for loaded in self.thumb_loader.poll() {
            if let Some(entry) = self.thumbs.get_mut(&loaded.path) {
                *entry = match loaded.result {
                    Ok(img) => Slot::Ready(img),
                    Err(e) => Slot::Failed(e),
                };
            }
        }
    }

    // ----- actions ----------------------------------------------------------------

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn open_dialog(&mut self) {
        let exts: Vec<&str> = Format::ALL
            .iter()
            .flat_map(|f| f.extensions().iter().copied())
            .collect();
        let mut dialog = rfd::FileDialog::new().add_filter("Images", &exts);
        if let Some(dir) = &self.dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(path);
        }
    }

    fn open_folder_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = &self.dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_folder() {
            self.open(path);
        }
    }

    fn delete_current(&mut self) {
        let Some(path) = self.confirm_delete.take() else {
            return;
        };
        match trash::delete(&path) {
            Ok(()) => {
                self.cache.remove(&path);
                self.thumbs.remove(&path);
                if let Some(pos) = self.entries.iter().position(|e| e.path == path) {
                    self.entries.remove(pos);
                    if self.index >= self.entries.len() && self.index > 0 {
                        self.index = self.entries.len() - 1;
                    }
                }
                self.reset_view();
                self.schedule();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.toast(format!("Moved {name} to the Recycle Bin"));
            }
            Err(e) => self.toast(format!("Could not delete: {e}")),
        }
    }

    fn copy_image(&mut self) {
        let Some(path) = self.current_path() else {
            return;
        };
        let result = chitra::decode_file(&path, chitra::DecodeOptions::default())
            .map_err(|e| e.to_string())
            .and_then(|d| {
                let rgba = d.image.to_rgba8();
                let data = arboard::ImageData {
                    width: rgba.width() as usize,
                    height: rgba.height() as usize,
                    bytes: rgba.into_raw().into(),
                };
                arboard::Clipboard::new()
                    .and_then(|mut c| c.set_image(data))
                    .map_err(|e| e.to_string())
            });
        match result {
            Ok(()) => self.toast("Image copied"),
            Err(e) => self.toast(format!("Could not copy: {e}")),
        }
    }

    fn toggle_fullscreen(&self, ctx: &egui::Context) {
        let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Single | Mode::Overview => Mode::Grid,
            Mode::Grid => Mode::Single,
        };
        self.grid_follow = true;
        self.schedule();
    }

    // ----- input ------------------------------------------------------------------

    fn handle_input(&mut self, ctx: &egui::Context) {
        if let Some(path) = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .find(|p| !p.as_os_str().is_empty())
        }) {
            self.open(path);
        }
        if self.confirm_delete.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let none = Modifiers::NONE;
        let cmd = Modifiers::COMMAND;
        let shift = Modifiers::SHIFT;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;

        // Shortcuts with extra modifiers first: a plain pattern also matches
        // presses that add Shift.
        if key(cmd_shift, Key::C) {
            if let Some(p) = self.current_path() {
                ctx.copy_text(p.display().to_string());
                self.toast("Path copied");
            }
        }
        if key(cmd, Key::C) {
            self.copy_image();
        }
        if key(cmd, Key::O) {
            self.open_dialog();
        }
        if key(cmd_shift, Key::O) {
            self.open_folder_dialog();
        }
        if key(cmd, Key::Comma) {
            self.show_settings = !self.show_settings;
        }
        if key(cmd, Key::S) {
            self.convert.open = true;
        }
        if key(cmd, Key::P) {
            self.passport.open_on(self.current_path());
        }
        if key(cmd, Key::Q) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if key(shift, Key::R) || key(none, Key::L) {
            self.rotation = (self.rotation + 3) % 4;
        }
        if key(none, Key::R) {
            self.rotation = (self.rotation + 1) % 4;
        }
        // Ctrl+Cmd+F is the macOS full-screen shortcut; F11 shows the desktop there.
        if key(none, Key::F11)
            || key(none, Key::F)
            || key(Modifiers::MAC_CMD | Modifiers::CTRL, Key::F)
        {
            self.toggle_fullscreen(ctx);
        }
        if key(none, Key::F1) || key(shift, Key::Questionmark) || key(none, Key::Questionmark) {
            self.show_help = !self.show_help;
        }
        if key(none, Key::I) {
            self.show_info = !self.show_info;
        }
        if key(none, Key::G) {
            self.toggle_mode();
        }
        if key(none, Key::S) {
            self.slideshow = match self.slideshow {
                Some(_) => None,
                None => Some(Instant::now()),
            };
        }
        if key(none, Key::OpenBracket) {
            self.set_slideshow_secs(self.settings.slideshow_secs.saturating_sub(1));
        }
        if key(none, Key::CloseBracket) {
            self.set_slideshow_secs(self.settings.slideshow_secs + 1);
        }
        if key(none, Key::F5) {
            self.rescan();
        }
        // Cmd+Backspace, since Mac laptops have no forward-Delete key.
        if key(none, Key::Delete) || key(cmd, Key::Backspace) {
            self.confirm_delete = self.current_path();
        }
        let (back, forward) = ctx.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Extra1),
                i.pointer.button_pressed(PointerButton::Extra2),
            )
        });
        if !self.settings.side_buttons {
            // Back/Forward are turned off in Settings.
        } else if self.mode == Mode::Overview {
            if back {
                self.open_parent();
            }
        } else if back || forward {
            self.step(if forward { 1 } else { -1 });
            if self.slideshow.is_some() {
                self.slideshow = Some(Instant::now());
            }
        }
        if key(none, Key::Escape) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if fs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else if self.show_help {
                self.show_help = false;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.passport.open {
                self.passport.open = false;
            } else if self.convert.open {
                self.convert.open = false;
            } else if self.slideshow.is_some() {
                self.slideshow = None;
            } else if self.mode != Mode::Single && !self.entries.is_empty() {
                self.mode = Mode::Single;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        match self.mode {
            Mode::Single => {
                let mut moved = false;
                if key(none, Key::ArrowRight)
                    || key(none, Key::Space)
                    || key(none, Key::PageDown)
                    || key(none, Key::ArrowDown)
                {
                    self.step(1);
                    moved = true;
                }
                if key(none, Key::ArrowLeft)
                    || key(none, Key::Backspace)
                    || key(none, Key::PageUp)
                    || key(none, Key::ArrowUp)
                {
                    self.step(-1);
                    moved = true;
                }
                if key(none, Key::Home) {
                    self.go_to(0);
                    moved = true;
                }
                if key(none, Key::End) {
                    self.go_to(self.entries.len().saturating_sub(1));
                    moved = true;
                }
                if moved && self.slideshow.is_some() {
                    self.slideshow = Some(Instant::now());
                }
                if key(none, Key::Enter) {
                    self.toggle_mode();
                }
                if key(none, Key::Num0) {
                    self.zoom = Zoom::Fit;
                    self.pan = Vec2::ZERO;
                }
                if key(none, Key::Num1) {
                    self.zoom = Zoom::Scale(1.0);
                    self.pan = Vec2::ZERO;
                }
                let zoom_in =
                    key(none, Key::Plus) || key(none, Key::Equals) || key(shift, Key::Equals);
                let zoom_out = key(none, Key::Minus);
                if zoom_in || zoom_out {
                    let factor = if zoom_in { 1.25 } else { 0.8 };
                    self.pending_zoom = Some(factor);
                }
            }
            Mode::Grid => {
                let cols = self.grid_cols.max(1) as isize;
                let n = self.entries.len() as isize;
                let mut target = self.index as isize;
                if key(none, Key::ArrowRight) {
                    target += 1;
                }
                if key(none, Key::ArrowLeft) {
                    target -= 1;
                }
                if key(none, Key::ArrowDown) {
                    target += cols;
                }
                if key(none, Key::ArrowUp) {
                    target -= cols;
                }
                let page = cols
                    * ((self.grid_view_h / (self.settings.thumb_size + 30.0)).floor() as isize)
                        .max(1);
                if key(none, Key::PageDown) {
                    target += page;
                }
                if key(none, Key::PageUp) {
                    target -= page;
                }
                if key(none, Key::Home) {
                    target = 0;
                }
                if key(none, Key::End) {
                    target = n - 1;
                }
                if n > 0 && target != self.index as isize {
                    self.go_to(target.clamp(0, n - 1) as usize);
                }
                if key(none, Key::Enter) && n > 0 {
                    self.toggle_mode();
                }
                if key(none, Key::Plus) || key(none, Key::Equals) || key(shift, Key::Equals) {
                    self.settings.thumb_size =
                        (self.settings.thumb_size * 1.2).min(THUMB_PX as f32);
                }
                if key(none, Key::Minus) {
                    self.settings.thumb_size = (self.settings.thumb_size / 1.2).max(64.0);
                }
            }
            Mode::Overview => {
                if key(none, Key::Backspace) {
                    self.open_parent();
                }
                if key(none, Key::Enter) {
                    self.toggle_mode();
                }
                if key(none, Key::Plus) || key(none, Key::Equals) || key(shift, Key::Equals) {
                    self.settings.thumb_size =
                        (self.settings.thumb_size * 1.2).min(THUMB_PX as f32);
                }
                if key(none, Key::Minus) {
                    self.settings.thumb_size = (self.settings.thumb_size / 1.2).max(64.0);
                }
            }
        }
    }

    fn open_parent(&mut self) {
        if let Some(parent) = self.dir.as_deref().and_then(Path::parent) {
            self.open(parent.to_path_buf());
        }
    }

    /// Mouse wheel (and wheel tilt) moves between photos, one photo per notch.
    fn wheel_step(&mut self, notches: f32) {
        if notches == 0.0 {
            return;
        }
        let now = Instant::now();
        if now < self.wheel_quiet_until {
            self.wheel_acc = 0.0;
            return;
        }
        self.wheel_acc += notches;
        if self.wheel_acc.abs() >= 1.0 {
            let delta = if self.wheel_acc > 0.0 { 1 } else { -1 };
            self.wheel_acc = 0.0;
            self.wheel_quiet_until = now + WHEEL_PAUSE;
            self.step(delta);
            if self.slideshow.is_some() {
                self.slideshow = Some(now);
            }
        }
    }

    // ----- UI pieces ----------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Wraps onto a second line when the window is too narrow.
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("Open", |ui| {
                if ui.button("Open file…  (Ctrl+O)").clicked() {
                    ui.close();
                    self.open_dialog();
                }
                if ui.button("Open folder…  (Ctrl+Shift+O)").clicked() {
                    ui.close();
                    self.open_folder_dialog();
                }
            });
            let parent = self.dir.as_deref().and_then(Path::parent).is_some();
            if ui
                .add_enabled(parent, egui::Button::new("Up"))
                .on_hover_text("Parent folder")
                .clicked()
            {
                self.open_parent();
            }
            ui.separator();
            let has = !self.entries.is_empty();
            if ui
                .add_enabled(has, egui::Button::new("  ‹  "))
                .on_hover_text("Previous (Left)")
                .clicked()
            {
                self.step(-1);
            }
            if ui
                .add_enabled(has, egui::Button::new("  ›  "))
                .on_hover_text("Next (Right / Space)")
                .clicked()
            {
                self.step(1);
            }
            ui.separator();
            if ui
                .selectable_label(self.mode == Mode::Grid, "Browse")
                .on_hover_text("Thumbnail grid (G)")
                .clicked()
            {
                self.toggle_mode();
            }
            if self.has_subfolders
                && ui
                    .selectable_label(self.mode == Mode::Overview, "Folders")
                    .on_hover_text("First photos of every sub-folder")
                    .clicked()
            {
                self.mode = if self.mode == Mode::Overview {
                    Mode::Grid
                } else {
                    Mode::Overview
                };
            }
            if self.mode == Mode::Single {
                if ui
                    .button("Fit")
                    .on_hover_text("Fit to window (0)")
                    .clicked()
                {
                    self.zoom = Zoom::Fit;
                    self.pan = Vec2::ZERO;
                }
                if ui
                    .button("1:1")
                    .on_hover_text("Actual pixels (1)")
                    .clicked()
                {
                    self.zoom = Zoom::Scale(1.0);
                    self.pan = Vec2::ZERO;
                }
                if ui.button(" − ").on_hover_text("Zoom out (-)").clicked() {
                    self.pending_zoom = Some(0.8);
                }
                if ui.button(" + ").on_hover_text("Zoom in (+)").clicked() {
                    self.pending_zoom = Some(1.25);
                }
                if ui
                    .button("‹ Rotate")
                    .on_hover_text("Rotate left (Shift+R)")
                    .clicked()
                {
                    self.rotation = (self.rotation + 3) % 4;
                }
                if ui
                    .button("Rotate ›")
                    .on_hover_text("Rotate right (R)")
                    .clicked()
                {
                    self.rotation = (self.rotation + 1) % 4;
                }
            } else {
                ui.add(
                    egui::Slider::new(&mut self.settings.thumb_size, THUMB_SIZES).show_value(false),
                )
                .on_hover_text("Thumbnail size (+/-)");
            }
            ui.separator();
            let playing = self.slideshow.is_some();
            if ui
                .selectable_label(playing, "Slideshow")
                .on_hover_text(format!(
                    "S; {} s per photo ([ and ], or Settings)",
                    self.settings.slideshow_secs
                ))
                .clicked()
            {
                self.slideshow = if playing { None } else { Some(Instant::now()) };
            }
            if ui
                .selectable_label(self.show_info, "Info")
                .on_hover_text("I")
                .clicked()
            {
                self.show_info = !self.show_info;
            }
            if ui
                .button("Convert")
                .on_hover_text("Batch convert (Ctrl+S)")
                .clicked()
            {
                self.convert.open = true;
            }
            if ui
                .add_enabled(has, egui::Button::new("Passport"))
                .on_hover_text("Passport or visa photo from this photo (Ctrl+P)")
                .clicked()
            {
                self.passport.open_on(self.current_path());
            }
            if ui
                .add_enabled(has, egui::Button::new("Delete"))
                .on_hover_text("Move to Recycle Bin (Delete)")
                .clicked()
            {
                self.confirm_delete = self.current_path();
            }
            ui.separator();
            if ui
                .selectable_label(self.show_settings, "Settings")
                .on_hover_text("Slideshow, mouse, sorting and theme (Ctrl+,)")
                .clicked()
            {
                self.show_settings = !self.show_settings;
            }
            if ui
                .button("Full screen")
                .on_hover_text("Full screen (F / F11)")
                .clicked()
            {
                self.toggle_fullscreen(ctx);
            }
            if ui
                .button("?")
                .on_hover_text("Keyboard shortcuts (F1)")
                .clicked()
            {
                self.show_help = !self.show_help;
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.mode == Mode::Overview {
                if let Some(dir) = &self.dir {
                    ui.label(dir.display().to_string());
                    ui.separator();
                }
                ui.label(format!(
                    "{} sub-folders, {} photos here",
                    self.rows.len(),
                    self.entries.len()
                ));
                return;
            }
            let Some(entry) = self.current() else {
                ui.label("No photo open. Press Ctrl+O, or drop a photo or folder here.");
                return;
            };
            ui.label(format!("{} / {}", self.index + 1, self.entries.len()));
            ui.separator();
            ui.label(entry.file_name());
            if let Some(Slot::Ready(img)) =
                self.cache.get(&entry.path).and_then(|c| c.display.as_ref())
            {
                ui.separator();
                ui.label(format!("{} × {}", img.full_size[0], img.full_size[1]));
                ui.separator();
                ui.label(img.format.name());
                if img.frames.len() > 1 {
                    ui.label(format!("({} frames)", img.frames.len()));
                }
            }
            ui.separator();
            ui.label(human_bytes(entry.size));
            if self.mode == Mode::Single {
                ui.separator();
                ui.label(format!("{:.0}%", self.last_scale * 100.0));
            }
            if self.slideshow.is_some() {
                ui.separator();
                ui.label(format!("Slideshow {} s", self.settings.slideshow_secs));
            }
        });
    }

    fn info_panel(&self, ui: &mut egui::Ui) {
        ui.heading("Info");
        ui.separator();
        let Some(entry) = self.current() else {
            ui.label("No photo open.");
            return;
        };
        let img = match self.cache.get(&entry.path).and_then(|c| c.display.as_ref()) {
            Some(Slot::Ready(img)) => Some(img.clone()),
            _ => None,
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("info-grid")
                .num_columns(2)
                .spacing([10.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.strong(k);
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    };
                    row("File", entry.file_name());
                    if let Some(parent) = entry.path.parent() {
                        row("Folder", parent.display().to_string());
                    }
                    row("Size", human_bytes(entry.size));
                    if let Some(m) = entry.modified {
                        row("Modified", format_time(m));
                    }
                    if let Some(img) = &img {
                        row("Format", img.format.name().to_string());
                        row(
                            "Dimensions",
                            format!("{} × {} px", img.full_size[0], img.full_size[1]),
                        );
                        let mp = img.full_size[0] as f64 * img.full_size[1] as f64 / 1e6;
                        row("Megapixels", format!("{mp:.1}"));
                        row("Pixels", img.color.clone());
                        if img.frames.len() > 1 {
                            row(
                                "Animation",
                                format!(
                                    "{} frames, {:.1} s",
                                    img.frames.len(),
                                    img.total_duration().as_secs_f32()
                                ),
                            );
                        }
                        if img.has_icc {
                            row("Colour profile", "Embedded ICC".into());
                        }
                        row("Decoded in", format!("{} ms", img.decode_time.as_millis()));
                        for (k, v) in &img.exif {
                            row(k, v.clone());
                        }
                    }
                });
        });
    }

    fn single_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        if response.double_clicked() {
            self.toggle_fullscreen(ctx);
        }
        if response.secondary_clicked() && self.settings.right_click_fit {
            self.zoom = Zoom::Fit;
            self.pan = Vec2::ZERO;
        }
        let painter = ui.painter_at(rect);

        let Some(path) = self.current_path() else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Open a photo with Ctrl+O, or drop a photo or folder here",
                egui::FontId::proportional(18.0),
                Color32::GRAY,
            );
            return;
        };
        let cached = self.cache.get(&path);
        let display = match cached.and_then(|c| c.display.as_ref()) {
            Some(Slot::Ready(img)) => img.clone(),
            Some(Slot::Failed(err)) => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("Cannot show this file\n{err}"),
                    egui::FontId::proportional(16.0),
                    Color32::LIGHT_RED,
                );
                return;
            }
            _ => {
                let spinner_rect = Rect::from_center_size(rect.center(), Vec2::splat(36.0));
                ui.put(spinner_rect, egui::Spinner::new().size(36.0));
                return;
            }
        };
        let full = match cached.and_then(|c| c.full.as_ref()) {
            Some(Slot::Ready(img)) => Some(img.clone()),
            _ => None,
        };

        let ppp = ctx.pixels_per_point();
        let [w, h] = display.full_size;
        let (fw, fh) = if self.rotation % 2 == 1 {
            (h as f32, w as f32)
        } else {
            (w as f32, h as f32)
        };
        let fit = ((rect.width() * ppp) / fw)
            .min((rect.height() * ppp) / fh)
            .min(1.0);

        // Zoom from wheel, pinch, keys and buttons, anchored at the pointer.
        // What the plain wheel does is a setting (by default: change photo at
        // fit size, zoom once zoomed in). Ctrl+wheel and pinch always zoom
        // (egui reports those as zoom_delta); wheel tilt changes photo.
        let (scroll, pinch, pointer, (vertical, tilt)) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.pointer.hover_pos(),
                wheel_notches(i),
            )
        });
        let mut factor = self.pending_zoom.take().unwrap_or(1.0);
        if response.hovered() {
            let wheel_navigates = match self.settings.wheel {
                WheelMode::Auto => self.zoom == Zoom::Fit,
                WheelMode::Navigate => true,
                WheelMode::Zoom => false,
            };
            let tilt = if self.settings.side_buttons {
                tilt
            } else {
                0.0
            };
            if wheel_navigates {
                self.wheel_step(vertical + tilt);
            } else {
                self.wheel_step(tilt);
                if scroll != 0.0 {
                    factor *= (scroll * 0.0025).exp();
                }
            }
            factor *= pinch;
        }
        if factor != 1.0 {
            let anchor = pointer
                .filter(|p| rect.contains(*p))
                .unwrap_or(rect.center());
            let old = match self.zoom {
                Zoom::Fit => fit,
                Zoom::Scale(s) => s,
            };
            let new = (old * factor).clamp(0.02, 64.0);
            let center = rect.center() + self.pan;
            let rel = (anchor - center) / old;
            self.pan = (anchor - rel * new) - rect.center();
            // Zooming back out past fit returns to fit, so the wheel moves
            // between photos again.
            self.zoom = if factor < 1.0 && old > fit && new <= fit {
                Zoom::Fit
            } else {
                Zoom::Scale(new)
            };
        }
        let scale = match self.zoom {
            Zoom::Fit => {
                self.pan = Vec2::ZERO;
                fit
            }
            Zoom::Scale(s) => s,
        };
        self.last_scale = scale;

        let size = Vec2::new(fw * scale / ppp, fh * scale / ppp);
        if response.dragged() {
            self.pan += response.drag_delta();
        }
        // Keep the photo on screen: centred when smaller than the view.
        let slack = ((size - rect.size()) / 2.0).max(Vec2::ZERO);
        self.pan = self.pan.clamp(-slack, slack);
        if response.dragged() || slack != Vec2::ZERO {
            ctx.set_cursor_icon(if response.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }

        // Need more detail than the screen-sized texture has? Load full size.
        if display.is_downscaled() {
            let tex_ratio = display.tex_size[0] as f32 / display.full_size[0] as f32;
            if scale > tex_ratio * 1.05 {
                self.request_full(&path);
            }
        }
        let img = full.unwrap_or(display);

        // Animation frame.
        let mut frame = 0;
        if img.frames.len() > 1 {
            let total = img.total_duration();
            if !total.is_zero() {
                let mut t = Duration::from_nanos(
                    (self.shown_at.elapsed().as_nanos() % total.as_nanos()) as u64,
                );
                for (k, (_, d)) in img.frames.iter().enumerate() {
                    if t < *d {
                        frame = k;
                        ctx.request_repaint_after(*d - t);
                        break;
                    }
                    t -= *d;
                }
            }
        }
        let texture = &img.frames[frame].0;

        let img_rect = Rect::from_center_size(rect.center() + self.pan, size);
        let mut mesh = egui::Mesh::with_texture(texture.id());
        let uvs = [
            Pos2::new(0.0, 0.0),
            Pos2::new(1.0, 0.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(0.0, 1.0),
        ];
        let corners = [
            img_rect.left_top(),
            img_rect.right_top(),
            img_rect.right_bottom(),
            img_rect.left_bottom(),
        ];
        let r = self.rotation as usize;
        for (k, pos) in corners.into_iter().enumerate() {
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: uvs[(k + 4 - r) % 4],
                color: Color32::WHITE,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }

    fn grid_view(&mut self, ui: &mut egui::Ui) {
        let n = self.entries.len();
        if n == 0 {
            let msg = if self.scan_rx.is_some() {
                "Scanning folder…"
            } else {
                "No photos in this folder"
            };
            ui.centered_and_justified(|ui| ui.label(msg));
            return;
        }
        let cell_w = self.settings.thumb_size + 12.0;
        let row_h = self.settings.thumb_size + 30.0;
        let cols = ((ui.available_width() / cell_w).floor() as usize).max(1);
        self.grid_cols = cols;
        let rows = n.div_ceil(cols);
        let spacing = ui.spacing().item_spacing.y;

        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if self.grid_follow {
            self.grid_follow = false;
            let row = (self.index / cols) as f32;
            let top = row * (row_h + spacing);
            let bottom = top + row_h;
            if top < self.grid_offset {
                area = area.vertical_scroll_offset(top);
            } else if bottom > self.grid_offset + self.grid_view_h {
                area = area.vertical_scroll_offset(bottom - self.grid_view_h);
            }
        }

        let mut clicked = None;
        let mut opened = None;
        let mut visible = 0..0;
        let entries = &self.entries;
        let thumbs = &self.thumbs;
        let thumb_size = self.settings.thumb_size;
        let index = self.index;
        let output = area.show_rows(ui, row_h, rows, |ui, range| {
            visible = range.start * cols..(range.end * cols).min(n);
            for r in range {
                ui.horizontal(|ui| {
                    for c in 0..cols {
                        let i = r * cols + c;
                        if i >= n {
                            break;
                        }
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(cell_w - 4.0, row_h), Sense::click());
                        let painter = ui.painter_at(rect);
                        if i == index {
                            painter.rect_filled(rect, 6.0, ui.visuals().selection.bg_fill);
                        } else if resp.hovered() {
                            painter.rect_filled(
                                rect,
                                6.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        }
                        paint_thumb(&painter, rect, thumb_size, thumbs.get(&entries[i].path));
                        paint_caption(
                            &painter,
                            rect,
                            entries[i].file_name(),
                            ui.visuals().text_color(),
                        );
                        if resp.double_clicked() {
                            opened = Some(i);
                        } else if resp.clicked() {
                            clicked = Some(i);
                        }
                        resp.on_hover_text(entries[i].file_name());
                    }
                });
            }
        });
        self.grid_offset = output.state.clamped_offset().y;
        self.grid_view_h = output.inner_rect.height();

        if let Some(i) = clicked {
            self.go_to(i);
        }
        if let Some(i) = opened {
            self.go_to(i);
            self.toggle_mode();
        }
        self.request_thumbs(visible, cols);
    }

    fn overview_view(&mut self, ui: &mut egui::Ui) {
        // The folder's own photos come first, as a row like the sub-folders.
        let here = usize::from(!self.entries.is_empty());
        let n = self.rows.len() + here;
        if n == 0 {
            let msg = if self.scan_rx.is_some() || self.rows_rx.is_some() {
                "Scanning folder…"
            } else {
                "No photos or sub-folders here"
            };
            ui.centered_and_justified(|ui| ui.label(msg));
            return;
        }
        let thumb_size = self.settings.thumb_size;
        let cell_w = thumb_size + 12.0;
        let tile_h = thumb_size + 30.0;
        let header_h = 28.0;
        let row_h = header_h + tile_h + 8.0;
        // As many photos as fit, leaving the last tile for "open folder".
        let per_row = ((ui.available_width() / cell_w).floor() as usize)
            .max(2)
            .saturating_sub(1);

        let row = |r: usize| -> (Option<&Path>, &str, &[Entry], usize, usize) {
            if r < here {
                let shown = self.entries.len().min(per_row);
                (
                    None,
                    "Photos in this folder",
                    &self.entries[..shown],
                    self.entries.len(),
                    0,
                )
            } else {
                let row = &self.rows[r - here];
                (
                    Some(row.dir.as_path()),
                    &row.name,
                    &row.photos,
                    row.total,
                    row.subfolders,
                )
            }
        };

        let mut action = None;
        let mut visible = 0..0;
        let thumbs = &self.thumbs;
        let current = self.current_path();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, row_h, n, |ui, range| {
                visible = range.clone();
                for r in range {
                    let (dir, name, photos, total, subfolders) = row(r);
                    let (header, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), header_h),
                        Sense::hover(),
                    );
                    let folders = match subfolders {
                        1 => "1 folder".to_string(),
                        n => format!("{n} folders"),
                    };
                    let detail = match total {
                        0 if subfolders > 0 => format!("{folders}, showing photos inside them"),
                        0 => "no photos".to_string(),
                        t => {
                            let photos = if t == 1 {
                                "1 photo".to_string()
                            } else {
                                format!("{t} photos")
                            };
                            if subfolders > 0 {
                                format!("{photos}, {folders}")
                            } else {
                                photos
                            }
                        }
                    };
                    let painter = ui.painter_at(header);
                    let title = painter.text(
                        header.left_bottom() + Vec2::new(6.0, -6.0),
                        egui::Align2::LEFT_BOTTOM,
                        name,
                        egui::FontId::proportional(16.0),
                        ui.visuals().strong_text_color(),
                    );
                    painter.text(
                        title.right_bottom() + Vec2::new(10.0, 0.0),
                        egui::Align2::LEFT_BOTTOM,
                        detail,
                        egui::FontId::proportional(13.0),
                        Color32::GRAY,
                    );
                    ui.horizontal(|ui| {
                        for e in photos.iter().take(per_row) {
                            let (rect, resp) = ui.allocate_exact_size(
                                Vec2::new(cell_w - 4.0, tile_h),
                                Sense::click(),
                            );
                            let painter = ui.painter_at(rect);
                            if dir.is_none() && current.as_ref() == Some(&e.path) {
                                painter.rect_filled(rect, 6.0, ui.visuals().selection.bg_fill);
                            } else if resp.hovered() {
                                painter.rect_filled(
                                    rect,
                                    6.0,
                                    ui.visuals().widgets.hovered.weak_bg_fill,
                                );
                            }
                            paint_thumb(&painter, rect, thumb_size, thumbs.get(&e.path));
                            paint_caption(&painter, rect, e.file_name(), ui.visuals().text_color());
                            if resp.on_hover_text(e.file_name()).clicked() {
                                action = Some(OverviewAction::OpenPhoto(e.path.clone()));
                            }
                        }
                        // The last tile opens the folder, with its first photo as the
                        // cover.
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(cell_w - 4.0, tile_h), Sense::click());
                        let painter = ui.painter_at(rect);
                        if resp.hovered() {
                            painter.rect_filled(
                                rect,
                                6.0,
                                ui.visuals().widgets.hovered.weak_bg_fill,
                            );
                        }
                        let cover = photos.first().and_then(|e| thumbs.get(&e.path));
                        let picture = paint_thumb(&painter, rect, thumb_size, cover);
                        let hidden = total.saturating_sub(photos.len().min(per_row));
                        paint_folder_badge(&painter, picture, hidden);
                        paint_caption(&painter, rect, name.to_string(), ui.visuals().text_color());
                        if resp.on_hover_text(format!("Browse {name}")).clicked() {
                            action = Some(OverviewAction::Browse(dir.map(Path::to_path_buf)));
                        }
                    });
                    ui.add_space(8.0);
                }
            });

        // Thumbnails for the rows on screen and one row either side.
        let first = visible.start.saturating_sub(1);
        let last = (visible.end + 1).min(n);
        let wanted: Vec<PathBuf> = (first..last)
            .flat_map(|r| row(r).2.iter().take(per_row).map(|e| e.path.clone()))
            .collect();
        self.request_thumb_paths(wanted);

        match action {
            Some(OverviewAction::OpenPhoto(path)) => {
                if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                    self.go_to(i);
                    self.mode = Mode::Single;
                    self.schedule();
                } else {
                    self.open(path);
                }
            }
            Some(OverviewAction::Browse(Some(dir))) => self.open(dir),
            Some(OverviewAction::Browse(None)) => {
                self.mode = Mode::Grid;
                self.grid_follow = true;
            }
            None => {}
        }
    }

    /// Load exactly these thumbnails (in order), cancelling other pending
    /// ones, and keep the decoded set bounded.
    fn request_thumb_paths(&mut self, wanted: Vec<PathBuf>) {
        let set: HashSet<PathBuf> = wanted.iter().cloned().collect();
        self.thumb_loader.retain(|p, _| set.contains(p));
        for path in wanted {
            if matches!(self.thumbs.get(&path), None | Some(Slot::Pending)) {
                self.thumbs.insert(path.clone(), Slot::Pending);
                self.thumb_loader.request(
                    Request {
                        path,
                        kind: Kind::Thumb,
                        max_side: THUMB_PX,
                    },
                    false,
                );
            }
        }
        if self.thumbs.len() > 400 {
            self.thumbs.retain(|p, _| set.contains(p));
        }
        self.thumbs
            .retain(|p, s| !matches!(s, Slot::Pending) || set.contains(p));
    }

    /// Load thumbnails for what is on screen (plus a row either side), cancel
    /// the rest, and free textures far away from the view.
    fn request_thumbs(&mut self, visible: std::ops::Range<usize>, cols: usize) {
        let n = self.entries.len();
        let start = visible.start.saturating_sub(cols);
        let end = (visible.end + cols).min(n);
        let wanted: HashSet<PathBuf> = self.entries[start..end]
            .iter()
            .map(|e| e.path.clone())
            .collect();
        self.thumb_loader.retain(|p, _| wanted.contains(p));
        for k in start..end {
            let path = &self.entries[k].path;
            let state = self.thumbs.get(path);
            if matches!(state, None | Some(Slot::Pending)) {
                self.thumbs.insert(path.clone(), Slot::Pending);
                self.thumb_loader.request(
                    Request {
                        path: path.clone(),
                        kind: Kind::Thumb,
                        max_side: THUMB_PX,
                    },
                    false,
                );
            }
        }
        // Keep up to ~400 decoded thumbnails around the view.
        let keep_start = start.saturating_sub(200);
        let keep_end = (end + 200).min(n);
        if self.thumbs.len() > 400 {
            let keep: HashSet<&PathBuf> = self.entries[keep_start..keep_end]
                .iter()
                .map(|e| &e.path)
                .collect();
            self.thumbs.retain(|p, s| {
                keep.contains(p) && !matches!(s, Slot::Pending) || wanted.contains(p)
            });
        }
        self.thumbs
            .retain(|p, s| !matches!(s, Slot::Pending) || wanted.contains(p));
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let current = self.current_path();
        let folder: Vec<PathBuf> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.convert.show(ctx, current.as_ref(), &folder);
        if self.convert.finished {
            self.convert.finished = false;
            self.rescan();
        }
        self.passport.show(ctx);
        if self.passport.saved {
            self.passport.saved = false;
            self.rescan();
        }

        if let Some(path) = self.confirm_delete.clone() {
            let mut confirm = false;
            let mut cancel = false;
            let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
                ui.set_width(320.0);
                ui.heading("Move to Recycle Bin?");
                ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    confirm = true;
                }
            });
            if confirm {
                self.delete_current();
            } else if cancel || modal.should_close() {
                self.confirm_delete = None;
            }
        }

        self.settings_window(ctx);

        if self.show_help {
            egui::Window::new("Keyboard shortcuts")
                .open(&mut self.show_help)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    egui::Grid::new("help-grid")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            for (k, v) in HELP {
                                ui.strong(*k);
                                ui.label(*v);
                                ui.end_row();
                            }
                        });
                });
        }

        if let Some((msg, at)) = &self.toast {
            if at.elapsed() > Duration::from_millis(2500) {
                self.toast = None;
            } else {
                let msg = msg.clone();
                egui::Area::new(egui::Id::unique("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -48.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(msg));
                    });
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut resort = false;
        let s = &mut self.settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Slideshow");
                ui.add(
                    egui::Slider::new(&mut s.slideshow_secs, SLIDESHOW_SECS)
                        .text("seconds per photo"),
                );
                ui.add_space(8.0);

                ui.heading("Mouse");
                ui.label("Mouse wheel in the photo view:");
                for mode in WheelMode::ALL {
                    ui.radio_value(&mut s.wheel, mode, mode.label());
                }
                ui.checkbox(
                    &mut s.right_click_fit,
                    "Right-click returns to fit-to-window",
                );
                ui.checkbox(
                    &mut s.side_buttons,
                    "Wheel tilt and Back/Forward buttons change photo",
                );
                ui.label(
                    egui::RichText::new("Ctrl+wheel always zooms.")
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);

                ui.heading("Sorting");
                for key in SortKey::ALL {
                    resort |= ui.radio_value(&mut s.sort, key, key.label()).changed();
                }
                resort |= ui.checkbox(&mut s.sort_desc, "Descending").changed();
                ui.add_space(8.0);

                ui.heading("Folders");
                ui.checkbox(
                    &mut s.folder_overview,
                    "Show the folder overview for folders with sub-folders",
                );
                ui.add(egui::Slider::new(&mut s.thumb_size, THUMB_SIZES).text("thumbnail size"));
                ui.add_space(8.0);

                ui.heading("Theme");
                ui.horizontal(|ui| {
                    for theme in Theme::ALL {
                        ui.radio_value(&mut s.theme, theme, theme.label());
                    }
                });
                ui.add_space(8.0);

                if ui.button("Restore defaults").clicked() {
                    let defaults = Settings::default();
                    resort |= defaults.sort != s.sort || defaults.sort_desc != s.sort_desc;
                    *s = defaults;
                }
                if let Some(path) = Settings::path() {
                    ui.label(
                        egui::RichText::new(format!("Saved in {}", path.display()))
                            .small()
                            .weak(),
                    );
                }
            });
        self.show_settings = open;
        if resort {
            self.resort();
        }
    }

    /// Apply the theme and save settings once they change (not while a
    /// slider is being dragged).
    fn apply_settings(&mut self, ctx: &egui::Context) {
        // On Linux the desktop portal knows the light/dark setting; elsewhere
        // egui gets it from the window system.
        let theme = match (self.settings.theme, self.desktop_theme.get()) {
            (Theme::System, Some(egui::Theme::Dark)) => egui::ThemePreference::Dark,
            (Theme::System, Some(egui::Theme::Light)) => egui::ThemePreference::Light,
            (theme, _) => theme.preference(),
        };
        if ctx.options(|o| o.theme_preference) != theme {
            ctx.set_theme(theme);
        }
        if self.settings != self.saved_settings && !ctx.input(|i| i.pointer.any_down()) {
            self.settings.save();
            self.saved_settings = self.settings.clone();
        }
    }

    fn set_slideshow_secs(&mut self, secs: u32) {
        self.settings.slideshow_secs = secs.clamp(*SLIDESHOW_SECS.start(), *SLIDESHOW_SECS.end());
        self.toast(format!(
            "Slideshow: {} s per photo",
            self.settings.slideshow_secs
        ));
    }

    fn tick_slideshow(&mut self, ctx: &egui::Context) {
        let Some(started) = self.slideshow else {
            return;
        };
        let interval = Duration::from_secs(u64::from(self.settings.slideshow_secs));
        let elapsed = started.elapsed();
        if elapsed >= interval {
            self.step(1);
            self.slideshow = Some(Instant::now());
            ctx.request_repaint_after(interval);
        } else {
            ctx.request_repaint_after(interval - elapsed);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.current() {
            Some(e) => format!(
                "{} ({}/{}) - Chitra",
                e.file_name(),
                self.index + 1,
                self.entries.len()
            ),
            None => "Chitra".to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

const HELP: &[(&str, &str)] = &[
    ("Right, Space, PgDn", "Next photo"),
    ("Left, Backspace, PgUp", "Previous photo"),
    ("Home / End", "First / last photo"),
    (
        "Mouse wheel, tilt, Back/Forward buttons",
        "Previous / next photo (zoomed in: wheel zooms)",
    ),
    ("Right-click", "Back to fit-to-window"),
    ("G or Enter", "Switch between photo and thumbnail grid"),
    ("Backspace (folder overview)", "Parent folder"),
    ("Ctrl+wheel, + / -", "Zoom (grid: thumbnail size)"),
    ("0 / 1", "Fit to window / actual pixels"),
    ("Drag", "Pan when zoomed in"),
    ("R / Shift+R", "Rotate view right / left"),
    ("F, F11, double-click", "Full screen"),
    ("I", "Info panel with EXIF"),
    ("S", "Slideshow"),
    ("[ / ]", "Slideshow: fewer / more seconds per photo"),
    ("Delete, Cmd+Backspace", "Move to Recycle Bin"),
    ("Ctrl+C / Ctrl+Shift+C", "Copy image / copy path"),
    ("Ctrl+O / Ctrl+Shift+O", "Open file / folder"),
    ("Ctrl+S", "Convert (batch)"),
    ("Ctrl+P", "Passport / visa photo"),
    ("Ctrl+,", "Settings (slideshow, mouse, sorting, theme)"),
    ("F5", "Refresh folder"),
    ("Esc", "Leave full screen, close, quit"),
];

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_scan();
        self.poll_rows();
        self.poll_loader();
        self.handle_input(&ctx);
        self.update_title(&ctx);
        self.tick_slideshow(&ctx);

        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !fullscreen {
            egui::Panel::top("toolbar").show(ui, |ui| {
                if self.headerbar {
                    headerbar::drag_area(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        headerbar::window_buttons(ui);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                            self.toolbar(ui, &ctx)
                        });
                    });
                } else {
                    self.toolbar(ui, &ctx);
                }
            });
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        }
        if self.show_info {
            egui::Panel::right("info")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| self.info_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(background(ctx.theme())))
            .show(ui, |ui| match self.mode {
                Mode::Single => self.single_view(ui, &ctx),
                Mode::Grid => self.grid_view(ui),
                Mode::Overview => self.overview_view(ui),
            });
        self.dialogs(&ctx);
        self.apply_settings(&ctx);
        if self.headerbar {
            headerbar::resize_edges(&ctx);
        }
    }
}

/// "Open folder" across the bottom of a folder tile's cover photo, with how
/// many photos the row could not show.
fn paint_folder_badge(painter: &egui::Painter, picture: Rect, hidden: usize) {
    let lines: f32 = if hidden > 0 { 2.0 } else { 1.0 };
    let band = Rect::from_min_max(
        Pos2::new(
            picture.min.x,
            picture.max.y - (8.0 + 18.0 * lines).min(picture.height()),
        ),
        picture.max,
    );
    painter.rect_filled(
        band,
        egui::CornerRadius {
            nw: 0,
            ne: 0,
            sw: 4,
            se: 4,
        },
        Color32::from_black_alpha(170),
    );
    painter.text(
        Pos2::new(band.center().x, band.min.y + 13.0),
        egui::Align2::CENTER_CENTER,
        "Open folder  ›",
        egui::FontId::proportional(14.0),
        Color32::WHITE,
    );
    if hidden > 0 {
        painter.text(
            Pos2::new(band.center().x, band.min.y + 31.0),
            egui::Align2::CENTER_CENTER,
            format!("+{hidden} more"),
            egui::FontId::proportional(12.0),
            Color32::from_gray(210),
        );
    }
}

/// Draw a thumbnail centred in the top square of a grid cell; returns where
/// the picture (or its placeholder) went.
fn paint_thumb(painter: &egui::Painter, cell: Rect, size: f32, slot: Option<&Slot>) -> Rect {
    let box_rect = Rect::from_min_size(
        cell.min + Vec2::new((cell.width() - size) / 2.0, 4.0),
        Vec2::splat(size),
    );
    match slot {
        Some(Slot::Ready(img)) => {
            let [tw, th] = img.tex_size;
            let s = (size / tw as f32).min(size / th as f32);
            let r =
                Rect::from_center_size(box_rect.center(), Vec2::new(tw as f32 * s, th as f32 * s));
            painter.image(
                img.frames[0].0.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            r
        }
        Some(Slot::Failed(_)) => {
            painter.text(
                box_rect.center(),
                egui::Align2::CENTER_CENTER,
                "!",
                egui::FontId::proportional(28.0),
                Color32::LIGHT_RED,
            );
            box_rect.shrink(size * 0.1)
        }
        _ => {
            let placeholder = if painter.ctx().theme() == egui::Theme::Dark {
                Color32::from_gray(40)
            } else {
                Color32::from_gray(215)
            };
            let r = box_rect.shrink(size * 0.1);
            painter.rect_filled(r, 4.0, placeholder);
            r
        }
    }
}

/// One line of text under a grid cell, clipped to the cell width.
fn paint_caption(painter: &egui::Painter, cell: Rect, text: String, color: Color32) {
    let text_pos = Pos2::new(cell.center().x, cell.bottom() - 13.0);
    let galley = painter.layout(
        text,
        egui::FontId::proportional(12.0),
        color,
        cell.width() - 8.0,
    );
    let line = galley.rows.first().map_or(galley.size().y, |r| r.height());
    painter
        .with_clip_rect(Rect::from_center_size(
            text_pos,
            Vec2::new(cell.width() - 6.0, line),
        ))
        .galley(
            text_pos - Vec2::new(galley.size().x / 2.0, line / 2.0),
            galley,
            Color32::WHITE,
        );
}

/// Mouse wheel movement this frame in notches as (vertical, tilt), positive =
/// next photo (wheel down, tilt right). Ctrl/Cmd+wheel is left for zooming.
fn wheel_notches(input: &egui::InputState) -> (f32, f32) {
    let mut out = (0.0, 0.0);
    for e in &input.events {
        if let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = e
        {
            if modifiers.ctrl || modifiers.command {
                continue;
            }
            let per_notch = match unit {
                egui::MouseWheelUnit::Line => 1.0,
                egui::MouseWheelUnit::Page => 1.0 / 3.0,
                egui::MouseWheelUnit::Point => 50.0,
            };
            out.0 -= delta.y / per_notch;
            out.1 -= delta.x / per_notch;
        }
    }
    out
}

/// Behind photos and thumbnails: near-black in the dark theme, light grey in
/// the light one.
fn background(theme: egui::Theme) -> Color32 {
    if theme == egui::Theme::Dark {
        Color32::from_rgb(24, 24, 24)
    } else {
        Color32::from_gray(236)
    }
}

/// `canonicalize` on Windows returns `\\?\C:\...`; strip that prefix for display
/// and for paths handed to other programs.
fn simplify_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC\\") {
            return PathBuf::from(rest.to_string());
        }
    }
    p
}

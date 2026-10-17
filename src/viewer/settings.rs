//! Viewer preferences, kept in a small `key = value` text file in the user's
//! config folder (`%APPDATA%\Chitra\settings.ini` on Windows).

use std::path::PathBuf;

use chitra::folder::SortKey;
use eframe::egui;

pub const SLIDESHOW_SECS: std::ops::RangeInclusive<u32> = 1..=60;
pub const THUMB_SIZES: std::ops::RangeInclusive<f32> = 64.0..=320.0;

/// What the plain mouse wheel does in the photo view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelMode {
    /// Next/previous photo at fit size; zoom once zoomed in.
    Auto,
    /// Always next/previous photo (Ctrl+wheel zooms).
    Navigate,
    /// Always zoom.
    Zoom,
}

impl WheelMode {
    pub const ALL: [WheelMode; 3] = [WheelMode::Auto, WheelMode::Navigate, WheelMode::Zoom];

    pub fn label(self) -> &'static str {
        match self {
            WheelMode::Auto => "Next / previous photo, zoom once zoomed in",
            WheelMode::Navigate => "Always next / previous photo (Ctrl+wheel zooms)",
            WheelMode::Zoom => "Always zoom",
        }
    }

    fn key(self) -> &'static str {
        match self {
            WheelMode::Auto => "auto",
            WheelMode::Navigate => "navigate",
            WheelMode::Zoom => "zoom",
        }
    }
}

/// Light or dark look; System follows the operating system's setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System if cfg!(windows) => "Same as Windows",
            Theme::System => "Same as system",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub slideshow_secs: u32,
    pub wheel: WheelMode,
    /// Right-click in the photo view returns to fit-to-window.
    pub right_click_fit: bool,
    /// Wheel tilt and the mouse Back/Forward buttons change photo.
    pub side_buttons: bool,
    pub sort: SortKey,
    pub sort_desc: bool,
    /// Open folders that have sub-folders in the folder overview.
    pub folder_overview: bool,
    pub thumb_size: f32,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            slideshow_secs: 3,
            wheel: WheelMode::Auto,
            right_click_fit: true,
            side_buttons: true,
            sort: SortKey::Name,
            sort_desc: false,
            folder_overview: true,
            thumb_size: 160.0,
            theme: Theme::System,
        }
    }
}

fn sort_key_name(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Modified => "modified",
        SortKey::Size => "size",
        SortKey::Extension => "type",
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            PathBuf::from(std::env::var_os("APPDATA")?)
        } else if cfg!(target_os = "macos") {
            PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
        } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(xdg)
        } else {
            PathBuf::from(std::env::var_os("HOME")?).join(".config")
        };
        Some(base.join("Chitra").join("settings.ini"))
    }

    /// Saved settings, or the defaults when there are none.
    pub fn load() -> Settings {
        Settings::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| Settings::parse(&text))
            .unwrap_or_default()
    }

    /// Write the settings, replacing the file atomically. Errors are
    /// ignored: losing a preference is not worth interrupting the user.
    pub fn save(&self) {
        let Some(path) = Settings::path() else { return };
        let Some(dir) = path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = path.with_extension("ini.tmp");
        if std::fs::write(&tmp, self.to_text()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Unknown keys and bad values fall back to the defaults.
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let flag = matches!(value, "true" | "yes" | "1");
            match key.trim() {
                "slideshow_seconds" => {
                    if let Ok(n) = value.parse::<u32>() {
                        s.slideshow_secs = n.clamp(*SLIDESHOW_SECS.start(), *SLIDESHOW_SECS.end());
                    }
                }
                "mouse_wheel" => {
                    if let Some(m) = WheelMode::ALL.into_iter().find(|m| m.key() == value) {
                        s.wheel = m;
                    }
                }
                "right_click_fit" => s.right_click_fit = flag,
                "tilt_and_side_buttons" => s.side_buttons = flag,
                "sort" => {
                    if let Some(k) = SortKey::ALL
                        .into_iter()
                        .find(|k| sort_key_name(*k) == value)
                    {
                        s.sort = k;
                    }
                }
                "sort_descending" => s.sort_desc = flag,
                "folder_overview" => s.folder_overview = flag,
                "theme" => {
                    if let Some(th) = Theme::ALL.into_iter().find(|th| th.key() == value) {
                        s.theme = th;
                    }
                }
                "thumbnail_size" => {
                    if let Ok(n) = value.parse::<f32>() {
                        s.thumb_size = n.clamp(*THUMB_SIZES.start(), *THUMB_SIZES.end());
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Chitra settings\n\
             slideshow_seconds = {}\n\
             mouse_wheel = {}\n\
             right_click_fit = {}\n\
             tilt_and_side_buttons = {}\n\
             sort = {}\n\
             sort_descending = {}\n\
             folder_overview = {}\n\
             thumbnail_size = {}\n\
             theme = {}\n",
            self.slideshow_secs,
            self.wheel.key(),
            self.right_click_fit,
            self.side_buttons,
            sort_key_name(self.sort),
            self.sort_desc,
            self.folder_overview,
            self.thumb_size.round(),
            self.theme.key(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            slideshow_secs: 7,
            wheel: WheelMode::Navigate,
            right_click_fit: false,
            side_buttons: false,
            sort: SortKey::Modified,
            sort_desc: true,
            folder_overview: false,
            thumb_size: 200.0,
            theme: Theme::Light,
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn bad_values_fall_back() {
        let s =
            Settings::parse("slideshow_seconds = 999\nmouse_wheel = spin\nnonsense\nsort = size");
        assert_eq!(s.slideshow_secs, 60);
        assert_eq!(s.wheel, WheelMode::Auto);
        assert_eq!(s.sort, SortKey::Size);
    }
}

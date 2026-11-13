//! Video playback with the operating system's own player: Media Foundation
//! on Windows, AVFoundation on macOS and GStreamer on Linux. They decode
//! (with the GPU where they can), play the sound and keep it in step with the
//! picture; we copy each new picture into an egui texture.
//!
//! This is the only place besides `platform.rs` with `unsafe` code: the
//! Windows and macOS players are reached through their C/Objective-C APIs.
//! Every file is opened by the system player, which is what the system's own
//! video app uses, so no codec of ours parses untrusted video data.

// Builds without the `video` feature keep the shared plumbing unused.
#![cfg_attr(not(feature = "video"), allow(dead_code, unused_imports))]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{Color32, ColorImage};

#[cfg(all(feature = "video", target_os = "macos"))]
mod avf;
#[cfg(all(feature = "video", unix, not(target_os = "macos")))]
mod gst;
#[cfg(all(feature = "video", windows))]
mod mf;

#[cfg(all(feature = "video", target_os = "macos"))]
use avf as backend;
#[cfg(all(feature = "video", unix, not(target_os = "macos")))]
use gst as backend;
#[cfg(all(feature = "video", windows))]
use mf as backend;
#[cfg(not(feature = "video"))]
use unsupported as backend;

/// Wakes the UI when a new picture or event arrives.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

pub struct Options {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub repaint: Repaint,
    /// Pictures are scaled down to fit this many pixels on their longest
    /// side where the player can do it cheaply (the screen size).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub max_side: u32,
    /// Play the sound (tests turn it off: build machines have no speakers).
    pub audio: bool,
    pub volume: f32,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub position: Duration,
    pub duration: Option<Duration>,
    pub playing: bool,
    pub ended: bool,
    pub error: Option<String>,
}

/// The newest decoded picture, handed from the player's thread to the UI.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Clone, Default)]
pub(crate) struct FrameSlot(Arc<Mutex<Option<ColorImage>>>);

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl FrameSlot {
    pub(crate) fn put(&self, image: ColorImage) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(image);
        }
    }

    pub(crate) fn take(&self) -> Option<ColorImage> {
        self.0.lock().ok().and_then(|mut s| s.take())
    }
}

/// One video being played. Dropping it stops playback and frees the file.
pub struct Player {
    inner: backend::Player,
}

impl Player {
    pub fn open(path: &Path, options: Options) -> Result<Player, String> {
        // The system players want absolute paths (GStreamer makes a URI).
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Player {
            inner: backend::Player::open(&path, options)?,
        })
    }

    /// Where playback is; call once per UI frame.
    pub fn status(&mut self) -> Status {
        self.inner.status()
    }

    /// The newest picture, if it changed since the last call.
    pub fn take_frame(&mut self) -> Option<ColorImage> {
        self.inner.take_frame()
    }

    /// How soon to look for the next picture when the player cannot wake the
    /// UI itself (AVFoundation); `None` when it does.
    pub fn poll_interval(&self) -> Option<Duration> {
        self.inner.poll_interval()
    }

    pub fn play(&mut self) {
        self.inner.play()
    }

    pub fn pause(&mut self) {
        self.inner.pause()
    }

    /// Jump to `to`. `exact` lands on that frame (for clicks and keys);
    /// otherwise the nearest key frame, which is quick enough to follow a
    /// dragged slider.
    pub fn seek(&mut self, to: Duration, exact: bool) {
        self.inner.seek(to, exact)
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        self.inner.set_volume(volume.clamp(0.0, 1.0), muted)
    }
}

/// Play a video silently to its end on the main thread and describe what
/// came out, for `chitra --play-test` (a check that the system player works,
/// used by CI where tests cannot reach the main thread).
pub fn play_test(path: &Path) -> Result<String, String> {
    let mut player = Player::open(
        path,
        Options {
            repaint: Arc::new(|| {}),
            max_side: 4096,
            audio: false,
            volume: 0.0,
            muted: true,
        },
    )?;
    let start = std::time::Instant::now();
    let mut pictures = 0;
    let mut size = [0, 0];
    loop {
        let status = player.status();
        if let Some(err) = status.error {
            return Err(err);
        }
        if let Some(frame) = player.take_frame() {
            pictures += 1;
            size = frame.size;
        }
        if status.ended && pictures > 0 {
            let d = status.duration.unwrap_or_default().as_secs_f32();
            return Ok(format!(
                "{}: {}x{}, {pictures} pictures, {d:.2} s",
                path.display(),
                size[0],
                size[1]
            ));
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(format!(
                "did not finish in 30 s ({pictures} pictures, at {:?})",
                status.position
            ));
        }
        // AVFoundation reports progress through the main run loop.
        #[cfg(all(feature = "video", target_os = "macos"))]
        objc2_core_foundation::CFRunLoop::run_in_mode(
            // SAFETY: reading an immutable system constant.
            unsafe { objc2_core_foundation::kCFRunLoopDefaultMode },
            0.005,
            false,
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A video's first picture, for its thumbnail, read silently on the calling
/// (worker) thread. `None` when `cancelled` says it is no longer wanted.
pub fn first_frame(
    path: &Path,
    max_side: u32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Result<ColorImage, String>> {
    #[cfg(all(feature = "video", target_os = "macos"))]
    {
        let _ = (max_side, cancelled);
        Some(avf::first_frame(path))
    }
    #[cfg(not(all(feature = "video", target_os = "macos")))]
    {
        let start = std::time::Instant::now();
        let mut player = match Player::open(
            path,
            Options {
                repaint: Arc::new(|| {}),
                max_side,
                audio: false,
                volume: 0.0,
                muted: true,
            },
        ) {
            Ok(p) => p,
            Err(e) => return Some(Err(e)),
        };
        loop {
            if cancelled() {
                return None;
            }
            if let Some(err) = player.status().error {
                return Some(Err(err));
            }
            if let Some(frame) = player.take_frame() {
                return Some(Ok(frame));
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Some(Err("no picture in this video".into()));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

/// Copy rows of 4-byte pixels into an egui image, dropping any row padding.
/// `order` gives the byte offsets of red, green and blue in each pixel.
pub(crate) fn image_from_rows(
    width: usize,
    height: usize,
    stride: usize,
    data: &[u8],
    order: [usize; 3],
) -> Option<ColorImage> {
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    let needed = stride.checked_mul(height - 1)?.checked_add(width * 4)?;
    if data.len() < needed {
        return None;
    }
    let [r, g, b] = order;
    let mut pixels = Vec::with_capacity(width * height);
    for row in data.chunks(stride).take(height) {
        pixels.extend(
            row[..width * 4]
                .chunks_exact(4)
                .map(|p| Color32::from_rgb(p[r], p[g], p[b])),
        );
    }
    Some(ColorImage::new([width, height], pixels))
}

/// Builds without the `video` feature list videos but cannot play them.
#[cfg(not(feature = "video"))]
mod unsupported {
    use super::*;

    pub struct Player;

    impl Player {
        pub fn open(_: &Path, _: Options) -> Result<Player, String> {
            Err("This build of Chitra cannot play videos".into())
        }
        pub fn status(&mut self) -> Status {
            Status::default()
        }
        pub fn take_frame(&mut self) -> Option<ColorImage> {
            None
        }
        pub fn poll_interval(&self) -> Option<Duration> {
            None
        }
        pub fn play(&mut self) {}
        pub fn pause(&mut self) {}
        pub fn seek(&mut self, _: Duration, _: bool) {}
        pub fn set_volume(&mut self, _: f32, _: bool) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[cfg(all(feature = "video", not(target_os = "macos")))]
    use std::time::Instant;

    #[test]
    fn rows_with_padding_become_pixels() {
        // 2x2 BGRA with 4 bytes of padding per row.
        let data = [
            1, 2, 3, 255, 4, 5, 6, 255, 0, 0, 0, 0, //
            7, 8, 9, 255, 10, 11, 12, 255, 0, 0, 0, 0,
        ];
        let img = image_from_rows(2, 2, 12, &data, [2, 1, 0]).unwrap();
        assert_eq!(img.size, [2, 2]);
        assert_eq!(img.pixels[0], Color32::from_rgb(3, 2, 1));
        assert_eq!(img.pixels[3], Color32::from_rgb(12, 11, 10));
        // Too short for the rows it claims, or narrower than a row.
        assert!(image_from_rows(2, 3, 12, &data, [2, 1, 0]).is_none());
        assert!(image_from_rows(4, 2, 12, &data, [2, 1, 0]).is_none());
    }

    fn data(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name)
    }

    // Runs where the system player is there and can decode H.264: build
    // machines for Windows and macOS have it; on Linux CI installs GStreamer.
    #[cfg(feature = "video")]
    #[test]
    fn reads_the_first_picture_of_an_h264_phone_clip() {
        if std::env::var_os("CHITRA_SKIP_VIDEO_TESTS").is_some() {
            return;
        }
        let frame = first_frame(&data("h264-aac.mp4"), 4096, &|| false)
            .unwrap()
            .unwrap();
        // Coded at 160x120; players may scale but keep the shape.
        let [w, h] = frame.size;
        assert!(w >= 80 && h >= 60, "{w}x{h}");
        assert_eq!(w * 3, h * 4, "{w}x{h}");
    }

    // AVPlayer only runs on the main thread, which tests do not get; CI
    // plays a clip with `chitra --play-test` on macOS instead.
    #[cfg(all(feature = "video", not(target_os = "macos")))]
    #[test]
    fn seeking_and_pausing_work() {
        if std::env::var_os("CHITRA_SKIP_VIDEO_TESTS").is_some() {
            return;
        }
        let mut player = Player::open(
            &data("h264-aac.mp4"),
            Options {
                repaint: Arc::new(|| {}),
                max_side: 4096,
                audio: false,
                volume: 0.0,
                muted: true,
            },
        )
        .unwrap();
        let start = Instant::now();
        let mut ended = false;
        while start.elapsed() < Duration::from_secs(20) {
            let s = player.status();
            assert_eq!(s.error, None);
            if s.ended {
                ended = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ended, "a one second clip should end");
        let s = player.status();
        let d = s.duration.expect("duration");
        assert!(
            d > Duration::from_millis(800) && d < Duration::from_millis(1300),
            "{d:?}"
        );

        player.pause();
        player.seek(Duration::from_millis(500), true);
        let start = Instant::now();
        let mut s = player.status();
        while start.elapsed() < Duration::from_secs(10) && (s.ended || s.position.is_zero()) {
            std::thread::sleep(Duration::from_millis(10));
            s = player.status();
        }
        assert!(!s.playing, "{s:?}");
        assert!(!s.ended, "{s:?}");
        let p = s.position.as_millis();
        assert!((300..=700).contains(&p), "{s:?}");
    }
}

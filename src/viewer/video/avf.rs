//! macOS: AVFoundation's AVPlayer (what QuickTime Player uses), with an
//! AVPlayerItemVideoOutput we ask for each new picture as BGRA. AVPlayer
//! belongs to the main thread, where the viewer's UI runs. Thumbnails are
//! read on worker threads with an AVAssetReader instead.

use std::path::Path;
use std::time::Duration;

use eframe::egui::ColorImage;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVAsset, AVAssetReader, AVAssetReaderOutput, AVAssetReaderTrackOutput, AVMediaTypeVideo,
    AVPlayer, AVPlayerItem, AVPlayerItemOutput, AVPlayerItemStatus, AVPlayerItemVideoOutput,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{CMTime, kCMTimeZero};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{Options, Status, image_from_rows};

pub struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    playing: bool,
}

/// Output settings asking for 32-bit BGRA pictures.
fn bgra_settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: kCVPixelBufferPixelFormatTypeKey is a constant CFString, which
    // is toll-free bridged to NSString, so the pointer cast is the documented
    // way to use it as a dictionary key.
    let key: &NSString = unsafe {
        let cf: &CFString = kCVPixelBufferPixelFormatTypeKey;
        &*(cf as *const CFString).cast::<NSString>()
    };
    let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let value: &AnyObject = &format;
    NSDictionary::from_slices(&[key], &[value])
}

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn seconds(t: CMTime) -> Option<Duration> {
    // SAFETY: CMTimeGetSeconds takes the time by value.
    let s = unsafe { t.seconds() };
    (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s))
}

impl Player {
    pub fn open(path: &Path, options: Options) -> Result<Player, String> {
        let mtm = MainThreadMarker::new().ok_or("videos can only be played on the main thread")?;
        let url = file_url(path);
        // SAFETY: AVFoundation calls on the main thread (`mtm`) with valid
        // objects; the settings dictionary has the types the API expects.
        unsafe {
            let item = AVPlayerItem::playerItemWithURL(&url, mtm);
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                AVPlayerItemVideoOutput::alloc(),
                Some(&bgra_settings()),
            );
            let as_output: &AVPlayerItemOutput = &output;
            item.addOutput(as_output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setVolume(if options.audio { options.volume } else { 0.0 });
            player.setMuted(options.muted || !options.audio);
            player.play();
            Ok(Player {
                player,
                item,
                output,
                playing: true,
            })
        }
    }

    pub fn status(&mut self) -> Status {
        // SAFETY: read-only queries of objects owned by this player.
        unsafe {
            let error = (self.item.status() == AVPlayerItemStatus::Failed).then(|| {
                self.item
                    .error()
                    .map(|e| format!("Cannot play this video: {}", e.localizedDescription()))
                    .unwrap_or_else(|| "Cannot play this video".into())
            });
            let position = seconds(self.item.currentTime()).unwrap_or_default();
            let duration = seconds(self.item.duration()).filter(|d| !d.is_zero());
            let ended = duration.is_some_and(|d| position + Duration::from_millis(1) >= d);
            if ended || error.is_some() {
                self.playing = false;
            }
            Status {
                position,
                duration,
                playing: self.playing,
                ended,
                error,
            }
        }
    }

    pub fn take_frame(&mut self) -> Option<ColorImage> {
        // SAFETY: the output belongs to this player's item; the pixel buffer
        // is locked while its memory is read and unlocked after.
        unsafe {
            let time = self.item.currentTime();
            if !self.output.hasNewPixelBufferForItemTime(time) {
                return None;
            }
            let buffer = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(time, std::ptr::null_mut())?;
            copy_bgra(&buffer)
        }
    }

    pub fn poll_interval(&self) -> Option<Duration> {
        self.playing.then_some(Duration::from_millis(8))
    }

    pub fn play(&mut self) {
        // SAFETY: plain AVPlayer calls on the main thread that owns it.
        unsafe {
            if self.status().ended {
                self.player.seekToTime(kCMTimeZero);
            }
            self.player.play();
        }
        self.playing = true;
    }

    pub fn pause(&mut self) {
        // SAFETY: as above.
        unsafe { self.player.pause() };
        self.playing = false;
    }

    pub fn seek(&mut self, to: Duration, exact: bool) {
        // SAFETY: as above; times are passed by value.
        unsafe {
            let t = CMTime::with_seconds(to.as_secs_f64(), 600);
            if exact {
                self.player
                    .seekToTime_toleranceBefore_toleranceAfter(t, kCMTimeZero, kCMTimeZero);
            } else {
                self.player.seekToTime(t);
            }
        }
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        // SAFETY: as above.
        unsafe {
            self.player.setVolume(volume);
            self.player.setMuted(muted);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: stop playback before the objects are released.
        unsafe {
            self.player.pause();
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}

/// Copy a locked BGRA pixel buffer into an egui image.
///
/// # Safety
/// `buffer` must be a valid 32-bit BGRA pixel buffer.
unsafe fn copy_bgra(buffer: &CVPixelBuffer) -> Option<ColorImage> {
    // SAFETY: the buffer is locked read-only around the read, and only the
    // `bytes_per_row * height` bytes CoreVideo reports for it are read.
    unsafe {
        if CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly)
            != kCVReturnSuccess
        {
            return None;
        }
        let width = CVPixelBufferGetWidth(buffer);
        let height = CVPixelBufferGetHeight(buffer);
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
        let image = if base.is_null() {
            None
        } else {
            let bytes = std::slice::from_raw_parts(base, stride * height);
            image_from_rows(width, height, stride, bytes, [2, 1, 0])
        };
        CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        image
    }
}

/// The first picture of a video, read with an AVAssetReader (any thread).
pub fn first_frame(path: &Path) -> Result<ColorImage, String> {
    // SAFETY: AVAssetReader and friends may be used from any thread; all
    // objects passed are valid for the calls.
    unsafe {
        let asset = AVAsset::assetWithURL(&file_url(path));
        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .map_err(|e| e.localizedDescription().to_string())?;
        let video = AVMediaTypeVideo.ok_or("AVFoundation is missing")?;
        #[allow(deprecated)]
        let tracks = asset.tracksWithMediaType(video);
        let track = tracks.firstObject().ok_or("this file has no video")?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &track,
            Some(&bgra_settings()),
        );
        let as_output: &AVAssetReaderOutput = &output;
        reader.addOutput(as_output);
        if !reader.startReading() {
            return Err("macOS cannot read this video".into());
        }
        let sample = as_output
            .copyNextSampleBuffer()
            .ok_or("macOS could not decode this video")?;
        let buffer = sample.image_buffer().ok_or("no picture in this video")?;
        let image = copy_bgra(&buffer).ok_or_else(|| "unreadable picture".to_string());
        reader.cancelReading();
        image
    }
}

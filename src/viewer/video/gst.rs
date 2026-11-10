//! Linux: GStreamer's playbin, with an appsink that hands us RGBA pictures.
//! Which codecs work depends on the GStreamer plugins installed (most
//! desktops have H.264 and HEVC through gst-libav or the VA-API plugins).

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use eframe::egui::ColorImage;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;

use super::{FrameSlot, Options, Status, image_from_rows};

pub struct Player {
    playbin: gst::Element,
    bus: gst::Bus,
    frames: FrameSlot,
    duration: Option<Duration>,
    ended: bool,
    error: Option<String>,
    /// What we last asked for; the pipeline gets there asynchronously.
    playing: bool,
}

fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| format!("GStreamer is not available: {e}")))
        .clone()
}

impl Player {
    pub fn open(path: &Path, options: Options) -> Result<Player, String> {
        init()?;
        let playbin = gst::ElementFactory::make("playbin").build().map_err(|_| {
            "GStreamer's playbin is missing; install the GStreamer base plugins".to_string()
        })?;
        let uri = gst::glib::filename_to_uri(path, None).map_err(|e| e.to_string())?;

        let caps = gst_video::VideoCapsBuilder::new()
            .format(gst_video::VideoFormat::Rgba)
            .build();
        let sink = gst_app::AppSink::builder()
            .caps(&caps)
            .max_buffers(1)
            .drop(true)
            .build();
        let frames = FrameSlot::default();
        let on_sample = |frames: FrameSlot, repaint: super::Repaint, preroll: bool| {
            move |sink: &gst_app::AppSink| {
                let sample = if preroll {
                    sink.pull_preroll()
                } else {
                    sink.pull_sample()
                }
                .map_err(|_| gst::FlowError::Eos)?;
                if let Some(image) = sample_to_image(&sample) {
                    frames.put(image);
                    repaint();
                }
                Ok(gst::FlowSuccess::Ok)
            }
        };
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(on_sample(frames.clone(), options.repaint.clone(), false))
                .new_preroll(on_sample(frames.clone(), options.repaint.clone(), true))
                .build(),
        );

        playbin.set_property("uri", uri.as_str());
        playbin.set_property("video-sink", &sink);
        if !options.audio {
            if let Ok(fake) = gst::ElementFactory::make("fakesink")
                .property("sync", true)
                .build()
            {
                playbin.set_property("audio-sink", &fake);
            }
        }
        playbin.set_property("volume", options.volume as f64);
        playbin.set_property("mute", options.muted);

        let bus = playbin.bus().ok_or("GStreamer pipeline has no bus")?;
        // Bus messages (errors, end of stream) also wake the UI.
        let repaint = options.repaint.clone();
        bus.set_sync_handler(move |_, _| {
            repaint();
            gst::BusSyncReply::Pass
        });
        playbin
            .set_state(gst::State::Playing)
            .map_err(|_| "GStreamer could not start this video".to_string())?;
        Ok(Player {
            playbin,
            bus,
            frames,
            duration: None,
            ended: false,
            error: None,
            playing: true,
        })
    }

    pub fn status(&mut self) -> Status {
        while let Some(msg) = self.bus.pop() {
            match msg.view() {
                gst::MessageView::Eos(_) => {
                    self.ended = true;
                    self.playing = false;
                }
                gst::MessageView::Error(err) if self.error.is_none() => {
                    self.error = Some(describe_error(&err.error()));
                    self.playing = false;
                }
                gst::MessageView::DurationChanged(_) => self.duration = None,
                _ => {}
            }
        }
        if self.duration.is_none() {
            self.duration = self
                .playbin
                .query_duration::<gst::ClockTime>()
                .map(|t| Duration::from_nanos(t.nseconds()));
        }
        let position = if self.ended {
            self.duration
        } else {
            self.playbin
                .query_position::<gst::ClockTime>()
                .map(|t| Duration::from_nanos(t.nseconds()))
        };
        Status {
            position: position.unwrap_or_default(),
            duration: self.duration,
            playing: self.playing,
            ended: self.ended,
            error: self.error.clone(),
        }
    }

    pub fn take_frame(&mut self) -> Option<ColorImage> {
        self.frames.take()
    }

    pub fn poll_interval(&self) -> Option<Duration> {
        None
    }

    pub fn play(&mut self) {
        if self.ended {
            self.seek(Duration::ZERO, true);
        }
        self.playing = self.playbin.set_state(gst::State::Playing).is_ok();
    }

    pub fn pause(&mut self) {
        let _ = self.playbin.set_state(gst::State::Paused);
        self.playing = false;
    }

    pub fn seek(&mut self, to: Duration, exact: bool) {
        let flags = gst::SeekFlags::FLUSH
            | if exact {
                gst::SeekFlags::ACCURATE
            } else {
                gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST
            };
        if self.ended {
            // After the end the pipeline needs to be prerolled again.
            let _ = self.playbin.set_state(gst::State::Paused);
            let _ = self.playbin.state(gst::ClockTime::from_mseconds(500));
            self.ended = false;
        }
        let to = gst::ClockTime::from_nseconds(to.as_nanos() as u64);
        let _ = self.playbin.seek_simple(flags, to);
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        self.playbin.set_property("volume", volume as f64);
        self.playbin.set_property("mute", muted);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
        self.bus.unset_sync_handler();
    }
}

fn sample_to_image(sample: &gst::Sample) -> Option<ColorImage> {
    let buffer = sample.buffer()?;
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
    let stride = usize::try_from(*frame.plane_stride().first()?).ok()?;
    let data = frame.plane_data(0).ok()?;
    image_from_rows(
        frame.width() as usize,
        frame.height() as usize,
        stride,
        data,
        [0, 1, 2],
    )
}

fn describe_error(err: &gst::glib::Error) -> String {
    let missing = err.matches(gst::CoreError::MissingPlugin)
        || err.matches(gst::StreamError::CodecNotFound)
        || err.matches(gst::StreamError::WrongType);
    if missing {
        "No decoder for this video is installed. Install the GStreamer \
         libav plugins (gstreamer1.0-libav or gst-libav) to play it."
            .into()
    } else {
        format!("Cannot play this video: {err}")
    }
}

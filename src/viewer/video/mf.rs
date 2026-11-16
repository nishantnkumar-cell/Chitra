//! Windows: the Media Foundation media engine (what Windows' own apps play
//! video with) in frame-server mode. It decodes, plays the sound and keeps
//! time; on every new picture we have it draw that picture, scaled to the
//! screen, into a WIC bitmap and copy that out.
//!
//! Each player runs on its own thread, inside a multithreaded COM apartment,
//! and is driven by messages, so the UI thread never touches COM.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui::ColorImage;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFMediaEngine, IMFMediaEngineClassFactory,
    IMFMediaEngineEx, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MF_MEDIA_ENGINE_CALLBACK,
    MF_MEDIA_ENGINE_SEEK_MODE_APPROXIMATE, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION, MFARGB,
    MFCreateAttributes, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{BSTR, Interface};

use super::{FrameSlot, Options, Repaint, Status, image_from_rows};

enum Command {
    Play,
    Pause,
    Seek(Duration, bool),
    Volume(f32, bool),
}

pub struct Player {
    commands: Option<Sender<Command>>,
    status: Arc<Mutex<Status>>,
    frames: FrameSlot,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn open(path: &Path, options: Options) -> Result<Player, String> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status {
            playing: true,
            ..Status::default()
        }));
        let frames = FrameSlot::default();
        let worker = Worker {
            path: path.to_string_lossy().into_owned(),
            max_side: options.max_side.max(16),
            repaint: options.repaint.clone(),
            status: status.clone(),
            frames: frames.clone(),
        };
        let volume = if options.audio { options.volume } else { 0.0 };
        let muted = options.muted || !options.audio;
        let thread = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || worker.run(rx, ready_tx, volume, muted))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Player {
                commands: Some(tx),
                status,
                frames,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("the video player stopped unexpectedly".into())
            }
        }
    }

    pub fn status(&mut self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn take_frame(&mut self) -> Option<ColorImage> {
        self.frames.take()
    }

    pub fn poll_interval(&self) -> Option<Duration> {
        None
    }

    fn send(&self, command: Command) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(command);
        }
    }

    pub fn play(&mut self) {
        if let Ok(mut s) = self.status.lock() {
            s.playing = true;
            s.ended = false;
        }
        self.send(Command::Play);
    }

    pub fn pause(&mut self) {
        if let Ok(mut s) = self.status.lock() {
            s.playing = false;
        }
        self.send(Command::Pause);
    }

    pub fn seek(&mut self, to: Duration, exact: bool) {
        self.send(Command::Seek(to, exact));
    }

    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        self.send(Command::Volume(volume, muted));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Closing the channel ends the worker, which shuts the engine down.
        self.commands = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Tells the UI to look again whenever the engine reports an event.
#[windows_core::implement(IMFMediaEngineNotify)]
struct Notify(Repaint);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, _event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        (self.0)();
        Ok(())
    }
}

struct Worker {
    path: String,
    max_side: u32,
    repaint: Repaint,
    status: Arc<Mutex<Status>>,
    frames: FrameSlot,
}

/// The bitmap the engine draws into, kept while the picture size stays.
struct Target {
    bitmap: IWICBitmap,
    width: u32,
    height: u32,
}

impl Worker {
    fn run(
        self,
        commands: Receiver<Command>,
        ready: Sender<Result<(), String>>,
        volume: f32,
        muted: bool,
    ) {
        // SAFETY: plain initialisation calls for this thread, balanced below.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if com.is_err() {
            let _ = ready.send(Err(format!("COM did not start: {com:?}")));
            return;
        }
        // SAFETY: as above; MFStartup is reference counted per process.
        let mf = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) };
        if let Err(e) = mf {
            let _ = ready.send(Err(format!("Media Foundation did not start: {e}")));
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
            return;
        }

        match self.create(volume, muted) {
            Ok((engine, wic)) => {
                let _ = ready.send(Ok(()));
                self.play_loop(&engine, &wic, &commands);
                // SAFETY: the engine is ours and no longer used after this.
                unsafe {
                    let _ = engine.Shutdown();
                }
            }
            Err(e) => {
                let _ = ready.send(Err(e));
            }
        }
        // SAFETY: balances MFStartup and CoInitializeEx; every COM object of
        // this thread has been dropped or shut down by now.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }

    fn create(
        &self,
        volume: f32,
        muted: bool,
    ) -> Result<(IMFMediaEngine, IWICImagingFactory), String> {
        fn err(what: &'static str) -> impl Fn(windows::core::Error) -> String {
            move |e| format!("{what}: {}", e.message())
        }
        // SAFETY: COM calls on this thread's apartment with valid arguments;
        // every pointer passed lives for the duration of its call.
        unsafe {
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(err("Media Foundation is not available"))?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 2).map_err(err("Media Foundation"))?;
            let attributes = attributes.ok_or("Media Foundation gave no attributes")?;
            let notify: IMFMediaEngineNotify = Notify(self.repaint.clone()).into();
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
                .map_err(err("Media Foundation"))?;
            attributes
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(err("Media Foundation"))?;
            let engine = factory
                .CreateInstance(0, &attributes)
                .map_err(err("Could not create a video player"))?;
            let _ = engine.SetVolume(f64::from(volume));
            let _ = engine.SetMuted(muted);
            engine
                .SetSource(&BSTR::from(self.path.as_str()))
                .map_err(err("Cannot open this video"))?;
            engine.Play().map_err(err("Cannot play this video"))?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(err("Windows Imaging Component is not available"))?;
            Ok((engine, wic))
        }
    }

    fn play_loop(
        &self,
        engine: &IMFMediaEngine,
        wic: &IWICImagingFactory,
        commands: &Receiver<Command>,
    ) {
        let mut target: Option<Target> = None;
        let mut last_pts = None;
        let mut playing = true;
        loop {
            // A short wait while playing so no picture is late; longer when
            // paused, where only seeks change the picture.
            let wait = if playing {
                Duration::from_millis(4)
            } else {
                Duration::from_millis(20)
            };
            match commands.recv_timeout(wait) {
                Ok(command) => {
                    self.apply(engine, command, &mut playing);
                    while let Ok(command) = commands.try_recv() {
                        self.apply(engine, command, &mut playing);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            // SAFETY: engine calls on its own thread with valid out pointers.
            let pts = unsafe {
                if engine.HasVideo().as_bool() {
                    engine.OnVideoStreamTick().ok()
                } else {
                    None
                }
            };
            if let Some(pts) = pts.filter(|p| Some(*p) != last_pts) {
                if let Some(image) = self.grab(engine, wic, &mut target) {
                    last_pts = Some(pts);
                    self.frames.put(image);
                    (self.repaint)();
                }
            }

            let status = self.read_status(engine, playing);
            if status.ended || status.error.is_some() {
                playing = false;
            }
            if let Ok(mut s) = self.status.lock() {
                *s = status;
            }
        }
    }

    fn apply(&self, engine: &IMFMediaEngine, command: Command, playing: &mut bool) {
        // SAFETY: engine calls on its own thread; all arguments are values.
        unsafe {
            match command {
                Command::Play => {
                    // After the end, Play starts again from the beginning.
                    let _ = engine.Play();
                    *playing = true;
                }
                Command::Pause => {
                    let _ = engine.Pause();
                    *playing = false;
                }
                Command::Seek(to, exact) => {
                    let secs = to.as_secs_f64();
                    let quick = (!exact)
                        .then(|| engine.cast::<IMFMediaEngineEx>().ok())
                        .flatten();
                    match quick {
                        Some(ex) => {
                            let _ =
                                ex.SetCurrentTimeEx(secs, MF_MEDIA_ENGINE_SEEK_MODE_APPROXIMATE);
                        }
                        None => {
                            let _ = engine.SetCurrentTime(secs);
                        }
                    }
                }
                Command::Volume(volume, muted) => {
                    let _ = engine.SetVolume(f64::from(volume));
                    let _ = engine.SetMuted(muted);
                }
            }
        }
    }

    fn read_status(&self, engine: &IMFMediaEngine, playing: bool) -> Status {
        // SAFETY: read-only engine queries on its own thread.
        unsafe {
            let secs = |s: f64| (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s));
            let ended = engine.IsEnded().as_bool();
            let error = engine.GetError().ok().map(|e| {
                let code = e.GetErrorCode();
                let detail = match e.GetExtendedErrorCode() {
                    Err(x) => format!(" ({})", x.message()),
                    Ok(()) => String::new(),
                };
                match code {
                    4 => format!("Windows has no decoder for this video{detail}"),
                    3 => format!("Windows could not decode this video{detail}"),
                    _ => format!("Cannot play this video{detail}"),
                }
            });
            Status {
                position: secs(engine.GetCurrentTime()).unwrap_or_default(),
                duration: secs(engine.GetDuration()),
                playing: playing && !ended && error.is_none() && !engine.IsPaused().as_bool(),
                ended,
                error,
            }
        }
    }

    /// Have the engine draw the current picture, scaled to fit `max_side`,
    /// and copy it out of the bitmap.
    fn grab(
        &self,
        engine: &IMFMediaEngine,
        wic: &IWICImagingFactory,
        target: &mut Option<Target>,
    ) -> Option<ColorImage> {
        // SAFETY: COM calls on this thread. The locked bitmap memory is only
        // read while `lock` is alive, within the size WIC reports for it.
        unsafe {
            let (mut w, mut h) = (0u32, 0u32);
            engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).ok()?;
            if w == 0 || h == 0 {
                return None;
            }
            let scale = (self.max_side as f64 / w.max(h) as f64).min(1.0);
            let tw = ((w as f64 * scale).round() as u32).max(1);
            let th = ((h as f64 * scale).round() as u32).max(1);
            if !target
                .as_ref()
                .is_some_and(|t| t.width == tw && t.height == th)
            {
                let bitmap = wic
                    .CreateBitmap(tw, th, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad)
                    .ok()?;
                *target = Some(Target {
                    bitmap,
                    width: tw,
                    height: th,
                });
            }
            let t = target.as_ref()?;
            let dst = RECT {
                left: 0,
                top: 0,
                right: tw as i32,
                bottom: th as i32,
            };
            let black = MFARGB {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbAlpha: 255,
            };
            engine
                .TransferVideoFrame(&t.bitmap, None, &dst, Some(&black))
                .ok()?;
            let rect = WICRect {
                X: 0,
                Y: 0,
                Width: tw as i32,
                Height: th as i32,
            };
            let lock = t.bitmap.Lock(&rect, WICBitmapLockRead.0 as u32).ok()?;
            let stride = lock.GetStride().ok()? as usize;
            let mut size = 0u32;
            let mut data = std::ptr::null_mut();
            lock.GetDataPointer(&mut size, &mut data).ok()?;
            if data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, size as usize);
            image_from_rows(tw as usize, th as usize, stride, bytes, [2, 1, 0])
        }
    }
}

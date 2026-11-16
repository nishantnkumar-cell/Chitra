//! The single view for a video: the picture fitted to the window and a
//! control bar (play/pause, position, time, mute and volume) that hides
//! while the video plays and the mouse is still.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

use chitra::video::VideoInfo;

use super::video::{Options, Player, Status};

/// How long the controls stay up after the mouse last moved.
const CONTROLS_LINGER: Duration = Duration::from_millis(2500);
const BAR_HEIGHT: f32 = 44.0;
/// Shift+Left/Right jump.
pub const SEEK_STEP: Duration = Duration::from_secs(5);

pub struct VideoView {
    path: PathBuf,
    player: Result<Player, String>,
    info: Option<VideoInfo>,
    texture: Option<egui::TextureHandle>,
    status: Status,
    last_activity: Instant,
    /// Where the slider is being dragged to, as a fraction of the length.
    scrub: Option<f32>,
    pub volume: f32,
    pub muted: bool,
}

impl VideoView {
    pub fn open(
        ctx: &egui::Context,
        path: &Path,
        max_side: u32,
        volume: f32,
        muted: bool,
    ) -> VideoView {
        let info = chitra::video::probe(path).ok();
        let repaint_ctx = ctx.clone();
        let player = Player::open(
            path,
            Options {
                repaint: Arc::new(move || repaint_ctx.request_repaint()),
                max_side,
                audio: true,
                volume,
                muted,
            },
        );
        VideoView {
            path: path.to_path_buf(),
            player,
            info,
            texture: None,
            status: Status::default(),
            last_activity: Instant::now(),
            scrub: None,
            volume,
            muted,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn info(&self) -> Option<&VideoInfo> {
        self.info.as_ref()
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Size of the picture as shown (after the phone's rotation).
    pub fn display_size(&self) -> Option<[u32; 2]> {
        self.info
            .as_ref()
            .filter(|i| i.width > 0)
            .map(|i| i.display_size())
            .or_else(|| {
                self.texture
                    .as_ref()
                    .map(|t| [t.size()[0] as u32, t.size()[1] as u32])
            })
    }

    pub fn toggle_play(&mut self) {
        self.last_activity = Instant::now();
        let Ok(player) = &mut self.player else { return };
        if self.status.playing {
            player.pause();
            self.status.playing = false;
        } else {
            player.play();
            self.status.playing = true;
            self.status.ended = false;
        }
    }

    /// Jump forwards (or backwards when `forward` is false) by `step`.
    pub fn seek_by(&mut self, step: Duration, forward: bool) {
        self.last_activity = Instant::now();
        let Ok(player) = &mut self.player else { return };
        let pos = self.status.position;
        let mut to = if forward {
            pos + step
        } else {
            pos.saturating_sub(step)
        };
        if let Some(d) = self.status.duration {
            to = to.min(d);
        }
        player.seek(to, true);
        self.status.position = to;
    }

    pub fn toggle_mute(&mut self) {
        self.last_activity = Instant::now();
        self.muted = !self.muted;
        if let Ok(player) = &mut self.player {
            player.set_volume(self.volume, self.muted);
        }
    }

    /// Fetch the newest picture and where playback is. Call every frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        let Ok(player) = &mut self.player else { return };
        self.status = player.status();
        if let Some(image) = player.take_frame() {
            match &mut self.texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("video", image, egui::TextureOptions::LINEAR));
                }
            }
        }
        if self.status.playing {
            if let Some(every) = player.poll_interval() {
                ctx.request_repaint_after(every);
            }
            // Keep the clock moving between pictures of a slow video.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Draw the video and its controls in `rect`. `rotation` is the view's
    /// own rotation (R / Shift+R) on top of the phone's.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, rotation: u8) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(rect);
        if ui.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_pressed()) {
            self.last_activity = Instant::now();
        }

        let error = match &self.player {
            Err(e) => Some(e.clone()),
            Ok(_) => self.status.error.clone(),
        };
        if let Some(err) = error {
            let mut msg = format!("Cannot play this video\n{err}");
            if cfg!(windows) && self.info.as_ref().is_some_and(VideoInfo::is_hevc) {
                msg.push_str(
                    "\n\nThis video is HEVC (H.265), which iPhones and many Android \
                     phones record. Windows plays it once \"HEVC Video Extensions\" \
                     is installed from the Microsoft Store.",
                );
            }
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(16.0),
                Color32::LIGHT_RED,
            );
            return;
        }

        // The bar covers the bottom of the picture only while it is shown.
        let show_controls = !self.status.playing
            || self.scrub.is_some()
            || self.last_activity.elapsed() < CONTROLS_LINGER;
        if self.status.playing && !show_controls {
            if response.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::None);
            }
        } else if self.status.playing {
            ctx.request_repaint_after(CONTROLS_LINGER.saturating_sub(self.last_activity.elapsed()));
        }

        match &self.texture {
            Some(tex) => {
                let size = tex.size();
                let phone = self.info.as_ref().map_or(0, |i| upright_turns(i, size));
                let turns = (phone + rotation) % 4;
                let (w, h) = if turns % 2 == 1 {
                    (size[1] as f32, size[0] as f32)
                } else {
                    (size[0] as f32, size[1] as f32)
                };
                // Videos fill the window, smaller ones are enlarged too.
                let scale = (rect.width() / w).min(rect.height() / h);
                let shown = Rect::from_center_size(rect.center(), Vec2::new(w, h) * scale);
                paint_turned(&painter, tex.id(), shown, turns);
            }
            None => {
                let spinner = Rect::from_center_size(rect.center(), Vec2::splat(36.0));
                ui.put(spinner, egui::Spinner::new().size(36.0));
            }
        }

        if response.clicked()
            && !self
                .bar_rect(rect)
                .contains(response.interact_pointer_pos().unwrap_or_default())
        {
            self.toggle_play();
        }
        if show_controls {
            self.controls(ui, rect);
        }
    }

    fn bar_rect(&self, rect: Rect) -> Rect {
        Rect::from_min_max(
            Pos2::new(rect.left(), rect.bottom() - BAR_HEIGHT),
            rect.right_bottom(),
        )
    }

    fn controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let bar = self.bar_rect(rect);
        let painter = ui.painter_at(bar);
        painter.rect_filled(bar, 0.0, Color32::from_black_alpha(170));
        let fg = Color32::from_gray(235);
        let mid = bar.center().y;

        // Play / pause.
        let play_rect =
            Rect::from_center_size(Pos2::new(bar.left() + 26.0, mid), Vec2::splat(32.0));
        let play = ui
            .interact(play_rect, ui.scope_id().with("video-play"), Sense::click())
            .on_hover_text(if self.status.playing {
                "Pause (Space)"
            } else {
                "Play (Space)"
            });
        if play.hovered() {
            painter.rect_filled(play_rect, 6.0, Color32::from_white_alpha(30));
        }
        let c = play_rect.center();
        if self.status.playing {
            for dx in [-5.0, 5.0] {
                painter.rect_filled(
                    Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(5.0, 16.0)),
                    1.0,
                    fg,
                );
            }
        } else {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + Vec2::new(-6.0, -9.0),
                    c + Vec2::new(9.0, 0.0),
                    c + Vec2::new(-6.0, 9.0),
                ],
                fg,
                Stroke::NONE,
            ));
        }
        if play.clicked() {
            self.toggle_play();
        }

        // Mute and volume on the right.
        let vol_w = 90.0;
        let vol_rect = Rect::from_min_size(
            Pos2::new(bar.right() - 14.0 - vol_w, mid - 10.0),
            Vec2::new(vol_w, 20.0),
        );
        let mute_rect =
            Rect::from_center_size(Pos2::new(vol_rect.left() - 20.0, mid), Vec2::splat(28.0));
        let mute = ui
            .interact(mute_rect, ui.scope_id().with("video-mute"), Sense::click())
            .on_hover_text(if self.muted { "Unmute (M)" } else { "Mute (M)" });
        if mute.hovered() {
            painter.rect_filled(mute_rect, 6.0, Color32::from_white_alpha(30));
        }
        paint_speaker(
            &painter,
            mute_rect.center(),
            self.muted || self.volume == 0.0,
            fg,
        );
        if mute.clicked() {
            self.toggle_mute();
        }
        let vol = ui.interact(
            vol_rect,
            ui.scope_id().with("video-volume"),
            Sense::click_and_drag(),
        );
        if let Some(p) = vol
            .interact_pointer_pos()
            .filter(|_| vol.is_pointer_button_down_on())
        {
            self.volume = ((p.x - vol_rect.left()) / vol_rect.width()).clamp(0.0, 1.0);
            self.muted = false;
            self.last_activity = Instant::now();
            if let Ok(player) = &mut self.player {
                player.set_volume(self.volume, self.muted);
            }
        }
        let shown_volume = if self.muted { 0.0 } else { self.volume };
        paint_track(&painter, vol_rect, shown_volume, fg);

        // Time and the position slider in between.
        let duration = self.status.duration.unwrap_or_default();
        let time_text = format!(
            "{} / {}",
            clock(
                self.scrub
                    .map_or(self.status.position, |f| duration.mul_f32(f))
            ),
            clock(duration)
        );
        let font = egui::FontId::proportional(14.0);
        let time_w = painter
            .layout_no_wrap(time_text.clone(), font.clone(), fg)
            .size()
            .x;
        painter.text(
            Pos2::new(play_rect.right() + 10.0, mid),
            egui::Align2::LEFT_CENTER,
            time_text,
            font,
            fg,
        );
        let seek_rect = Rect::from_min_max(
            Pos2::new(play_rect.right() + 24.0 + time_w, mid - 10.0),
            Pos2::new(mute_rect.left() - 14.0, mid + 10.0),
        );
        if seek_rect.width() > 20.0 && !duration.is_zero() {
            let seek = ui.interact(
                seek_rect,
                ui.scope_id().with("video-seek"),
                Sense::click_and_drag(),
            );
            let at = |p: Pos2| ((p.x - seek_rect.left()) / seek_rect.width()).clamp(0.0, 1.0);
            if let Ok(player) = &mut self.player {
                if seek.dragged() {
                    if let Some(p) = seek.interact_pointer_pos() {
                        let f = at(p);
                        if self.scrub != Some(f) {
                            player.seek(duration.mul_f32(f), false);
                        }
                        self.scrub = Some(f);
                    }
                } else if seek.drag_stopped() || seek.clicked() {
                    let f = seek
                        .interact_pointer_pos()
                        .map(at)
                        .or(self.scrub)
                        .unwrap_or(0.0);
                    player.seek(duration.mul_f32(f), true);
                    self.status.position = duration.mul_f32(f);
                    self.status.ended = false;
                    self.scrub = None;
                }
            }
            let frac = self
                .scrub
                .unwrap_or_else(|| self.status.position.as_secs_f32() / duration.as_secs_f32());
            paint_track(&painter, seek_rect, frac.clamp(0.0, 1.0), fg);
        }
    }
}

/// Clockwise quarter turns that make a player's picture of size `picture`
/// upright. Players that already turned it (their pictures have the shown
/// shape, not the coded one) need none.
pub fn upright_turns(info: &VideoInfo, picture: [usize; 2]) -> u8 {
    if info.quarter_turns % 2 == 1 && info.width != info.height {
        let coded_wide = info.width > info.height;
        let picture_wide = picture[0] > picture[1];
        if coded_wide != picture_wide {
            return 0;
        }
    }
    info.quarter_turns % 4
}

/// Draw a texture into `rect` turned clockwise by `turns` quarter turns.
fn paint_turned(painter: &egui::Painter, texture: egui::TextureId, rect: Rect, turns: u8) {
    let mut mesh = egui::Mesh::with_texture(texture);
    let uvs = [
        Pos2::new(0.0, 0.0),
        Pos2::new(1.0, 0.0),
        Pos2::new(1.0, 1.0),
        Pos2::new(0.0, 1.0),
    ];
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let r = turns as usize;
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

/// A slider track filled to `frac`, with a knob.
fn paint_track(painter: &egui::Painter, rect: Rect, frac: f32, fg: Color32) {
    let y = rect.center().y;
    let track = Rect::from_min_max(
        Pos2::new(rect.left(), y - 2.0),
        Pos2::new(rect.right(), y + 2.0),
    );
    painter.rect_filled(track, 2.0, Color32::from_white_alpha(70));
    let x = rect.left() + rect.width() * frac;
    painter.rect_filled(
        Rect::from_min_max(track.min, Pos2::new(x, track.max.y)),
        2.0,
        fg,
    );
    painter.circle_filled(Pos2::new(x, y), 6.0, fg);
}

/// A small speaker, crossed out when muted.
fn paint_speaker(painter: &egui::Painter, c: Pos2, muted: bool, fg: Color32) {
    let body = vec![
        c + Vec2::new(-9.0, -3.5),
        c + Vec2::new(-5.0, -3.5),
        c + Vec2::new(0.0, -8.0),
        c + Vec2::new(0.0, 8.0),
        c + Vec2::new(-5.0, 3.5),
        c + Vec2::new(-9.0, 3.5),
    ];
    painter.add(egui::Shape::convex_polygon(body, fg, Stroke::NONE));
    let stroke = Stroke::new(1.8, fg);
    if muted {
        painter.line_segment([c + Vec2::new(3.0, -4.0), c + Vec2::new(10.0, 4.0)], stroke);
        painter.line_segment([c + Vec2::new(3.0, 4.0), c + Vec2::new(10.0, -4.0)], stroke);
    } else {
        for r in [5.0, 9.0] {
            let pts: Vec<Pos2> = (-4..=4)
                .map(|k| {
                    let a = k as f32 * 0.17;
                    c + Vec2::new(1.5 + r * a.cos(), r * a.sin())
                })
                .collect();
            painter.add(egui::Shape::line(pts, stroke));
        }
    }
}

/// 75 s -> "1:15"; an hour or more -> "1:02:03".
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(clock(Duration::from_millis(75_900)), "1:15");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(clock(Duration::ZERO), "0:00");
    }
}

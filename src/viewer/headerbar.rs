//! A GNOME-style header bar.
//!
//! GNOME on Wayland does not draw title bars: each app draws its own. There
//! Chtra turns off the window system's decorations and makes its toolbar the
//! title bar (drag to move, double-click to maximize, window buttons on the
//! right) and handles resizing from the window edges itself. KDE Plasma,
//! X11, Windows and macOS keep the desktop's own title bar.

use eframe::egui::{self, CursorIcon, Sense, ViewportCommand};
use egui::viewport::ResizeDirection;

/// Whether to draw our own title bar: GNOME (including Ubuntu's) on Wayland.
pub fn wanted() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("GNOME")))
}

/// Make the panel's background a title bar. Call before adding its widgets,
/// so buttons drawn later still get their clicks.
pub fn drag_area(ui: &mut egui::Ui) {
    let resp = ui.interact(
        ui.max_rect(),
        ui.scope_id().with("titlebar"),
        Sense::click_and_drag(),
    );
    if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

/// Minimize, maximize/restore and close, right to left, drawn like
/// GNOME's round title bar buttons.
pub fn window_buttons(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    if button(ui, Icon::Close, "Close") {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let (icon, tip) = if maximized {
        (Icon::Restore, "Restore")
    } else {
        (Icon::Maximize, "Maximize")
    };
    if button(ui, icon, tip) {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if button(ui, Icon::Minimize, "Minimize") {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 24.0), Sense::click());
    let visuals = ui.style().interact(&resp);
    let painter = ui.painter_at(rect);
    let c = rect.center();
    let bg = if resp.hovered() {
        visuals.bg_fill
    } else {
        ui.visuals().widgets.inactive.weak_bg_fill
    };
    painter.circle_filled(c, 11.0, bg);
    let stroke = egui::Stroke::new(1.4, visuals.fg_stroke.color);
    let d = 3.5;
    match icon {
        Icon::Minimize => {
            painter.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, d)], stroke);
        }
        Icon::Maximize => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::Vec2::splat(2.0 * d)),
                1.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Icon::Restore => {
            let back =
                egui::Rect::from_center_size(c + egui::vec2(1.5, -1.5), egui::Vec2::splat(1.6 * d));
            let front =
                egui::Rect::from_center_size(c + egui::vec2(-1.5, 1.5), egui::Vec2::splat(1.6 * d));
            painter.rect_stroke(back, 1.0, stroke, egui::StrokeKind::Middle);
            painter.rect_filled(front, 1.0, bg);
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Middle);
        }
        Icon::Close => {
            painter.line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
            painter.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
        }
    }
    resp.on_hover_text(tip).clicked()
}

/// Resize from the window edges and corners, as the desktop would.
pub fn resize_edges(ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|i| {
        let v = i.viewport();
        (v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false))
    });
    if maximized || fullscreen {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let r = ctx.content_rect();
    const EDGE: f32 = 6.0;
    let (left, right) = (pos.x - r.min.x < EDGE, r.max.x - pos.x < EDGE);
    let (top, bottom) = (pos.y - r.min.y < EDGE, r.max.y - pos.y < EDGE);
    let (dir, cursor) = match (left, right, top, bottom) {
        (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
        (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
        (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
        (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
        (true, ..) => (ResizeDirection::West, CursorIcon::ResizeWest),
        (_, true, ..) => (ResizeDirection::East, CursorIcon::ResizeEast),
        (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeNorth),
        (_, _, _, true) => (ResizeDirection::South, CursorIcon::ResizeSouth),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

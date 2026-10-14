//! Small custom-painted widgets. Everything is drawn with egui's painter so
//! the look is fully controlled by theme tokens, and every hover/press is
//! animated (egui only repaints while an animation is actually running).

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Id, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};

use super::images::Images;
use crate::api::models::Track;
use crate::theme::{self, Theme};

pub const HOVER_TIME: f32 = 0.12;

pub fn ease(ui: &Ui, id: Id, on: bool) -> f32 {
    ui.ctx().animate_bool_with_time_and_easing(id, on, HOVER_TIME, egui::emath::easing::cubic_out)
}

fn radius(theme: &Theme) -> CornerRadius {
    CornerRadius::same(theme.rounding.clamp(0.0, 255.0) as u8)
}

/// Single line of text, cut with "…" to fit `max_width`.
pub fn line(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping { max_width: max_width.max(0.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    ui.painter().layout_job(job)
}

pub fn body(theme: &Theme) -> FontId {
    FontId::proportional(theme.font_size)
}

pub fn small(theme: &Theme) -> FontId {
    FontId::proportional(theme.font_size * 0.88)
}

pub fn title_font(theme: &Theme) -> FontId {
    FontId::new(theme.font_size, theme::bold())
}

/// Album art with a rounded placeholder while it loads, then a fade-in.
pub fn art(ui: &Ui, images: &Images, rect: Rect, url: Option<&str>, theme: &Theme) {
    let r = radius(theme);
    ui.painter().rect_filled(rect, r, theme.colors.hover);
    let px = (rect.width() * ui.pixels_per_point()).round() as u32;
    if let Some((tex, alpha)) = url.and_then(|u| images.get(ui.ctx(), u, px)) {
        egui::Image::from_texture((tex, rect.size()))
            .corner_radius(r)
            .tint(Color32::WHITE.gamma_multiply(alpha))
            .paint_at(ui, rect);
    }
}

/// Height of a track/playlist row for the current theme.
pub fn row_height(theme: &Theme) -> f32 {
    if theme.show_art { theme.art_size + 14.0 } else { theme.font_size * 2.9 }
}

/// A full-width clickable row with a soft animated hover background.
fn row_frame(ui: &mut Ui, theme: &Theme, active: bool, sense: Sense) -> (Rect, Response) {
    let size = vec2(ui.available_width(), row_height(theme));
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let t = ease(ui, resp.id.with("hover"), resp.hovered());
        let fill = if active { theme.colors.selection } else { theme.colors.hover.gamma_multiply(t) };
        ui.painter().rect_filled(rect, radius(theme), fill);
    }
    (rect, resp)
}

/// What was clicked inside a track row, besides the row itself.
#[derive(Default)]
pub struct RowClicks {
    pub like: bool,
    pub artist: bool,
    pub album: bool,
}

/// A piece of text that acts as a link: underlined and pointing-hand on
/// hover. Returns whether it was clicked; its response is merged into
/// `row`, so right-clicking the text still opens the row's menu.
/// "Artist • Album • 388M plays" in one line of `max_w`, artist and album
/// as links. The album name gives way first, so the play count always fits.
/// Returns the artist and album link responses (None when not links).
pub fn byline(ui: &Ui, id: Id, pos: Pos2, max_w: f32, track: &Track, theme: &Theme, plays: bool) -> [Option<Response>; 2] {
    let (font, color) = (small(theme), theme.colors.text_dim);
    let right = pos.x + max_w;
    let sep = |ui: &Ui, x: f32| {
        let g = line(ui, " • ", font.clone(), color, f32::INFINITY);
        let w = g.size().x;
        ui.painter().galley(pos2(x, pos.y), g, color);
        x + w
    };
    let plays = track.plays.as_ref().filter(|_| plays).map(|p| line(ui, p, font.clone(), color, max_w));
    let reserve = plays.as_ref().map_or(0.0, |g| g.size().x + 14.0);
    let artist = line(ui, &track.artists, font.clone(), color, (max_w - reserve).max(0.0));
    let mut x = pos.x + artist.size().x;
    let artist = text_link(ui, id.with("artist"), pos, artist, color, track.artist_id.is_some());
    let mut album_link = None;
    if let Some(album) = &track.album {
        let ax = if track.artists.is_empty() { x } else { sep(ui, x) };
        if right - reserve - ax > 24.0 {
            let g = line(ui, album, font.clone(), color, right - reserve - ax);
            x = ax + g.size().x;
            album_link = text_link(ui, id.with("album"), pos2(ax, pos.y), g, color, track.album_id.is_some());
        }
    }
    if let Some(g) = plays {
        let px = if x > pos.x { sep(ui, x) } else { x };
        if px + g.size().x <= right + 0.5 {
            ui.painter().galley(pos2(px, pos.y), g, color);
        }
    }
    [artist, album_link]
}

/// Draws `galley` at `pos`; when `enabled` it's clickable, underlined on hover.
pub fn text_link(ui: &Ui, id: Id, pos: Pos2, galley: Arc<Galley>, color: Color32, enabled: bool) -> Option<Response> {
    let rect = Rect::from_min_size(pos, galley.size());
    ui.painter().galley(pos, galley, color);
    if !enabled {
        return None;
    }
    let resp = ui.interact(rect, id, Sense::click());
    if resp.hovered() {
        let y = rect.bottom() - 1.0;
        ui.painter().line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0_f32, color));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    Some(resp)
}

/// Track row: art, title (links to the album), "artist • album" (each a
/// link), and on the right the like button and duration. The like button
/// shows on hover, and always when the song is liked.
pub fn track_row(ui: &mut Ui, images: &Images, theme: &Theme, track: &Track, look: RowLook) -> (Response, RowClicks) {
    let sense = if look.draggable { Sense::click_and_drag() } else { Sense::click() };
    let (rect, mut resp) = row_frame(ui, theme, look.active, sense);
    let mut clicks = RowClicks::default();
    if !ui.is_rect_visible(rect) {
        return (resp, clicks);
    }
    let hovered = ui.rect_contains_pointer(rect);
    let pad = 8.0;
    let mut x = rect.left() + pad;
    if theme.show_art {
        let s = theme.art_size;
        art(ui, images, Rect::from_min_size(pos2(x, rect.center().y - s / 2.0), Vec2::splat(s)), track.thumb.as_deref(), theme);
        x += s + 12.0;
    }
    let c = &theme.colors;
    // Right side: duration, then the like button left of it.
    let mut right = rect.right() - pad;
    // Duration in a fixed-width, right-aligned column (reserved even when
    // missing), so the like button lines up on every row whatever the
    // digits: "4:48" is wider than "2:11".
    let mut slot = line(ui, "00:00", small(theme), c.text_dim, 80.0).size().x;
    if let Some(d) = &track.duration {
        let g = line(ui, d, small(theme), c.text_dim, 80.0);
        slot = slot.max(g.size().x);
        ui.painter().galley(pos2(right - g.size().x, rect.center().y - g.size().y / 2.0), g, c.text_dim);
    }
    right -= slot + 12.0;
    if look.can_like {
        let size = 26.0;
        let btn = Rect::from_center_size(pos2(right - size / 2.0, rect.center().y), Vec2::splat(size));
        right -= size + 8.0;
        let like = ui.interact(btn, resp.id.with("like"), Sense::click());
        let t = ease(ui, resp.id.with("like-vis"), hovered || look.liked);
        if t > 0.0 {
            let over = ease(ui, resp.id.with("like-hover"), like.hovered());
            let base = if look.liked { c.accent } else { c.text_dim.lerp_to_gamma(c.text, over) };
            thumbs_up(ui.painter(), btn.center(), 15.0, base.gamma_multiply(t), look.liked);
        }
        if like.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        clicks.like = like.clicked();
    }

    let row_id = resp.id;
    let max_w = (right - x).max(0.0);
    let title_color = if look.playing { c.accent } else { c.text };
    let title = line(ui, &track.title, title_font(theme), title_color, max_w);
    let sub_h = line(ui, " ", small(theme), c.text_dim, f32::INFINITY).size().y;
    let total = title.size().y + 2.0 + sub_h;
    let top = rect.center().y - total / 2.0;
    let sub_y = top + title.size().y + 2.0;
    ui.painter().galley(pos2(x, top), title, title_color);
    let [artist, album] = byline(ui, row_id, pos2(x, sub_y), max_w, track, theme, true);
    for (link, clicked) in [(artist, &mut clicks.artist), (album, &mut clicks.album)] {
        if let Some(link) = link {
            *clicked = link.clicked();
            resp = resp.union(link);
        }
    }
    if look.playing {
        let bar = Rect::from_center_size(pos2(rect.left() + 2.0, rect.center().y), vec2(3.0, rect.height() * 0.5));
        ui.painter().rect_filled(bar, CornerRadius::same(2), c.accent);
    }
    (resp.on_hover_cursor(egui::CursorIcon::PointingHand), clicks)
}

/// How a track row is drawn.
#[derive(Clone, Copy, Default)]
pub struct RowLook {
    pub playing: bool,
    pub active: bool,
    pub draggable: bool,
    pub can_like: bool,
    pub liked: bool,
}

/// Thumbs-up icon in a `size` box: outlined, or filled when `filled`.
/// Built from convex pieces, since egui only fills convex polygons.
pub fn thumbs_up(p: &egui::Painter, center: Pos2, size: f32, color: Color32, filled: bool) {
    let o = center - Vec2::splat(size / 2.0);
    let pt = |x: f32, y: f32| o + vec2(x, y) * size;
    if filled {
        p.rect_filled(Rect::from_min_max(pt(0.04, 0.42), pt(0.26, 0.96)), CornerRadius::same(1), color);
        let palm = vec![pt(0.33, 0.38), pt(0.90, 0.38), pt(0.97, 0.47), pt(0.87, 0.90), pt(0.80, 0.96), pt(0.33, 0.96)];
        p.add(Shape::convex_polygon(palm, color, Stroke::NONE));
        let thumb = vec![pt(0.33, 0.46), pt(0.51, 0.10), pt(0.59, 0.04), pt(0.67, 0.08), pt(0.66, 0.40), pt(0.33, 0.40)];
        p.add(Shape::convex_polygon(thumb, color, Stroke::NONE));
    } else {
        let stroke = Stroke::new(1.4_f32, color);
        p.rect_stroke(Rect::from_min_max(pt(0.04, 0.42), pt(0.26, 0.96)), CornerRadius::same(1), stroke, egui::StrokeKind::Middle);
        let hand = vec![
            pt(0.33, 0.96),
            pt(0.33, 0.46),
            pt(0.51, 0.10),
            pt(0.59, 0.04),
            pt(0.67, 0.08),
            pt(0.65, 0.38),
            pt(0.90, 0.38),
            pt(0.97, 0.47),
            pt(0.87, 0.90),
            pt(0.80, 0.96),
        ];
        p.add(Shape::closed_line(hand, stroke));
    }
}

/// Album/playlist tile: square art with title and subtitle under it.
pub fn card(ui: &mut Ui, images: &Images, theme: &Theme, title: &str, subtitle: &str, thumb: Option<&str>, size: f32) -> Response {
    let text_h = theme.font_size * 2.6;
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size + text_h + 6.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let t = ease(ui, resp.id.with("hover"), resp.hovered());
        let art_rect = Rect::from_min_size(rect.min, Vec2::splat(size)).shrink(2.0 * (1.0 - t));
        art(ui, images, art_rect, thumb, theme);
        let c = &theme.colors;
        let title_g = line(ui, title, title_font(theme), c.text, size);
        let y = rect.top() + size + 6.0;
        let h = title_g.size().y;
        ui.painter().galley(pos2(rect.left(), y), title_g, c.text);
        let sub = line(ui, subtitle, small(theme), c.text_dim, size);
        ui.painter().galley(pos2(rect.left(), y + h + 1.0), sub, c.text_dim);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Outlined rounded button, the quieter sibling of `pill`.
pub fn ghost_pill(ui: &mut Ui, theme: &Theme, label: &str) -> Response {
    let font = title_font(theme);
    let g = line(ui, label, font, theme.colors.text, f32::INFINITY);
    let size = g.size() + vec2(28.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = ease(ui, resp.id.with("hover"), resp.hovered());
    let c = &theme.colors;
    ui.painter().rect_filled(rect, CornerRadius::same(255), c.hover.gamma_multiply(t));
    let stroke = Stroke::new(1.0_f32, c.text_dim.lerp_to_gamma(c.text, t));
    ui.painter().rect_stroke(rect, CornerRadius::same(255), stroke, egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, c.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Playlist/album row: art with title and subtitle.
pub fn playlist_row(ui: &mut Ui, images: &Images, theme: &Theme, title: &str, subtitle: &str, thumb: Option<&str>) -> Response {
    let (rect, resp) = row_frame(ui, theme, false, Sense::click());
    if ui.is_rect_visible(rect) {
        let pad = 8.0;
        let mut x = rect.left() + pad;
        if theme.show_art {
            let s = theme.art_size;
            art(ui, images, Rect::from_min_size(pos2(x, rect.center().y - s / 2.0), Vec2::splat(s)), thumb, theme);
            x += s + 12.0;
        }
        let c = &theme.colors;
        let max_w = rect.right() - pad - x;
        let title = line(ui, title, title_font(theme), c.text, max_w);
        let sub = line(ui, subtitle, small(theme), c.text_dim, max_w);
        let total = title.size().y + 2.0 + sub.size().y;
        let top = rect.center().y - total / 2.0;
        let sub_y = top + title.size().y + 2.0;
        ui.painter().galley(pos2(x, top), title, c.text);
        ui.painter().galley(pos2(x, sub_y), sub, c.text_dim);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Sidebar entry with animated hover and selection.
pub fn nav_item(ui: &mut Ui, theme: &Theme, label: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), theme.font_size * 2.5), Sense::click());
    let c = &theme.colors;
    let hover = ease(ui, resp.id.with("hover"), resp.hovered());
    let sel = ease(ui, resp.id.with("sel"), selected);
    let p = ui.painter();
    p.rect_filled(rect, radius(theme), c.hover.gamma_multiply(hover * (1.0 - sel)));
    p.rect_filled(rect, radius(theme), c.selection.gamma_multiply(sel));
    let color = c.text_dim.lerp_to_gamma(c.text, hover.max(sel));
    let font = if selected { title_font(theme) } else { body(theme) };
    p.text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, font, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Rounded "pill" button: accent fill, label in the text color.
pub fn pill(ui: &mut Ui, theme: &Theme, label: &str) -> Response {
    let font = title_font(theme);
    let g = line(ui, label, font, theme.colors.text, f32::INFINITY);
    let size = g.size() + vec2(28.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = ease(ui, resp.id.with("hover"), resp.hovered());
    let fill = theme.colors.accent.gamma_multiply(0.85 + 0.15 * t);
    ui.painter().rect_filled(rect.expand(t * 1.5), CornerRadius::same(255), fill);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, theme.colors.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Text link-style button ("‹ Library").
pub fn text_button(ui: &mut Ui, theme: &Theme, label: &str) -> Response {
    let g = line(ui, label, body(theme), theme.colors.text_dim, f32::INFINITY);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(4.0, 4.0), Sense::click());
    let t = ease(ui, resp.id.with("hover"), resp.hovered());
    let color = theme.colors.text_dim.lerp_to_gamma(theme.colors.text, t);
    ui.painter().galley_with_override_text_color(rect.min + vec2(2.0, 2.0), g, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[derive(Clone, Copy)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
}

/// Round icon button. `primary` draws a filled accent circle.
pub fn icon_button(ui: &Ui, theme: &Theme, rect: Rect, id: Id, icon: Icon, primary: bool, enabled: bool) -> Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let c = &theme.colors;
    let t = ease(ui, id.with("hover"), resp.hovered() && enabled);
    let pressed = ease(ui, id.with("press"), resp.is_pointer_button_down_on());
    let scale = 1.0 + 0.06 * t - 0.08 * pressed;
    let center = rect.center();
    let r = rect.width().min(rect.height()) / 2.0 * scale;
    let p = ui.painter();
    let fg = if primary {
        p.circle_filled(center, r, c.accent);
        c.text
    } else {
        p.circle_filled(center, r, c.hover.gamma_multiply(t));
        c.text_dim.lerp_to_gamma(c.text, t)
    };
    let fg = if enabled { fg } else { fg.gamma_multiply(0.4) };
    paint_icon(p, icon, center, r * 0.42, fg);
    resp
}

fn paint_icon(p: &egui::Painter, icon: Icon, c: Pos2, s: f32, color: Color32) {
    let tri = |points: Vec<Pos2>| Shape::convex_polygon(points, color, Stroke::NONE);
    match icon {
        Icon::Play => {
            // Optical centring: nudge right a little.
            let c = c + vec2(s * 0.15, 0.0);
            p.add(tri(vec![pos2(c.x - s * 0.8, c.y - s), pos2(c.x + s, c.y), pos2(c.x - s * 0.8, c.y + s)]));
        }
        Icon::Pause => {
            let w = s * 0.55;
            for dx in [-s * 0.55, s * 0.55] {
                p.rect_filled(Rect::from_center_size(pos2(c.x + dx, c.y), vec2(w, s * 2.0)), CornerRadius::same(1), color);
            }
        }
        Icon::Next | Icon::Prev => {
            let dir = if matches!(icon, Icon::Next) { 1.0 } else { -1.0 };
            let tip = c.x + dir * s * 0.55;
            let back = c.x - dir * s * 0.85;
            p.add(tri(if dir > 0.0 {
                vec![pos2(back, c.y - s * 0.85), pos2(tip, c.y), pos2(back, c.y + s * 0.85)]
            } else {
                vec![pos2(back, c.y + s * 0.85), pos2(tip, c.y), pos2(back, c.y - s * 0.85)]
            }));
            let bar = Rect::from_center_size(pos2(tip + dir * s * 0.2, c.y), vec2(s * 0.3, s * 1.7));
            p.rect_filled(bar, CornerRadius::same(1), color);
        }
    }
}

pub struct SliderResult {
    /// Value under the pointer while dragging, else the given value.
    pub value: f32,
    pub dragging: bool,
    /// Pointer released (or clicked) this frame: commit `value`.
    pub committed: bool,
}

/// Thin progress-style slider; the track thickens and a knob appears on hover.
pub fn slim_slider(ui: &Ui, theme: &Theme, rect: Rect, id: Id, value: f32) -> SliderResult {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let dragging = resp.dragged() || resp.is_pointer_button_down_on();
    let mut v = value.clamp(0.0, 1.0);
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| dragging || resp.clicked()) {
        v = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    }
    let committed = resp.drag_stopped() || resp.clicked();
    let t = ease(ui, id.with("hover"), resp.hovered() || dragging);
    let c = &theme.colors;
    let h = 3.0 + 2.0 * t;
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), h));
    let p = ui.painter();
    p.rect_filled(track, CornerRadius::same(255), c.hover.lerp_to_gamma(c.selection, 0.5));
    let filled = Rect::from_min_max(track.min, pos2(track.left() + track.width() * v, track.bottom()));
    p.rect_filled(filled, CornerRadius::same(255), c.accent);
    if t > 0.0 {
        p.circle_filled(pos2(filled.right(), track.center().y), 6.0 * t, c.text);
    }
    if resp.hovered() || dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    SliderResult { value: v, dragging, committed }
}

/// "3:07" / "1:02:03".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

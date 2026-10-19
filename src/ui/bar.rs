//! Now-playing bar: art + title on the left, transport and seek bar in the
//! middle, volume on the right. Painted directly into fixed rects.

use egui::{Align2, Id, Rect, Sense, Ui, pos2, vec2};

use super::app::App;
use super::widgets::{self, Icon, icon_button, line, slim_slider};

/// Content height of the bar (padding is added around it).
pub const HEIGHT: f32 = 58.0;

enum Action {
    Prev,
    Toggle,
    /// The pointer rests on play: the click is likely coming.
    Warm,
    Next,
    Seek(f64),
    /// Level, and whether to save it (on release, not every drag step).
    Volume(f32, bool),
    OpenAlbum(Box<crate::api::models::Track>),
    OpenArtist(String),
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let mut action = None;
    {
        let theme = &app.theme;
        let c = &theme.colors;
        let track = app.now_playing();
        let pb = &app.playback;

        // Left: art and title.
        let left_w = (rect.width() * 0.28).clamp(150.0, 360.0);
        let art = Rect::from_min_size(pos2(rect.left(), rect.center().y - HEIGHT / 2.0), vec2(HEIGHT, HEIGHT));
        widgets::art(ui, &app.images, art, track.and_then(|t| t.thumb.as_deref()), theme);
        let text_x = art.right() + 12.0;
        let text_w = rect.left() + left_w - text_x - 8.0;
        let title = track.map_or("Not playing", |t| t.title.as_str());
        let g_title = line(ui, title, widgets::title_font(theme), c.text, text_w);
        let sub_h = line(ui, " ", widgets::small(theme), c.text_dim, f32::INFINITY).size().y;
        let total = g_title.size().y + 3.0 + sub_h;
        let top = rect.center().y - total / 2.0;
        let sub_y = top + g_title.size().y + 3.0;
        ui.painter().galley(pos2(text_x, top), g_title, c.text);
        // "Artist • Album": artist opens the artist page, album the album.
        if let Some(t) = track {
            let links = widgets::byline(ui, Id::new("bar"), pos2(text_x, sub_y), text_w, t, theme, false);
            if let Some(id) = links.artist {
                action = Some(Action::OpenArtist(id));
            }
            if links.album {
                action = Some(Action::OpenAlbum(Box::new(t.clone())));
            }
        }

        // Right: volume (hidden when the window is narrow).
        let show_volume = rect.width() > 640.0;
        let right_w = if show_volume { 150.0 } else { 0.0 };
        if show_volume {
            let r = Rect::from_min_size(pos2(rect.right() - 120.0, rect.center().y - 8.0), vec2(120.0, 16.0));
            let s = slim_slider(ui, theme, r, Id::new("volume"), pb.volume);
            if s.committed || (s.dragging && (s.value - pb.volume).abs() >= 0.01) {
                action = Some(Action::Volume(s.value, s.committed));
            }
            // The level while it's being set (or pointed at).
            let label =
                if s.dragging || s.hovered { format!("{}%", (s.value * 100.0).round()) } else { "Vol".to_string() };
            ui.painter().text(pos2(r.left() - 10.0, r.center().y), Align2::RIGHT_CENTER, label, widgets::small(theme), c.text_dim);
        }

        // Centre: transport buttons and seek bar.
        let centre = Rect::from_x_y_ranges(rect.left() + left_w..=rect.right() - right_w, rect.y_range());
        let cx = centre.center().x;
        let by = rect.top() + 18.0;
        let enabled = track.is_some();
        let play_icon = Icon::PlayPause(widgets::ease(ui, Id::new("play-morph"), !pb.paused && track.is_some()));
        let btn = |x: f32, d: f32| Rect::from_center_size(pos2(x, by), vec2(d, d));
        if icon_button(ui, theme, btn(cx - 52.0, 30.0), Id::new("prev"), Icon::Prev, false, enabled).clicked() {
            action = Some(Action::Prev);
        }
        let play = icon_button(ui, theme, btn(cx, 38.0), Id::new("play"), play_icon, true, enabled);
        if play.clicked() {
            action = Some(Action::Toggle);
        } else if play.hovered() {
            action = Some(Action::Warm);
        }
        if icon_button(ui, theme, btn(cx + 52.0, 30.0), Id::new("next"), Icon::Next, false, enabled).clicked() {
            action = Some(Action::Next);
        }

        let seek_w = (centre.width() - 120.0).clamp(80.0, 560.0);
        let seek = Rect::from_center_size(pos2(cx, rect.bottom() - 9.0), vec2(seek_w, 14.0));
        let frac = if pb.duration > 0.0 { (pb.position() / pb.duration) as f32 } else { 0.0 };
        let s = slim_slider(ui, theme, seek, Id::new("seek"), frac);
        if s.committed && pb.duration > 0.0 {
            action = Some(Action::Seek(s.value as f64 * pb.duration));
        }
        let shown = if s.dragging { s.value as f64 * pb.duration } else { pb.position() };
        let (left_txt, right_txt) = if pb.loading && track.is_some() {
            ("Loading…".to_string(), String::new())
        } else if track.is_some() {
            (widgets::clock(shown), widgets::clock(pb.duration))
        } else {
            (String::new(), String::new())
        };
        let font = widgets::small(theme);
        ui.painter().text(pos2(seek.left() - 10.0, seek.center().y), Align2::RIGHT_CENTER, left_txt, font.clone(), c.text_dim);
        ui.painter().text(pos2(seek.right() + 10.0, seek.center().y), Align2::LEFT_CENTER, right_txt, font, c.text_dim);
    }

    match action {
        Some(Action::Prev) => app.prev(),
        Some(Action::Toggle) => app.toggle_pause(),
        Some(Action::Warm) => app.warm_resume(),
        Some(Action::Next) => app.next(),
        Some(Action::Seek(t)) => app.seek(t),
        Some(Action::Volume(v, save)) => app.set_volume(v, save),
        Some(Action::OpenAlbum(t)) => app.open_album(ui.ctx(), &t),
        Some(Action::OpenArtist(id)) => app.open_artist(ui.ctx(), &id),
        None => {}
    }
}

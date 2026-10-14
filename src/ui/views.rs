//! Sidebar and the main content views. Views draw with shared borrows and
//! return the action the user took; it is applied afterwards.

use egui::{Align, FontId, Layout, RichText, ScrollArea, TextEdit, Ui, vec2};

use super::app::{App, View};
use super::images::Images;
use super::widgets::{self, nav_item, pill, playlist_row, text_button, track_row};
use crate::api::models::Track;
use crate::theme::{self, Theme};

const VIEW_FADE: f64 = 0.18;

/// What a track list reported this frame.
#[derive(Default)]
struct ListResult {
    clicked: Option<usize>,
    hovered: Option<String>,
    /// Row under a held-down mouse button: the click hasn't landed yet.
    pressed: Option<String>,
    /// Rows laid out on screen this frame.
    on_screen: Vec<String>,
}

enum Action {
    Go(View),
    PlaySearch(usize),
    OpenPlaylist(usize),
    PlayPlaylist(usize),
    PlayQueue(usize),
    SignIn,
}

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let mut action = None;
    {
        let theme = &app.theme;
        ui.add_space(6.0);
        ui.label(RichText::new("ytmcross").font(FontId::new(theme.font_size * 1.25, theme::bold())).color(theme.colors.accent));
        ui.add_space(14.0);
        for view in app.config.layout.sidebar_items.iter().filter_map(|n| View::from_name(n)) {
            let selected = app.view == view || (view == View::Library && app.view == View::Playlist);
            if nav_item(ui, theme, view.label(), selected).clicked() {
                action = Some(Action::Go(view));
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            if status(app, ui) {
                action = Some(Action::SignIn);
            }
        });
    }
    apply(app, ui, action);
}

/// Notices and the sign-in prompt. Returns true if "Sign in again" was clicked.
fn status(app: &App, ui: &mut Ui) -> bool {
    let theme = &app.theme;
    let mut clicked = false;
    if app.signed_out {
        if app.signing_in {
            ui.label(RichText::new("Waiting for sign-in…").color(theme.colors.text_dim));
        } else {
            clicked = pill(ui, theme, "Sign in again").clicked();
            ui.label(RichText::new("Your session expired.").color(theme.colors.text_dim));
        }
    }
    if let Some((msg, _)) = &app.notice {
        ui.add(egui::Label::new(RichText::new(msg).font(widgets::small(theme)).color(theme.colors.text_dim)).wrap());
    }
    clicked
}

pub fn content(app: &mut App, ui: &mut Ui) {
    // Fade the view in after switching.
    let now = ui.input(|i| i.time);
    let t = ((now - app.view_since) / VIEW_FADE).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    ui.multiply_opacity(egui::emath::easing::cubic_out(t));

    let mut action = None;
    if !app.config.layout.sidebar {
        ui.horizontal(|ui| {
            for view in app.config.layout.sidebar_items.iter().filter_map(|n| View::from_name(n)) {
                if text_button(ui, &app.theme, view.label()).clicked() {
                    action = Some(Action::Go(view));
                }
                ui.add_space(8.0);
            }
        });
        if status(app, ui) {
            action = Some(Action::SignIn);
        }
        ui.add_space(8.0);
    }

    let playing = app.now_playing().map(|t| t.video_id.clone());
    let playing = playing.as_deref();
    let mut list = ListResult::default();
    action = match app.view {
        View::Search => search(app, ui, playing, &mut list),
        View::Library => library(app, ui),
        View::Playlist => playlist(app, ui, playing, &mut list),
        View::Queue => queue(app, ui, &mut list),
    }
    .or(action);
    // Let the resolver get ahead of what the user is about to do.
    app.hovered(ui.ctx(), list.hovered.as_deref());
    app.pressing(list.pressed.as_deref());
    app.on_screen(ui.ctx(), list.on_screen);
    apply(app, ui, action);
}

fn apply(app: &mut App, ui: &Ui, action: Option<Action>) {
    let ctx = ui.ctx();
    match action {
        Some(Action::Go(view)) => app.go(ctx, view),
        Some(Action::PlaySearch(i)) => {
            if let Some(t) = app.search.results.get(i).cloned() {
                app.play_list(vec![t], 0, true);
            }
        }
        Some(Action::OpenPlaylist(i)) => {
            if let Some(meta) = app.library.as_ref().and_then(|l| l.get(i)).cloned() {
                app.open_playlist(ctx, meta);
            }
        }
        Some(Action::PlayPlaylist(i)) => {
            if let Some(tracks) = app.playlist.as_ref().map(|p| p.tracks.clone()) {
                app.play_list(tracks, i, false);
            }
        }
        Some(Action::PlayQueue(i)) => app.play_queue_index(i),
        Some(Action::SignIn) => app.sign_in_again(),
        None => {}
    }
}

fn heading(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).font(FontId::new(theme.font_size * 1.7, theme::bold())).color(theme.colors.text));
    ui.add_space(10.0);
}

fn dim(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(RichText::new(text).color(theme.colors.text_dim));
}

fn spinner(ui: &mut Ui, theme: &Theme) {
    ui.add_space(12.0);
    ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(22.0).color(theme.colors.accent)));
}

/// Virtualized list: only rows inside the viewport are laid out and painted.
fn track_list(ui: &mut Ui, images: &Images, theme: &Theme, tracks: &[Track], playing: Option<&str>, current: Option<usize>) -> ListResult {
    let mut out = ListResult::default();
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), tracks.len(), |ui, range| {
        for i in range {
            let t = &tracks[i];
            let resp = track_row(ui, images, theme, t, playing == Some(t.video_id.as_str()), current == Some(i));
            if resp.clicked() {
                out.clicked = Some(i);
            }
            if resp.hovered() {
                out.hovered = Some(t.video_id.clone());
            }
            if resp.is_pointer_button_down_on() {
                out.pressed = Some(t.video_id.clone());
            }
            out.on_screen.push(t.video_id.clone());
        }
    });
    out
}

fn search(app: &mut App, ui: &mut Ui, playing: Option<&str>, list: &mut ListResult) -> Option<Action> {
    heading(ui, &app.theme, "Search");
    let font = FontId::proportional(app.theme.font_size * 1.1);
    let resp = ui.add(
        TextEdit::singleline(&mut app.search.query)
            .hint_text("Songs, artists, albums…")
            .font(font)
            .margin(vec2(14.0, 10.0))
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(&mut app.search.focus) {
        resp.request_focus();
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        app.submit_search();
    }
    ui.add_space(10.0);

    let s = &app.search;
    if s.loading {
        spinner(ui, &app.theme);
        return None;
    }
    if s.searched && s.results.is_empty() {
        dim(ui, &app.theme, "No results.");
    }
    *list = track_list(ui, &app.images, &app.theme, &s.results, playing, None);
    list.clicked.map(Action::PlaySearch)
}

fn library(app: &mut App, ui: &mut Ui) -> Option<Action> {
    let theme = &app.theme;
    heading(ui, theme, "Library");
    let Some(list) = &app.library else {
        if app.library_loading {
            spinner(ui, theme);
        }
        return None;
    };
    if list.is_empty() {
        dim(ui, theme, "No playlists yet.");
        return None;
    }
    let mut clicked = None;
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), list.len(), |ui, range| {
        for i in range {
            let p = &list[i];
            if playlist_row(ui, &app.images, theme, &p.title, &p.subtitle, p.thumb.as_deref()).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked.map(Action::OpenPlaylist)
}

fn playlist(app: &mut App, ui: &mut Ui, playing: Option<&str>, list: &mut ListResult) -> Option<Action> {
    let theme = &app.theme;
    let mut action = None;
    ui.add_space(4.0);
    if text_button(ui, theme, "‹ Library").clicked() {
        action = Some(Action::Go(View::Library));
    }
    let Some(pl) = &app.playlist else { return action };
    heading(ui, theme, &pl.meta.title);
    ui.horizontal(|ui| {
        if !pl.tracks.is_empty() && pill(ui, theme, "Play").clicked() {
            action = Some(Action::PlayPlaylist(0));
        }
        ui.add_space(8.0);
        let count = match pl.tracks.len() {
            1 => "1 track".to_string(),
            n => format!("{n} tracks"),
        };
        dim(ui, theme, &if pl.loading { format!("{count}, loading…") } else { count });
    });
    ui.add_space(10.0);
    if pl.loading && pl.tracks.is_empty() {
        spinner(ui, theme);
        return action;
    }
    *list = track_list(ui, &app.images, theme, &pl.tracks, playing, None);
    list.clicked.map(Action::PlayPlaylist).or(action)
}

fn queue(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
    let theme = &app.theme;
    heading(ui, theme, "Up next");
    if app.queue.is_empty() {
        dim(ui, theme, "Nothing queued. Play something from Search or your Library.");
        return None;
    }
    let playing = app.now_playing().map(|t| t.video_id.as_str());
    *list = track_list(ui, &app.images, theme, &app.queue, playing, app.pos);
    list.clicked.map(Action::PlayQueue)
}

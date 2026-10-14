//! Sidebar and the main content views. Views draw with shared borrows and
//! return the action the user took; it is applied afterwards.

use egui::{Align, CornerRadius, CursorIcon, FontId, Id, Layout, Rect, RichText, ScrollArea, TextEdit, Ui, pos2, vec2};

use super::app::{App, View, is_special_playlist};
use super::images::Images;
use super::menus::{self, MenuAction, PlaylistChoice, TrackChoice};
use super::widgets::{self, nav_item, pill, playlist_row, text_button, track_row};
use crate::api::models::{Playlist, Track};
use crate::theme::{self, Theme};

const VIEW_FADE: f64 = 0.18;
const NEW_PLAYLIST_FIELD: &str = "new-playlist-title";
/// Distance from the list's edge where dragging a row scrolls the list.
const DRAG_SCROLL_ZONE: f32 = 48.0;

/// Which list a track row belongs to (for right-click actions).
#[derive(Clone, Copy)]
enum Source {
    Search,
    Playlist,
    Queue,
}

/// What a track list reported this frame.
#[derive(Default)]
struct ListResult {
    clicked: Option<usize>,
    hovered: Option<String>,
    /// Row under a held-down mouse button: the click hasn't landed yet.
    pressed: Option<String>,
    /// Rows laid out on screen this frame.
    on_screen: Vec<String>,
    menu: Option<(usize, TrackChoice)>,
    drag_start: Option<usize>,
    /// Where a dragged row would land (insert before this index).
    drop_at: Option<usize>,
}

/// How a track list is drawn.
struct ListSpec<'a> {
    tracks: &'a [Track],
    playing: Option<&'a str>,
    current: Option<usize>,
    /// For "Add to playlist" (None when signed out).
    playlists: Option<&'a [Playlist]>,
    /// Some when rows can be dragged to reorder: the drag in progress.
    reorder: Option<Option<(usize, usize)>>,
}

enum Action {
    Go(View),
    PlaySearch(usize),
    OpenPlaylist(usize),
    PlayPlaylist(usize),
    PlayQueue(usize),
    PlayLibraryPlaylist(usize),
    TrackMenu(Source, usize, TrackChoice),
    AskDelete(usize),
    StartNewPlaylist,
    CreatePlaylist,
    CancelNewPlaylist,
    SignIn,
    Menu(MenuAction),
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
            status(app, ui);
            ui.add_space(8.0);
            // A fixed-height block at the bottom, laid out top-down inside.
            let height = 3.0 * theme.font_size * 2.5 + 2.0 * ui.spacing().item_spacing.y;
            ui.allocate_ui_with_layout(vec2(ui.available_width(), height), Layout::top_down(Align::Min), |ui| {
                if let Some(m) = menus::menus(app, ui, false) {
                    action = Some(Action::Menu(m));
                }
            });
        });
    }
    apply(app, ui, action);
}

/// Notices (status messages that fade after a few seconds).
fn status(app: &App, ui: &mut Ui) {
    if let Some((msg, _)) = &app.notice {
        let theme = &app.theme;
        ui.add(egui::Label::new(RichText::new(msg).font(widgets::small(theme)).color(theme.colors.text_dim)).wrap());
    }
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
            ui.add_space(16.0);
            if let Some(m) = menus::menus(app, ui, true) {
                action = Some(Action::Menu(m));
            }
        });
        status(app, ui);
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
    drag(app, ui, list.drag_start, list.drop_at);

    match menus::delete_dialog(app, ui.ctx()) {
        Some(true) => {
            if let Some(p) = app.confirm_delete.take() {
                app.delete_playlist(p);
            }
        }
        Some(false) => app.confirm_delete = None,
        None => {}
    }
    apply(app, ui, action);
}

/// Drag-to-reorder bookkeeping for the open playlist.
fn drag(app: &mut App, ui: &Ui, start: Option<usize>, drop_at: Option<usize>) {
    if let Some(from) = start {
        app.drag = Some((from, from));
    }
    let Some((from, to)) = app.drag else { return };
    let to = drop_at.unwrap_or(to);
    app.drag = Some((from, to));
    let (released, cancelled) = ui.input(|i| (i.pointer.any_released(), i.key_pressed(egui::Key::Escape)));
    if cancelled {
        app.drag = None;
    } else if released {
        app.drag = None;
        app.move_in_playlist(from, to);
    } else {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
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
                app.open_playlist(ctx, meta, false);
            }
        }
        Some(Action::PlayLibraryPlaylist(i)) => {
            // Opens it and starts playing as soon as its first page arrives.
            if let Some(meta) = app.library.as_ref().and_then(|l| l.get(i)).cloned() {
                app.open_playlist(ctx, meta, true);
            }
        }
        Some(Action::PlayPlaylist(i)) => {
            if let Some(tracks) = app.playlist.as_ref().map(|p| p.tracks.clone()) {
                app.play_list(tracks, i, false);
            }
        }
        Some(Action::PlayQueue(i)) => app.play_queue_index(i),
        Some(Action::TrackMenu(source, i, choice)) => {
            let track = match source {
                Source::Search => app.search.results.get(i),
                Source::Playlist => app.playlist.as_ref().and_then(|p| p.tracks.get(i)),
                Source::Queue => app.queue.get(i),
            }
            .cloned();
            let Some(track) = track else { return };
            match choice {
                TrackChoice::PlayNext => app.play_next(track),
                TrackChoice::AddToQueue => app.add_to_queue(track),
                TrackChoice::AddToPlaylist(p) => {
                    if let Some(playlist) = app.library.as_ref().and_then(|l| l.get(p)) {
                        app.add_to_playlist(playlist, &track);
                    }
                }
            }
        }
        Some(Action::AskDelete(i)) => app.confirm_delete = app.library.as_ref().and_then(|l| l.get(i)).cloned(),
        Some(Action::StartNewPlaylist) => {
            app.new_playlist = Some(String::new());
            ctx.memory_mut(|m| m.request_focus(Id::new(NEW_PLAYLIST_FIELD)));
        }
        Some(Action::CreatePlaylist) => {
            let title = app.new_playlist.take().unwrap_or_default();
            app.create_playlist(&title);
        }
        Some(Action::CancelNewPlaylist) => app.new_playlist = None,
        Some(Action::SignIn) => app.sign_in(),
        Some(Action::Menu(m)) => menus::apply(app, m),
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
fn track_list(ui: &mut Ui, images: &Images, theme: &Theme, spec: ListSpec) -> ListResult {
    let mut out = ListResult::default();
    let dragging = spec.reorder.flatten();
    let pointer = ui.ctx().pointer_latest_pos();
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), spec.tracks.len(), |ui, range| {
        let mut rows: Vec<(usize, Rect)> = Vec::with_capacity(range.len());
        for i in range.clone() {
            let t = &spec.tracks[i];
            let resp = track_row(ui, images, theme, t, spec.playing == Some(t.video_id.as_str()), spec.current == Some(i), spec.reorder.is_some());
            rows.push((i, resp.rect));
            if resp.clicked() {
                out.clicked = Some(i);
            }
            if resp.hovered() {
                out.hovered = Some(t.video_id.clone());
            }
            if resp.is_pointer_button_down_on() {
                out.pressed = Some(t.video_id.clone());
            }
            if resp.drag_started() && spec.reorder.is_some() {
                out.drag_start = Some(i);
            }
            if let Some(choice) = menus::track_menu(&resp, spec.playlists) {
                out.menu = Some((i, choice));
            }
            out.on_screen.push(t.video_id.clone());
        }
        if let (Some((from, _)), Some(pointer)) = (dragging.or(out.drag_start.map(|f| (f, f))), pointer) {
            drop_indicator(ui, theme, &rows, from, pointer.y, range.end, &mut out.drop_at);
        }
    });
    out
}

/// Finds where a dragged row would land, draws the insertion line, dims the
/// row being moved, and scrolls when the pointer nears the list's edge.
fn drop_indicator(ui: &Ui, theme: &Theme, rows: &[(usize, Rect)], from: usize, y: f32, end: usize, drop_at: &mut Option<usize>) {
    let Some(&(_, last)) = rows.last() else { return };
    let target = rows.iter().find(|(_, r)| y < r.center().y).map_or((end, last.bottom()), |&(i, r)| (i, r.top()));
    *drop_at = Some(target.0);
    let p = ui.painter();
    if let Some((_, r)) = rows.iter().find(|(i, _)| *i == from) {
        p.rect_filled(*r, CornerRadius::same(theme.rounding.clamp(0.0, 255.0) as u8), theme.colors.background.gamma_multiply(0.6));
    }
    if target.0 != from && target.0 != from + 1 {
        let line = Rect::from_min_max(pos2(last.left() + 4.0, target.1 - 1.5), pos2(last.right() - 4.0, target.1 + 1.5));
        p.rect_filled(line, CornerRadius::same(2), theme.colors.accent);
    }
    let clip = ui.clip_rect();
    let speed = if y < clip.top() + DRAG_SCROLL_ZONE {
        8.0
    } else if y > clip.bottom() - DRAG_SCROLL_ZONE {
        -8.0
    } else {
        0.0
    };
    if speed != 0.0 {
        ui.scroll_with_delta(vec2(0.0, speed));
        ui.ctx().request_repaint();
    }
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
    let spec = ListSpec { tracks: &s.results, playing, current: None, playlists: app.library.as_deref(), reorder: None };
    *list = track_list(ui, &app.images, &app.theme, spec);
    list.menu.take().map(|(i, c)| Action::TrackMenu(Source::Search, i, c)).or(list.clicked.map(Action::PlaySearch))
}

fn library(app: &mut App, ui: &mut Ui) -> Option<Action> {
    let mut action = None;
    // `heading`'s spacing, applied outside the row (inside it would be horizontal).
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Library").font(FontId::new(app.theme.font_size * 1.7, theme::bold())).color(app.theme.colors.text));
        if app.signed_out {
            return;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &mut app.new_playlist {
            None => {
                if pill(ui, &app.theme, "New playlist").clicked() {
                    action = Some(Action::StartNewPlaylist);
                }
            }
            Some(title) => {
                if ui.button("Cancel").clicked() {
                    action = Some(Action::CancelNewPlaylist);
                }
                if pill(ui, &app.theme, "Create").clicked() {
                    action = Some(Action::CreatePlaylist);
                }
                let edit = ui.add(TextEdit::singleline(title).id(Id::new(NEW_PLAYLIST_FIELD)).hint_text("Playlist name").desired_width(220.0));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = Some(Action::CreatePlaylist);
                }
            }
        });
    });
    ui.add_space(10.0);
    let theme = &app.theme;
    if app.signed_out {
        dim(ui, theme, "Sign in to see your playlists.");
        ui.add_space(8.0);
        if pill(ui, theme, "Sign in").clicked() {
            action = Some(Action::SignIn);
        }
        return action;
    }
    let Some(list) = &app.library else {
        if app.library_loading {
            spinner(ui, theme);
        }
        return action;
    };
    if list.is_empty() {
        dim(ui, theme, "No playlists yet.");
        return action;
    }
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), list.len(), |ui, range| {
        for i in range {
            let p = &list[i];
            let resp = playlist_row(ui, &app.images, theme, &p.title, &p.subtitle, p.thumb.as_deref());
            if resp.clicked() {
                action = Some(Action::OpenPlaylist(i));
            }
            match menus::playlist_menu(&resp, p) {
                Some(PlaylistChoice::Play) => action = Some(Action::PlayLibraryPlaylist(i)),
                Some(PlaylistChoice::Delete) => action = Some(Action::AskDelete(i)),
                None => {}
            }
        }
    });
    action
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
        let mut info = if pl.loading { format!("{count}, loading…") } else { count };
        if pl.editable && !is_special_playlist(&pl.meta.id) {
            info.push_str(" · drag to reorder");
        }
        dim(ui, theme, &info);
    });
    ui.add_space(10.0);
    if pl.loading && pl.tracks.is_empty() {
        spinner(ui, theme);
        return action;
    }
    let reorder = (pl.editable && !is_special_playlist(&pl.meta.id)).then_some(app.drag);
    let spec = ListSpec { tracks: &pl.tracks, playing, current: None, playlists: app.library.as_deref(), reorder };
    *list = track_list(ui, &app.images, theme, spec);
    list.menu
        .take()
        .map(|(i, c)| Action::TrackMenu(Source::Playlist, i, c))
        .or(list.clicked.filter(|_| app.drag.is_none()).map(Action::PlayPlaylist))
        .or(action)
}

fn queue(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
    let theme = &app.theme;
    heading(ui, theme, "Up next");
    if app.queue.is_empty() {
        dim(ui, theme, "Nothing queued. Play something from Search or your Library.");
        return None;
    }
    let playing = app.now_playing().map(|t| t.video_id.as_str());
    let spec = ListSpec { tracks: &app.queue, playing, current: app.pos, playlists: app.library.as_deref(), reorder: None };
    *list = track_list(ui, &app.images, theme, spec);
    list.menu.take().map(|(i, c)| Action::TrackMenu(Source::Queue, i, c)).or(list.clicked.map(Action::PlayQueue))
}

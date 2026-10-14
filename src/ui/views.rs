//! Sidebar and the main content views. Views draw with shared borrows and
//! return the action the user took; it is applied afterwards.

use std::ops::Range;

use egui::{Align, CornerRadius, CursorIcon, FontId, Id, Layout, Rect, RichText, ScrollArea, TextEdit, Ui, pos2, vec2};

use super::app::{App, View};
use super::menus::{self, Download, MenuAction, PlaylistChoice, TrackChoice};
use super::widgets::{self, ListLook, RowLook, card, ghost_pill, nav_item, pill, playlist_row, text_button, track_row};
use crate::api::models::{Kind, Playlist, Track};
use crate::api::parse;
use crate::theme::{self, Theme};

/// Entrance animation of a view: fade in while sliding up this far.
const VIEW_ENTER: f64 = 0.22;
const VIEW_SLIDE: f32 = 14.0;
const NEW_PLAYLIST_FIELD: &str = "new-playlist-title";
/// Distance from the list's edge where dragging a row scrolls the list.
const DRAG_SCROLL_ZONE: f32 = 48.0;
/// Album tiles (search, artist pages) and page header art.
const CARD: f32 = 132.0;
const HEADER_ART: f32 = 128.0;

/// One line of a track list.
enum Entry<'a> {
    Track(&'a Track),
    /// Section title (the queue's "Next in queue", ...).
    Header(&'a str),
}

/// What a track list reported this frame. Indexes are list rows.
#[derive(Default)]
struct ListResult {
    clicked: Option<usize>,
    hovered: Option<String>,
    /// Row under a held-down mouse button: the click hasn't landed yet.
    pressed: Option<String>,
    /// Rows laid out on screen this frame.
    on_screen: Vec<String>,
    menu: Option<(usize, TrackChoice)>,
    like: Option<usize>,
    album: Option<usize>,
    artist: Option<usize>,
    drag_start: Option<usize>,
    /// Where a dragged row would land (insert before this index).
    drop_at: Option<usize>,
}

/// How a track list is drawn.
struct ListSpec<'a> {
    len: usize,
    entry: &'a dyn Fn(usize) -> Entry<'a>,
    playing: Option<&'a str>,
    /// Row drawn as selected (the queue's current track).
    active: Option<usize>,
    /// Some when rows can be dragged to reorder: the drag in progress.
    reorder: Option<Option<(usize, usize)>>,
}

/// A row of the queue view.
#[derive(Clone, Copy)]
enum QueueRow {
    Header,
    Current,
    Manual(usize),
    Context(usize),
}

enum Action {
    Go(View),
    Back,
    PlaySearch(usize),
    /// Play the open playlist/album from a row, or shuffled.
    PlayPage(usize, bool),
    ToggleSaved,
    Open(Playlist),
    /// Open and start playing as soon as it loads.
    PlayCollection(Playlist),
    /// Download a playlist/album, or remove its download.
    ToggleDownload(Playlist),
    /// The Download/Downloaded button of the open playlist/album.
    TogglePageDownload,
    OpenArtist(String),
    OpenAlbum(Track),
    PlayQueue(QueueRow),
    PlayArtistSong(usize),
    TrackMenu(Track, TrackChoice),
    Like(Track),
    AskDelete(Playlist),
    Remove(Playlist),
    LibraryTab(Kind),
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
            let selected = app.section() == view;
            if nav_item(ui, theme, view.label(), selected).clicked() {
                action = Some(Action::Go(view));
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            status(app, ui);
            ui.add_space(8.0);
            // A fixed-height block at the bottom, laid out top-down inside.
            let items = menus::MENU_COUNT as f32;
            let height = items * theme.font_size * 2.5 + (items - 1.0) * ui.spacing().item_spacing.y;
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
    // Entrance: the view fades in while gliding up into place.
    let now = ui.input(|i| i.time);
    let t = ((now - app.view_since) / VIEW_ENTER).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    let e = egui::emath::easing::cubic_out(t);
    ui.multiply_opacity(e);
    ui.add_space((1.0 - e) * VIEW_SLIDE);

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

    let mut list = ListResult::default();
    action = match app.view {
        View::Search => search(app, ui, &mut list),
        View::Library => library(app, ui),
        View::Playlist => playlist(app, ui, &mut list),
        View::Artist => artist(app, ui, &mut list),
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
    let Some(action) = action else { return };
    match action {
        Action::Go(view) => app.go(ctx, view),
        Action::Back => app.back(ctx),
        Action::PlaySearch(i) => {
            if let Some(t) = app.search.results.get(i).cloned() {
                app.play_list(vec![t], 0, true, String::new(), None);
            }
        }
        Action::PlayPage(i, shuffle) => app.play_page(i, shuffle),
        Action::ToggleSaved => app.toggle_saved(),
        Action::Open(list) => app.open_playlist(ctx, list, false),
        Action::PlayCollection(list) => app.open_playlist(ctx, list, true),
        Action::ToggleDownload(list) => app.toggle_download(list),
        Action::TogglePageDownload => app.toggle_page_download(),
        Action::OpenArtist(id) => app.open_artist(ctx, &id),
        Action::OpenAlbum(track) => app.open_album(ctx, &track),
        Action::PlayQueue(row) => match row {
            QueueRow::Manual(i) => app.skip_to_manual(i),
            QueueRow::Context(i) => app.skip_to_context(i),
            QueueRow::Current | QueueRow::Header => {}
        },
        Action::PlayArtistSong(i) => {
            if let Some(a) = app.artist.as_ref().and_then(|a| a.artist.as_ref()) {
                let (songs, name) = (a.songs.clone(), a.name.clone());
                app.play_list(songs, i, true, name, None);
            }
        }
        Action::TrackMenu(track, choice) => match choice {
            TrackChoice::PlayNext => app.play_next(track),
            TrackChoice::AddToQueue => app.add_to_queue(track),
            TrackChoice::AddToPlaylist(p) => {
                if let Some(playlist) = app.library.as_ref().and_then(|l| l.get(p)) {
                    app.add_to_playlist(playlist, &track);
                }
            }
            TrackChoice::Download => app.download_song(track),
        },
        Action::Like(track) => app.toggle_like(&track),
        Action::AskDelete(list) => app.confirm_delete = Some(list),
        Action::Remove(list) => {
            if let Some(id) = list.save_id.clone() {
                app.set_saved(list, id, false);
            }
        }
        Action::LibraryTab(kind) => {
            app.library_tab = kind;
            app.ensure_library();
        }
        Action::StartNewPlaylist => {
            app.new_playlist = Some(String::new());
            ctx.memory_mut(|m| m.request_focus(Id::new(NEW_PLAYLIST_FIELD)));
        }
        Action::CreatePlaylist => {
            let title = app.new_playlist.take().unwrap_or_default();
            app.create_playlist(&title);
        }
        Action::CancelNewPlaylist => app.new_playlist = None,
        Action::SignIn => app.sign_in(),
        Action::Menu(m) => menus::apply(app, m),
    }
}

fn heading(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.add_space(6.0);
    ui.label(heading_text(theme, text));
    ui.add_space(10.0);
}

fn heading_text(theme: &Theme, text: &str) -> RichText {
    RichText::new(text).font(FontId::new(theme.font_size * 1.7, theme::bold())).color(theme.colors.text)
}

/// Smaller section title inside a view ("Albums", "Top songs").
fn section(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).font(FontId::new(theme.font_size * 1.15, theme::bold())).color(theme.colors.text));
    ui.add_space(4.0);
}

fn dim(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(RichText::new(text).color(theme.colors.text_dim));
}

fn spinner(ui: &mut Ui, theme: &Theme) {
    ui.add_space(12.0);
    ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(22.0).color(theme.colors.accent)));
}

fn back_button(ui: &mut Ui, theme: &Theme) -> Option<Action> {
    ui.add_space(4.0);
    text_button(ui, theme, "‹ Back").clicked().then_some(Action::Back)
}

/// Lays out rows `range` of a list directly (no scrolling of its own).
fn rows(ui: &mut Ui, app: &App, spec: &ListSpec, range: Range<usize>, out: &mut ListResult) -> Vec<(usize, Rect)> {
    let theme = &app.theme;
    // "Add to playlist" offers the user's own playlists.
    let playlists = (!app.signed_out).then_some(app.library.as_deref().unwrap_or_default());
    let mut rects = Vec::with_capacity(range.len());
    for i in range {
        let t = match (spec.entry)(i) {
            Entry::Track(t) => t,
            Entry::Header(text) => {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), widgets::row_height(theme)), egui::Sense::hover());
                let g = widgets::line(ui, text, widgets::title_font(theme), theme.colors.text_dim, rect.width());
                ui.painter().galley(pos2(rect.left() + 8.0, rect.bottom() - g.size().y - 8.0), g, theme.colors.text_dim);
                continue;
            }
        };
        let download = app.song_download(&t.video_id);
        let look = RowLook {
            playing: spec.playing == Some(t.video_id.as_str()),
            active: spec.active == Some(i),
            draggable: spec.reorder.is_some(),
            can_like: app.can_like(),
            liked: app.is_liked(t),
            downloaded: download == Download::Done,
        };
        let (resp, clicks) = track_row(ui, &app.images, theme, t, look);
        rects.push((i, resp.rect));
        if clicks.like {
            out.like = Some(i);
        } else if clicks.album {
            out.album = Some(i);
        } else if clicks.artist {
            out.artist = Some(i);
        } else if resp.clicked() {
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
        if let Some(choice) = menus::track_menu(&resp, playlists, download) {
            out.menu = Some((i, choice));
        }
        out.on_screen.push(t.video_id.clone());
    }
    rects
}

/// Virtualized list: only rows inside the viewport are laid out and painted.
fn track_list(ui: &mut Ui, app: &App, spec: ListSpec) -> ListResult {
    let mut out = ListResult::default();
    let dragging = spec.reorder.flatten();
    let pointer = ui.ctx().pointer_latest_pos();
    let theme = &app.theme;
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), spec.len, |ui, range| {
        let end = range.end;
        let rects = rows(ui, app, &spec, range, &mut out);
        if let (Some((from, _)), Some(pointer)) = (dragging.or(out.drag_start.map(|f| (f, f))), pointer) {
            drop_indicator(ui, theme, &rects, from, pointer.y, end, &mut out.drop_at);
        }
    });
    out
}

/// Turns what a list reported into an action, given how its rows map to tracks.
fn row_action<'a>(list: &mut ListResult, track: impl Fn(usize) -> Option<&'a Track>) -> Option<Action> {
    let cloned = |i: Option<usize>| i.and_then(&track).cloned();
    if let Some((i, choice)) = list.menu.take() {
        return cloned(Some(i)).map(|t| Action::TrackMenu(t, choice));
    }
    if let Some(t) = cloned(list.like) {
        return Some(Action::Like(t));
    }
    if let Some(t) = cloned(list.album) {
        return Some(Action::OpenAlbum(t));
    }
    cloned(list.artist).and_then(|t| t.artist_id).map(Action::OpenArtist)
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

fn list_look(p: &Playlist, download: Download) -> ListLook<'_> {
    ListLook { title: &p.title, subtitle: &p.subtitle, thumb: p.thumb.as_deref(), downloaded: download == Download::Done }
}

/// A horizontal strip (or wrapped grid) of album/playlist tiles.
fn cards(ui: &mut Ui, app: &App, items: &[Playlist], wrap: bool) -> Option<Action> {
    let mut action = None;
    let mut draw = |ui: &mut Ui| {
        for item in items {
            let download = app.list_download(&item.id);
            let look = list_look(item, download);
            let resp = card(ui, &app.images, &app.theme, look, CARD);
            if resp.clicked() {
                action = Some(Action::Open(item.clone()));
            }
            match menus::playlist_menu(&resp, item, app.signed_out, download) {
                Some(PlaylistChoice::Play) => action = Some(Action::PlayCollection(item.clone())),
                Some(PlaylistChoice::Download) => action = Some(Action::ToggleDownload(item.clone())),
                _ => {}
            }
        }
    };
    if wrap {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
            draw(ui);
        });
    } else {
        ScrollArea::horizontal().id_salt(ui.next_auto_id()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                draw(ui);
            });
        });
    }
    action
}

fn search(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
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

    let app = &*app;
    let s = &app.search;
    let theme = &app.theme;
    if s.loading {
        spinner(ui, theme);
        return None;
    }
    if s.searched && s.results.is_empty() {
        dim(ui, theme, "No results.");
    }
    let mut action = None;
    if !s.albums.is_empty() {
        section(ui, theme, "Albums");
        action = cards(ui, app, &s.albums, false);
        section(ui, theme, "Songs");
    }
    let playing = app.now_playing().map(|t| t.video_id.as_str());
    let entry = |i: usize| Entry::Track(&s.results[i]);
    *list = track_list(ui, app, ListSpec { len: s.results.len(), entry: &entry, playing, active: None, reorder: None });
    row_action(list, |i| s.results.get(i)).or(list.clicked.map(Action::PlaySearch)).or(action)
}

fn library(app: &mut App, ui: &mut Ui) -> Option<Action> {
    let mut action = None;
    // `heading`'s spacing, applied outside the row (inside it would be horizontal).
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(heading_text(&app.theme, "Library"));
        if app.signed_out {
            return;
        }
        ui.add_space(12.0);
        for (kind, label) in [(Kind::Playlist, "Playlists"), (Kind::Album, "Albums")] {
            if ui.add(egui::Button::selectable(app.library_tab == kind, label)).clicked() && app.library_tab != kind {
                action = Some(Action::LibraryTab(kind));
            }
        }
        if app.library_tab != Kind::Playlist {
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
        dim(ui, theme, "Sign in to see your playlists and albums.");
        ui.add_space(8.0);
        if pill(ui, theme, "Sign in").clicked() {
            action = Some(Action::SignIn);
        }
        return action;
    }
    let (items, loading, empty) = match app.library_tab {
        Kind::Playlist => (&app.library, app.library_loading, "No playlists yet."),
        Kind::Album => (&app.albums, app.albums_loading, "No saved albums yet. Open an album and press Save."),
    };
    let Some(items) = items else {
        if loading {
            spinner(ui, theme);
        }
        return action;
    };
    if items.is_empty() {
        dim(ui, theme, empty);
        return action;
    }
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, widgets::row_height(theme), items.len(), |ui, range| {
        for p in &items[range] {
            let download = app.list_download(&p.id);
            let resp = playlist_row(ui, &app.images, theme, list_look(p, download));
            if resp.clicked() {
                action = Some(Action::Open(p.clone()));
            }
            match menus::playlist_menu(&resp, p, app.signed_out, download) {
                Some(PlaylistChoice::Play) => action = Some(Action::PlayCollection(p.clone())),
                Some(PlaylistChoice::Download) => action = Some(Action::ToggleDownload(p.clone())),
                Some(PlaylistChoice::Delete) => action = Some(Action::AskDelete(p.clone())),
                Some(PlaylistChoice::Remove) => action = Some(Action::Remove(p.clone())),
                None => {}
            }
        }
    });
    action
}

fn playlist(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
    let app = &*app;
    let theme = &app.theme;
    let mut action = back_button(ui, theme);
    let Some(pl) = &app.playlist else { return action };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(HEADER_ART, HEADER_ART), egui::Sense::hover());
        widgets::art(ui, &app.images, rect, pl.header.thumb.as_deref().or(pl.meta.thumb.as_deref()), theme);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            let kind = if pl.meta.kind == Kind::Album { "ALBUM" } else { "PLAYLIST" };
            ui.label(RichText::new(kind).font(widgets::small(theme)).color(theme.colors.text_dim));
            ui.add(egui::Label::new(heading_text(theme, &pl.meta.title)).truncate());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if let Some((name, id)) = &pl.header.artist {
                    let g = widgets::line(ui, name, widgets::title_font(theme), theme.colors.text, ui.available_width() * 0.6);
                    let (rect, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
                    let link = widgets::text_link(ui, Id::new("page-artist"), rect.min, g, theme.colors.text, id.is_some());
                    if let (Some(id), true) = (id, link.is_some_and(|r| r.clicked())) {
                        action = Some(Action::OpenArtist(id.clone()));
                    }
                    if !pl.header.subtitle.is_empty() {
                        dim(ui, theme, "  •  ");
                    }
                }
                let mut info = pl.header.subtitle.clone();
                if info.is_empty() {
                    info = match pl.tracks.len() {
                        1 => "1 track".into(),
                        n => format!("{n} tracks"),
                    };
                }
                // Albums: total plays of all their songs.
                if pl.meta.kind == Kind::Album {
                    let total: u64 = pl.tracks.iter().filter_map(|t| t.plays.as_deref().and_then(parse::play_count)).sum();
                    if total > 0 {
                        info.push_str(&format!(" • {} plays", parse::short_count(total)));
                    }
                }
                if pl.loading && !pl.tracks.is_empty() {
                    info.push_str(", loading…");
                }
                ui.add(egui::Label::new(RichText::new(info).color(theme.colors.text_dim)).truncate());
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if !pl.tracks.is_empty() {
                    if pill(ui, theme, "Play").clicked() {
                        action = Some(Action::PlayPage(0, false));
                    }
                    if ghost_pill(ui, theme, "Shuffle").clicked() {
                        action = Some(Action::PlayPage(0, true));
                    }
                }
                if let Some((_, saved)) = pl.header.save.as_ref().filter(|_| !app.signed_out) {
                    let label = if *saved { "Saved" } else { "Save" };
                    if ghost_pill(ui, theme, label).on_hover_text(if *saved { "Remove from library" } else { "Save to library" }).clicked() {
                        action = Some(Action::ToggleSaved);
                    }
                }
                if !pl.tracks.is_empty() {
                    let (label, tip) = match app.downloads.progress(&pl.meta.id) {
                        Some((done, total)) if total > 0 => (format!("Downloading {done}/{total}"), None),
                        Some(_) => ("Downloading\u{2026}".to_string(), None),
                        None if app.downloads.has_list(&pl.meta.id) => ("Downloaded".to_string(), Some("Remove the download")),
                        None => ("Download".to_string(), Some("Save every song to the downloads folder")),
                    };
                    let resp = ghost_pill(ui, theme, &label);
                    if let Some(tip) = tip
                        && resp.on_hover_text(tip).clicked()
                    {
                        action = Some(Action::TogglePageDownload);
                    }
                }
                if pl.reorderable() {
                    ui.add_space(6.0);
                    dim(ui, theme, "Drag to reorder");
                }
            });
        });
    });
    ui.add_space(12.0);
    if pl.loading && pl.tracks.is_empty() {
        spinner(ui, theme);
        return action;
    }
    let playing = app.now_playing().map(|t| t.video_id.as_str());
    let entry = |i: usize| Entry::Track(&pl.tracks[i]);
    let reorder = pl.reorderable().then_some(app.drag);
    *list = track_list(ui, app, ListSpec { len: pl.tracks.len(), entry: &entry, playing, active: None, reorder });
    row_action(list, |i| pl.tracks.get(i))
        .or(list.clicked.filter(|_| app.drag.is_none()).map(|i| Action::PlayPage(i, false)))
        .or(action)
}

fn artist(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
    let app = &*app;
    let theme = &app.theme;
    let mut action = back_button(ui, theme);
    let Some(state) = &app.artist else { return action };
    let Some(a) = &state.artist else {
        spinner(ui, theme);
        return action;
    };
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(HEADER_ART, HEADER_ART), egui::Sense::hover());
            widgets::art(ui, &app.images, rect, a.thumb.as_deref(), theme);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(HEADER_ART / 2.0 - theme.font_size * 2.2);
                ui.label(RichText::new("ARTIST").font(widgets::small(theme)).color(theme.colors.text_dim));
                ui.add(egui::Label::new(heading_text(theme, &a.name)).truncate());
                dim(ui, theme, &a.subtitle);
            });
        });
        if !a.songs.is_empty() {
            section(ui, theme, "Top songs");
            let playing = app.now_playing().map(|t| t.video_id.as_str());
            let entry = |i: usize| Entry::Track(&a.songs[i]);
            let spec = ListSpec { len: a.songs.len(), entry: &entry, playing, active: None, reorder: None };
            rows(ui, app, &spec, 0..a.songs.len(), list);
            action = row_action(list, |i| a.songs.get(i)).or(list.clicked.map(Action::PlayArtistSong)).or(action.take());
        }
        for (title, items) in [("Albums", &a.albums), ("Singles & EPs", &a.singles)] {
            if !items.is_empty() {
                section(ui, theme, title);
                action = cards(ui, app, items, true).or(action.take());
            }
        }
        ui.add_space(12.0);
    });
    action
}

fn queue(app: &mut App, ui: &mut Ui, list: &mut ListResult) -> Option<Action> {
    let app = &*app;
    let theme = &app.theme;
    heading(ui, theme, "Queue");
    let q = &app.queue;
    let Some(current) = &q.current else {
        dim(ui, theme, "Nothing playing. Play something from Search or your Library.");
        return None;
    };
    // Rows: "Now playing", the track, then "Next in queue" + the user's
    // queue, then "Next from …" + the rest of the album/playlist/radio.
    let rest = q.context_rest();
    let manual_start = 2;
    let context_start = manual_start + if q.manual.is_empty() { 0 } else { q.manual.len() + 1 };
    let len = context_start + if rest.is_empty() { 0 } else { rest.len() + 1 };
    let row = |i: usize| -> QueueRow {
        match i {
            1 => QueueRow::Current,
            i if i > manual_start && i < context_start => QueueRow::Manual(i - manual_start - 1),
            i if i > context_start => QueueRow::Context(i - context_start - 1),
            _ => QueueRow::Header,
        }
    };
    let context_title = if q.context_name.is_empty() { "Next up".to_string() } else { format!("Next from: {}", q.context_name) };
    let entry = |i: usize| match row(i) {
        QueueRow::Current => Entry::Track(current),
        QueueRow::Manual(m) => Entry::Track(&q.manual[m]),
        QueueRow::Context(c) => Entry::Track(&rest[c]),
        QueueRow::Header if i == 0 => Entry::Header("Now playing"),
        QueueRow::Header if i == manual_start => Entry::Header("Next in queue"),
        QueueRow::Header => Entry::Header(&context_title),
    };
    let track = |i: usize| match row(i) {
        QueueRow::Current => Some(current),
        QueueRow::Manual(m) => q.manual.get(m),
        QueueRow::Context(c) => rest.get(c),
        QueueRow::Header => None,
    };
    let playing = Some(current.video_id.as_str());
    *list = track_list(ui, app, ListSpec { len, entry: &entry, playing, active: Some(1), reorder: None });
    row_action(list, track).or(list.clicked.map(|i| Action::PlayQueue(row(i))))
}

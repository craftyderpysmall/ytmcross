//! Application state and the glue between UI, worker and player.
//!
//! `logic` runs whenever something woke us (input, a worker message, an
//! animation) and drains messages, even while the window is hidden, so the
//! queue keeps advancing. `ui` lays out the frame; the shell (shell.rs) only
//! calls it while the window is visible and something changed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{CentralPanel, FontData, Frame, Margin, Panel, Ui};

use super::images::Images;
use super::menus::Download;
use super::{bar, views};
use crate::api::client;
use crate::api::models::{Artist, Header, Kind, Playlist, Track};
use crate::auth;
use crate::config::{BarPosition, Config, EqualizerConfig};
use crate::download::Downloads;
use crate::{paths, proc};
use crate::player::{Player, PlayerEvent};
use crate::pool::Pool;
use crate::stream::{Hint, Resolver};
use crate::trace;
use crate::theme::{self, Theme};
use crate::worker::{self, Notifier, Request, Response};

/// Resting the mouse on a row this long makes it a likely next play.
const HOVER_PREFETCH: Duration = Duration::from_millis(120);
/// Rows that stay on screen this long are resolved in the background.
const VISIBLE_SETTLE: Duration = Duration::from_millis(300);
/// At most this many on-screen rows are resolved per settled view.
const MAX_VISIBLE: usize = 12;
/// Queue tracks resolved ahead of the current one.
const LOOKAHEAD: usize = 3;
/// Pages remembered for Back.
const MAX_HISTORY: usize = 16;
/// Like Windows' caret timeout: a text cursor stops blinking (and waking
/// the app twice a second) after this long without input.
const CARET_TIMEOUT: f64 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Search,
    Library,
    Queue,
    /// A playlist or album page.
    Playlist,
    Artist,
}

impl View {
    pub fn from_name(name: &str) -> Option<View> {
        match name {
            "search" => Some(View::Search),
            "library" => Some(View::Library),
            "queue" => Some(View::Queue),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Search => "Search",
            View::Library => "Library",
            View::Queue => "Queue",
            View::Playlist => "Playlist",
            View::Artist => "Artist",
        }
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<Track>,
    pub albums: Vec<Playlist>,
    pub loading: bool,
    pub searched: bool,
    pub focus: bool,
}

pub struct PlaylistState {
    pub meta: Playlist,
    pub header: Header,
    pub tracks: Vec<Track>,
    pub loading: bool,
    /// Start playing once the first page arrives ("Play" from a menu).
    autoplay: bool,
}

impl PlaylistState {
    /// Rows can be dragged to reorder (the user's own, real playlist).
    pub fn reorderable(&self) -> bool {
        self.header.editable && !is_special_playlist(&self.meta.id)
    }
}

pub struct ArtistState {
    pub id: String,
    /// None while loading.
    pub artist: Option<Box<Artist>>,
}

/// A page to return to with Back. Pages keep their content, so going back
/// costs no request.
enum Page {
    View(View),
    Playlist(Box<PlaylistState>),
    Artist(ArtistState),
}

/// Playlists that can't be edited or deleted: Liked Music, Episodes for Later.
pub fn is_special_playlist(id: &str) -> bool {
    matches!(id, "LM" | "SE")
}

/// What plays and in which order, like Spotify: the current track, then
/// the songs the user queued, then the rest of the album/playlist/radio it
/// was started from.
#[derive(Default)]
pub struct Queue {
    pub current: Option<Track>,
    /// The current track came from `manual`.
    from_manual: bool,
    /// Queued by the user; plays before the rest of the context.
    pub manual: Vec<Track>,
    /// The list playback was started from, and where in it we are.
    pub context: Vec<Track>,
    pub ctx_pos: Option<usize>,
    pub context_name: String,
    /// Playlist/album id of the context, so its later pages join it.
    context_id: Option<String>,
}

impl Queue {
    /// Context tracks still to come.
    pub fn context_rest(&self) -> &[Track] {
        let from = self.ctx_pos.map_or(0, |p| p + 1).min(self.context.len());
        &self.context[from..]
    }

    fn upcoming(&self) -> impl Iterator<Item = &Track> {
        self.manual.iter().chain(self.context_rest())
    }
}

pub struct Playback {
    /// The current track is resolving/buffering.
    pub loading: bool,
    pub paused: bool,
    /// Last known position and when we learned it; the clock is extrapolated.
    base: f64,
    at: Instant,
    pub duration: f64,
    pub volume: f32,
}

impl Playback {
    pub fn position(&self) -> f64 {
        let pos = if self.loading || self.paused { self.base } else { self.base + self.at.elapsed().as_secs_f64() };
        if self.duration > 0.0 { pos.min(self.duration) } else { pos }
    }

    fn set_position(&mut self, pos: f64) {
        self.base = pos;
        self.at = Instant::now();
    }
}

pub struct App {
    pub config: Config,
    pub theme: Theme,
    font_file: String,
    /// The theme's own font, kept to rebuild the font set with fallbacks.
    custom_font: Option<Arc<FontData>>,
    /// System fonts for scripts Inter lacks are installed (done on first need).
    fallback_fonts: bool,
    requests: Pool<Request>,
    responses: Receiver<Response>,
    notifier: Notifier,
    player: Player,
    resolver: Resolver,
    pub downloads: Downloads,
    /// Track we're waiting on a stream URL for, and where to start it.
    pending_play: Option<(String, f64)>,
    /// The track restored at startup isn't loaded yet: play starts it here.
    resume_at: Option<f64>,
    /// Row under the mouse, since when, and whether it was prefetched.
    hover: Option<(String, Instant, bool)>,
    /// Row currently pressed (prefetched immediately, before the click lands).
    pressed: Option<String>,
    /// Rows on screen, since when, and whether they were handed to the resolver.
    visible: (Vec<String>, Instant, bool),
    pub images: Images,

    pub view: View,
    /// egui time the current view appeared, for its entrance animation.
    pub view_since: f64,
    /// egui time of the last input event.
    last_input: f64,
    history: Vec<Page>,
    pub search: SearchState,
    /// Saved playlists and saved albums (None until loaded).
    pub library: Option<Vec<Playlist>>,
    pub library_loading: bool,
    pub albums: Option<Vec<Playlist>>,
    pub albums_loading: bool,
    /// Which half of the library is shown.
    pub library_tab: Kind,
    pub playlist: Option<PlaylistState>,
    pub artist: Option<ArtistState>,

    pub queue: Queue,
    radio_pending: bool,
    radio_seed: Option<String>,
    advance_on_radio: bool,
    pub playback: Playback,
    /// Likes changed in this session (they override what responses said).
    likes: HashMap<String, bool>,

    pub notice: Option<(String, Instant)>,
    pub signed_out: bool,
    pub signing_in: bool,

    /// Theme names for the Theme menu.
    pub themes: Vec<String>,
    /// Live equalizer settings (saved to config.toml on release).
    pub eq: EqualizerConfig,
    /// Live pitch shift in semitones.
    pub pitch: i32,
    /// Playlist row being dragged: (from index, current drop index).
    pub drag: Option<(usize, usize)>,
    /// Title being typed for a new playlist.
    pub new_playlist: Option<String>,
    /// Playlist waiting for delete confirmation.
    pub confirm_delete: Option<Playlist>,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        wake: worker::Waker,
        config: Config,
        loaded: theme::Loaded,
        config_error: Option<String>,
        signed_in: bool,
    ) -> App {
        // The track restored from the last session may need the fallback fonts.
        let last = load_last_played();
        let fallback_fonts = last.as_ref().is_some_and(|(t, _)| track_needs_fallback(t));
        let custom_font = loaded.font.map(|bytes| Arc::new(FontData::from_owned(bytes)));
        theme::install_fonts(ctx, custom_font.clone(), fallback_fonts);
        loaded.theme.apply(ctx);

        let agent = client::agent();
        let (requests, responses, notifier) = worker::spawn(wake, agent.clone());
        let images = Images::new(ctx, agent.clone());

        let notice = config_error.or(loaded.error);
        let player_notifier = notifier.clone();
        // Shares the agent (and its kept-alive connection to YouTube Music) with the API worker.
        let resolver = Resolver::new(notifier.clone(), agent.clone());
        let downloads =
            Downloads::new(paths::downloads_dir(&config.downloads.folder), resolver.clone(), agent.clone(), notifier.clone());
        let player = Player::new(&config.player, &config.equalizer, config.pitch.semitones, agent, move |e| {
            player_notifier.send(Response::Player(e))
        });

        let view = config.layout.sidebar_items.iter().find_map(|n| View::from_name(n)).unwrap_or(View::Library);
        if signed_in {
            requests.push(Request::Library { albums: false });
        }
        requests.push(Request::ListThemes);

        // The track playing at the last exit comes back paused where it was.
        let mut queue = Queue::default();
        let mut resume_at = None;
        let mut playback = Playback {
            loading: false,
            paused: false,
            base: 0.0,
            at: Instant::now(),
            duration: 0.0,
            volume: config.player.volume.min(100) as f32 / 100.0,
        };
        if let Some((track, at)) = last {
            playback.paused = true;
            playback.duration = track.seconds();
            playback.set_position(at);
            queue.context = vec![track.clone()];
            queue.ctx_pos = Some(0);
            queue.current = Some(track);
            resume_at = Some(at);
        }

        App {
            font_file: loaded.theme.font_file.clone(),
            custom_font,
            fallback_fonts,
            eq: config.equalizer.clone(),
            pitch: config.pitch.semitones,
            playback,
            config,
            theme: loaded.theme,
            requests,
            responses,
            notifier,
            player,
            resolver,
            downloads,
            pending_play: None,
            resume_at,
            hover: None,
            pressed: None,
            visible: (Vec::new(), Instant::now(), true),
            images,
            view,
            view_since: 0.0,
            last_input: 0.0,
            history: Vec::new(),
            search: SearchState { focus: view == View::Search, ..Default::default() },
            library: None,
            library_loading: signed_in,
            albums: None,
            albums_loading: false,
            library_tab: Kind::Playlist,
            playlist: None,
            artist: None,
            queue,
            radio_pending: false,
            radio_seed: None,
            advance_on_radio: false,
            likes: HashMap::new(),
            notice: notice.map(|n| (n, Instant::now())),
            signed_out: !signed_in,
            signing_in: false,
            themes: Vec::new(),
            drag: None,
            new_playlist: None,
            confirm_delete: None,
        }
    }

    /// Called once when the window closes: remembers what was playing.
    pub fn on_exit(&self) {
        save_last_played(self.queue.current.as_ref(), self.playback.position());
    }

    // ---- navigation -------------------------------------------------------

    /// Sidebar navigation: starts a fresh history.
    pub fn go(&mut self, ctx: &egui::Context, view: View) {
        self.history.clear();
        self.set_view(ctx, view);
    }

    fn set_view(&mut self, ctx: &egui::Context, view: View) {
        if self.view != view {
            self.view = view;
            self.view_since = ctx.input(|i| i.time);
        }
        if view == View::Search {
            self.search.focus = true;
        }
        if view == View::Library {
            self.ensure_library();
        }
    }

    /// Fetches the shown half of the library if it isn't loaded.
    pub fn ensure_library(&mut self) {
        if self.signed_out {
            return;
        }
        match self.library_tab {
            Kind::Playlist if self.library.is_none() && !self.library_loading => self.refresh_library(false),
            Kind::Album if self.albums.is_none() && !self.albums_loading => self.refresh_library(true),
            _ => {}
        }
    }

    fn refresh_library(&mut self, albums: bool) {
        if albums {
            self.albums_loading = true;
        } else {
            self.library_loading = true;
        }
        self.requests.push(Request::Library { albums });
    }

    /// Moves the current page onto the Back history.
    fn push_history(&mut self) {
        let page = match self.view {
            View::Playlist => self.playlist.take().map(|p| Page::Playlist(Box::new(p))),
            View::Artist => self.artist.take().map(Page::Artist),
            view => Some(Page::View(view)),
        };
        if let Some(page) = page {
            if self.history.len() == MAX_HISTORY {
                self.history.remove(0);
            }
            self.history.push(page);
        }
    }

    pub fn back(&mut self, ctx: &egui::Context) {
        match self.history.pop() {
            Some(Page::Playlist(pl)) => {
                let mut pl = *pl;
                // Left before it finished loading: its remaining pages were dropped.
                if pl.loading {
                    pl.tracks.clear();
                    self.requests.push(Request::Playlist(pl.meta.clone()));
                }
                self.playlist = Some(pl);
                self.drag = None;
                self.set_view(ctx, View::Playlist);
            }
            Some(Page::Artist(a)) => {
                if a.artist.is_none() {
                    self.requests.push(Request::Artist(a.id.clone()));
                }
                self.artist = Some(a);
                self.set_view(ctx, View::Artist);
            }
            Some(Page::View(view)) => self.set_view(ctx, view),
            None => self.set_view(ctx, View::Library),
        }
    }

    /// The sidebar entry a page belongs to: the view it was opened from.
    pub fn section(&self) -> View {
        match (self.view, self.history.first()) {
            (View::Playlist | View::Artist, Some(Page::View(root))) => *root,
            (View::Playlist | View::Artist, _) => View::Library,
            (view, _) => view,
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn open_playlist(&mut self, ctx: &egui::Context, meta: Playlist, autoplay: bool) {
        if self.view == View::Playlist && self.playlist.as_ref().is_some_and(|p| p.meta.id == meta.id) {
            return;
        }
        self.requests.push(Request::Playlist(meta.clone()));
        self.push_history();
        self.playlist = Some(PlaylistState { meta, header: Header::default(), tracks: Vec::new(), loading: true, autoplay });
        self.drag = None;
        self.set_view(ctx, View::Playlist);
    }

    /// The album a track is from, if known.
    pub fn open_album(&mut self, ctx: &egui::Context, track: &Track) {
        let Some(id) = track.album_id.clone() else { return };
        let meta = Playlist {
            id,
            kind: Kind::Album,
            title: track.album.clone().unwrap_or_default(),
            subtitle: track.artists.clone(),
            thumb: track.thumb.clone(),
            owned: false,
            save_id: None,
        };
        self.open_playlist(ctx, meta, false);
    }

    pub fn open_artist(&mut self, ctx: &egui::Context, id: &str) {
        if self.view == View::Artist && self.artist.as_ref().is_some_and(|a| a.id == id) {
            return;
        }
        self.requests.push(Request::Artist(id.to_string()));
        self.push_history();
        self.artist = Some(ArtistState { id: id.to_string(), artist: None });
        self.set_view(ctx, View::Artist);
    }

    fn reload_open_playlist(&mut self) {
        if let Some(pl) = &mut self.playlist {
            pl.tracks.clear();
            pl.loading = true;
            self.drag = None;
            self.requests.push(Request::Playlist(pl.meta.clone()));
        }
    }

    // ---- menus ------------------------------------------------------------

    pub fn sign_out(&mut self) {
        self.requests.push(Request::SignOut);
    }

    pub fn list_themes(&self) {
        self.requests.push(Request::ListThemes);
    }

    pub fn set_theme(&self, name: &str) {
        self.requests.push(Request::SetTheme(name.to_string()));
    }

    /// Applies equalizer settings at once; `save` also writes config.toml
    /// (done when a slider is released, not on every drag step).
    pub fn set_equalizer(&mut self, eq: EqualizerConfig, save: bool) {
        self.player.set_equalizer(&eq);
        if save {
            self.requests.push(Request::SaveEqualizer(eq.clone()));
        }
        self.eq = eq;
    }

    /// Same as the equalizer: heard at once, saved on release.
    pub fn set_pitch(&mut self, semitones: i32, save: bool) {
        self.player.set_pitch(semitones);
        if save {
            self.requests.push(Request::SavePitch(semitones));
        }
        self.pitch = semitones;
    }

    // ---- library edits ----------------------------------------------------

    pub fn create_playlist(&mut self, title: &str) {
        let title = title.trim();
        if !title.is_empty() {
            self.requests.push(Request::CreatePlaylist(title.to_string()));
        }
        self.new_playlist = None;
    }

    /// Adds or removes a playlist/album in the loaded library lists.
    fn library_set(&mut self, list: &Playlist, saved: bool) {
        let target = match list.kind {
            Kind::Album => &mut self.albums,
            Kind::Playlist => &mut self.library,
        };
        let Some(items) = target else { return };
        if saved {
            if !items.iter().any(|p| p.id == list.id) {
                // Newest first, after Liked Music like on YouTube Music.
                let at = items.iter().take_while(|p| is_special_playlist(&p.id)).count();
                items.insert(at, list.clone());
            }
        } else {
            items.retain(|p| p.id != list.id);
        }
    }

    /// Optimistic: it disappears now; a failure brings the library back.
    pub fn delete_playlist(&mut self, playlist: Playlist) {
        self.library_set(&playlist, false);
        if self.playlist.as_ref().is_some_and(|p| p.meta.id == playlist.id) {
            self.playlist = None;
            self.history.clear();
            self.view = View::Library;
        }
        self.requests.push(Request::DeletePlaylist(playlist));
    }

    /// Saves (or removes) a playlist or album someone else made.
    pub fn set_saved(&mut self, list: Playlist, save_id: String, save: bool) {
        self.library_set(&list, save);
        if let Some(pl) = self.playlist.as_mut().filter(|p| p.meta.id == list.id) {
            pl.header.save = Some((save_id.clone(), save));
        }
        self.requests.push(Request::Save { list, save_id, save });
    }

    /// The Save/Remove button on the open page.
    pub fn toggle_saved(&mut self) {
        let Some(pl) = &self.playlist else { return };
        if let Some((save_id, saved)) = pl.header.save.clone() {
            let mut list = pl.meta.clone();
            list.save_id = Some(save_id.clone());
            self.set_saved(list, save_id, !saved);
        }
    }

    // ---- downloads --------------------------------------------------------

    pub fn song_download(&self, video_id: &str) -> Download {
        if self.downloads.has(video_id) {
            Download::Done
        } else if self.downloads.progress(video_id).is_some() {
            Download::Running
        } else {
            Download::No
        }
    }

    pub fn list_download(&self, id: &str) -> Download {
        if self.downloads.progress(id).is_some() {
            Download::Running
        } else if self.downloads.has_list(id) {
            Download::Done
        } else {
            Download::No
        }
    }

    pub fn download_song(&self, track: Track) {
        self.downloads.song(track);
    }

    /// Downloads a playlist or album (with the open page's tracks, if all
    /// loaded), or removes its download.
    pub fn toggle_download(&mut self, list: Playlist) {
        match self.list_download(&list.id) {
            Download::Running => {}
            Download::Done => self.downloads.remove_list(list),
            Download::No => {
                let tracks = self.playlist.as_ref().filter(|p| p.meta.id == list.id && !p.loading).map(|p| p.tracks.clone());
                self.notify(format!("Downloading \u{201c}{}\u{201d}", list.title));
                self.downloads.list(list, tracks);
            }
        }
    }

    pub fn toggle_page_download(&mut self) {
        if let Some(meta) = self.playlist.as_ref().map(|p| p.meta.clone()) {
            self.toggle_download(meta);
        }
    }

    pub fn open_folder(&mut self, dir: &std::path::Path) {
        if let Err(e) = proc::open_folder(dir, &self.config.downloads.file_manager) {
            self.notify(format!("Couldn't open {}: {e}", dir.display()));
        }
    }

    /// From Preferences: empty goes back to the default folder.
    pub fn set_download_folder(&mut self, folder: String) {
        let folder = folder.trim().to_string();
        self.downloads.set_dir(paths::downloads_dir(&folder));
        self.notify(format!("Downloads go to {}", self.downloads.dir().display()));
        self.config.downloads.folder = folder;
        self.requests.push(Request::SaveDownloads(self.config.downloads.clone()));
    }

    /// From Preferences (Linux): the program that opens folders, empty for
    /// the system default.
    pub fn set_file_manager(&mut self, program: String) {
        self.config.downloads.file_manager = program;
        self.requests.push(Request::SaveDownloads(self.config.downloads.clone()));
    }

    pub fn add_to_playlist(&self, playlist: &Playlist, track: &Track) {
        self.requests.push(Request::AddToPlaylist {
            id: playlist.id.clone(),
            title: playlist.title.clone(),
            video_id: track.video_id.clone(),
        });
    }

    /// Finishes a drag in the open playlist: moves locally at once, then
    /// tells YouTube (a failure reloads the real order).
    pub fn move_in_playlist(&mut self, from: usize, to: usize) {
        let Some(pl) = &mut self.playlist else { return };
        if from >= pl.tracks.len() || to == from || to == from + 1 {
            return;
        }
        let track = pl.tracks.remove(from);
        let at = if to > from { to - 1 } else { to }.min(pl.tracks.len());
        let Some(set_video_id) = track.set_video_id.clone() else {
            pl.tracks.insert(from, track);
            return;
        };
        pl.tracks.insert(at, track);
        let before = pl.tracks.get(at + 1).and_then(|t| t.set_video_id.clone());
        self.requests.push(Request::MoveInPlaylist { id: pl.meta.id.clone(), set_video_id, before });
    }

    // ---- likes ------------------------------------------------------------

    pub fn is_liked(&self, track: &Track) -> bool {
        self.likes.get(&track.video_id).copied().or(track.liked).unwrap_or(false)
    }

    pub fn can_like(&self) -> bool {
        !self.signed_out
    }

    /// Optimistic: flips at once; a failure flips it back.
    pub fn toggle_like(&mut self, track: &Track) {
        let like = !self.is_liked(track);
        self.likes.insert(track.video_id.clone(), like);
        self.requests.push(Request::Like { video_id: track.video_id.clone(), like });
    }

    // ---- misc -------------------------------------------------------------

    pub fn submit_search(&mut self) {
        let q = self.search.query.trim().to_string();
        if !q.is_empty() {
            self.search.loading = true;
            self.search.albums.clear();
            self.requests.push(Request::Search(q));
        }
    }

    pub fn reload(&self) {
        self.requests.push(Request::Reload);
    }

    pub fn sign_in(&mut self) {
        if self.signing_in {
            return;
        }
        self.signing_in = true;
        let notifier = self.notifier.clone();
        std::thread::spawn(move || notifier.send(Response::SignInFinished(auth::sign_in_blocking())));
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), Instant::now()));
    }

    // ---- queue and playback ----------------------------------------------

    pub fn now_playing(&self) -> Option<&Track> {
        self.queue.current.as_ref()
    }

    /// Plays `list` from `index`, making it the context (the user's queue is
    /// kept and still plays first). `radio` continues with related tracks.
    pub fn play_list(&mut self, list: Vec<Track>, index: usize, radio: bool, name: String, id: Option<String>) {
        let Some(track) = list.get(index).cloned() else { return };
        let q = &mut self.queue;
        q.context = list;
        q.ctx_pos = Some(index);
        q.context_name = name;
        q.context_id = id;
        self.radio_pending = false;
        self.radio_seed = None;
        self.advance_on_radio = false;
        self.play_track(track, false);
        if radio {
            self.request_radio(false);
        }
    }

    /// Plays a playlist/album page, from `index` or shuffled.
    pub fn play_page(&mut self, index: usize, shuffle: bool) {
        let Some(pl) = &self.playlist else { return };
        let mut tracks = pl.tracks.clone();
        let (name, id) = (pl.meta.title.clone(), pl.meta.id.clone());
        if shuffle {
            shuffle_tracks(&mut tracks);
            self.play_list(tracks, 0, false, name, None);
        } else {
            // Unshuffled, pages still loading join the queue as they arrive.
            self.play_list(tracks, index, false, name, Some(id));
        }
    }

    pub fn add_to_queue(&mut self, track: Track) {
        self.queue.manual.push(track);
        self.after_queue_edit();
    }

    pub fn play_next(&mut self, track: Track) {
        self.queue.manual.insert(0, track);
        self.after_queue_edit();
    }

    fn after_queue_edit(&mut self) {
        if self.queue.current.is_none() {
            self.next();
        } else {
            self.prefetch_ahead();
        }
    }

    /// Plays the `i`th track of the user's queue, skipping the ones before it.
    pub fn skip_to_manual(&mut self, i: usize) {
        if i < self.queue.manual.len() {
            self.queue.manual.drain(..i);
            let track = self.queue.manual.remove(0);
            self.play_track(track, true);
        }
    }

    /// Plays the `i`th upcoming context track (the user's queue stays).
    pub fn skip_to_context(&mut self, i: usize) {
        let pos = self.queue.ctx_pos.map_or(0, |p| p + 1) + i;
        if let Some(track) = self.queue.context.get(pos).cloned() {
            self.queue.ctx_pos = Some(pos);
            self.play_track(track, false);
        }
    }

    fn play_track(&mut self, track: Track, from_manual: bool) {
        self.queue.current = Some(track);
        self.queue.from_manual = from_manual;
        self.start(0.0);
    }

    /// Starts the current track at `at` seconds.
    fn start(&mut self, at: f64) {
        let Some(track) = &self.queue.current else { return };
        self.resume_at = None;
        let duration = track.seconds();
        let id = track.video_id.clone();
        // Downloaded (and still there): no network at all.
        let file = self.downloads.file(&id).filter(|f| f.is_file());
        let ready = if file.is_some() { None } else { self.resolver.ready(&id) };
        trace!("app: play {id} ({})", if file.is_some() { "file" } else if ready.is_some() { "url ready" } else { "resolving" });
        match (file, ready) {
            (Some(file), _) => {
                self.player.load_file(file, at);
                self.player.set_paused(false);
                self.pending_play = None;
            }
            (None, Some(url)) => {
                self.resolver.playing(&url);
                self.player.load(&url, at);
                self.player.set_paused(false);
                self.pending_play = None;
            }
            (None, None) => {
                // Silence the previous track while the new one resolves.
                self.player.stop();
                self.pending_play = Some((id.clone(), at));
                self.resolver.want(&id);
            }
        }
        self.playback.loading = true;
        self.playback.paused = false;
        self.playback.duration = duration;
        self.playback.set_position(at);
        self.prefetch_ahead();
    }

    /// Gets the next tracks ready: the very next one fully (stream resolved
    /// and its start preloaded), the ones after resolved.
    fn prefetch_ahead(&self) {
        for (i, t) in self.queue.upcoming().take(LOOKAHEAD).enumerate() {
            hint(&self.resolver, &self.downloads, &t.video_id, if i == 0 { Hint::Likely } else { Hint::Visible });
        }
    }

    /// Called every frame with the track rows on screen. Once they've been
    /// still for a moment, they're resolved in the background.
    pub fn on_screen(&mut self, ctx: &egui::Context, ids: Vec<String>) {
        if ids != self.visible.0 {
            self.visible = (ids, Instant::now(), false);
            ctx.request_repaint_after(VISIBLE_SETTLE);
        } else if !self.visible.2 && self.visible.1.elapsed() >= VISIBLE_SETTLE {
            self.visible.2 = true;
            for id in self.visible.0.iter().take(MAX_VISIBLE) {
                hint(&self.resolver, &self.downloads, id, Hint::Visible);
            }
        }
    }

    /// A row is being pressed: the click is coming, start right away.
    pub fn pressing(&mut self, video_id: Option<&str>) {
        if let Some(id) = video_id
            && self.pressed.as_deref() != Some(id)
        {
            hint(&self.resolver, &self.downloads, id, Hint::Likely);
        }
        self.pressed = video_id.map(str::to_string);
    }

    /// Called every frame with the track row under the mouse, if any.
    pub fn hovered(&mut self, ctx: &egui::Context, video_id: Option<&str>) {
        let Some(id) = video_id else {
            self.hover = None;
            return;
        };
        match &mut self.hover {
            Some((current, since, fired)) if current == id => {
                if !*fired && since.elapsed() >= HOVER_PREFETCH {
                    *fired = true;
                    hint(&self.resolver, &self.downloads, id, Hint::Likely);
                }
            }
            _ => {
                self.hover = Some((id.to_string(), Instant::now(), false));
                // Wake up once the dwell time has passed, even if the mouse is still.
                ctx.request_repaint_after(HOVER_PREFETCH);
            }
        }
    }

    fn request_radio(&mut self, autoplay: bool) {
        let seed = self.queue.context.last().or(self.queue.current.as_ref());
        if let Some(seed) = seed.map(|t| t.video_id.clone()) {
            self.radio_pending = true;
            self.radio_seed = Some(seed.clone());
            self.requests.push(Request::Radio { video_id: seed, autoplay });
        }
    }

    pub fn next(&mut self) {
        if !self.queue.manual.is_empty() {
            let track = self.queue.manual.remove(0);
            self.play_track(track, true);
            return;
        }
        let next = self.queue.ctx_pos.map_or(0, |p| p + 1);
        if let Some(track) = self.queue.context.get(next).cloned() {
            self.queue.ctx_pos = Some(next);
            self.play_track(track, false);
        } else if self.queue.current.is_some() {
            if self.radio_pending {
                // A prefetch is already on its way: play its first track when it lands.
                self.advance_on_radio = true;
            } else {
                self.request_radio(true);
            }
        }
    }

    pub fn prev(&mut self) {
        if self.queue.current.is_none() {
            return;
        }
        if self.playback.position() >= 3.0 {
            self.seek(0.0);
            return;
        }
        let q = &mut self.queue;
        if q.from_manual {
            // Back to the context track it interrupted; it plays again next.
            if let Some(track) = q.ctx_pos.and_then(|p| q.context.get(p)).cloned() {
                let current = q.current.take().expect("checked above");
                q.manual.insert(0, current);
                self.play_track(track, false);
                return;
            }
        } else if let Some(p) = q.ctx_pos.filter(|&p| p > 0) {
            q.ctx_pos = Some(p - 1);
            let track = q.context[p - 1].clone();
            self.play_track(track, false);
            return;
        }
        self.seek(0.0);
    }

    /// The pointer is on the play button: get a restored track's stream ready.
    /// (Not done at startup, where it would cost memory for nothing if the
    /// user never presses play.)
    pub fn warm_resume(&mut self) {
        if let (Some(_), Some(t)) = (self.resume_at, &self.queue.current) {
            hint(&self.resolver, &self.downloads, &t.video_id, Hint::Likely);
        }
    }

    pub fn toggle_pause(&mut self) {
        if self.queue.current.is_none() {
            return;
        }
        // Restored at startup and not loaded yet: start where it was left.
        if let Some(at) = self.resume_at {
            self.start(at);
            return;
        }
        // Optimistic: flip immediately so the button reacts on click.
        let pos = self.playback.position();
        self.playback.paused = !self.playback.paused;
        self.playback.set_position(pos);
        self.player.set_paused(self.playback.paused);
    }

    pub fn seek(&mut self, seconds: f64) {
        if self.resume_at.is_some() {
            self.resume_at = Some(seconds);
            self.playback.set_position(seconds);
            return;
        }
        self.player.seek(seconds);
        self.playback.set_position(seconds);
    }

    /// Heard at once; `save` also writes config.toml (on release).
    pub fn set_volume(&mut self, volume: f32, save: bool) {
        self.playback.volume = volume;
        let percent = (volume * 100.0).round() as u32;
        self.player.set_volume(percent as f64);
        if save && percent != self.config.player.volume {
            self.config.player.volume = percent;
            self.requests.push(Request::SaveVolume(percent));
        }
    }

    // ---- messages ---------------------------------------------------------

    /// Adds the system fonts for other scripts (once; rebuilds the atlas).
    fn use_fallback_fonts(&mut self, ctx: &egui::Context) {
        if !self.fallback_fonts {
            self.fallback_fonts = true;
            theme::install_fonts(ctx, self.custom_font.clone(), true);
        }
    }

    fn handle(&mut self, ctx: &egui::Context, response: Response) {
        if !self.fallback_fonts && response_needs_fallback(&response) {
            self.use_fallback_fonts(ctx);
        }
        match response {
            Response::Search { query, result } => {
                if query == self.search.query.trim() {
                    self.search.loading = false;
                    self.search.searched = true;
                    match result {
                        Ok(tracks) => {
                            // The top result is the most likely click: get it ready.
                            if let Some(top) = tracks.first() {
                                hint(&self.resolver, &self.downloads, &top.video_id, Hint::Likely);
                            }
                            self.search.results = tracks;
                        }
                        Err(e) => self.notify(format!("Search failed: {e}")),
                    }
                }
            }
            Response::SearchAlbums { query, albums } => {
                if query == self.search.query.trim() {
                    self.search.albums = albums;
                }
            }
            Response::Library { albums, result } => {
                let (list, loading) =
                    if albums { (&mut self.albums, &mut self.albums_loading) } else { (&mut self.library, &mut self.library_loading) };
                *loading = false;
                match result {
                    Ok(items) => *list = Some(items),
                    Err(e) => self.notify(format!("Couldn't load library: {e}")),
                }
            }
            Response::PlaylistPage { id, tracks, done, header } => {
                // An unshuffled play of this page: later pages join the queue.
                if !tracks.is_empty() && self.queue.context_id.as_deref() == Some(id.as_str()) {
                    self.queue.context.extend(tracks.iter().cloned());
                    self.prefetch_ahead();
                }
                let Some(pl) = self.playlist.as_mut().filter(|p| p.meta.id == id) else { return };
                pl.tracks.extend(tracks);
                pl.loading = !done;
                if let Some(h) = header {
                    pl.header = *h;
                    if pl.meta.thumb.is_none() {
                        pl.meta.thumb = pl.header.thumb.clone();
                    }
                }
                if !pl.tracks.is_empty() && std::mem::take(&mut pl.autoplay) {
                    self.play_page(0, false);
                }
            }
            Response::PlaylistFailed { id, error } => {
                if let Some(pl) = self.playlist.as_mut().filter(|p| p.meta.id == id) {
                    pl.loading = false;
                    self.notify(format!("Couldn't load: {error}"));
                }
            }
            Response::Artist { id, result } => {
                let Some(state) = self.artist.as_mut().filter(|a| a.id == id) else { return };
                match result {
                    Ok(artist) => state.artist = Some(artist),
                    Err(e) => self.notify(format!("Couldn't load artist: {e}")),
                }
            }
            Response::Radio { seed, tracks, autoplay } => {
                // Ignore answers for a queue that has since been replaced.
                if self.radio_seed.as_deref() != Some(seed.as_str()) {
                    return;
                }
                self.radio_seed = None;
                self.radio_pending = false;
                let q = &mut self.queue;
                let before = q.context.len();
                for t in tracks {
                    if !q.context.iter().any(|c| c.video_id == t.video_id) {
                        q.context.push(t);
                    }
                }
                if q.context_name.is_empty() || before <= 1 {
                    q.context_name = "Radio".into();
                }
                let advance = autoplay || std::mem::take(&mut self.advance_on_radio);
                if advance && self.queue.context.len() > before {
                    self.next();
                } else {
                    self.prefetch_ahead();
                }
            }
            Response::Reloaded { config, theme: loaded, config_error } => {
                if loaded.theme.font_file != self.font_file {
                    self.font_file = loaded.theme.font_file.clone();
                    self.custom_font = loaded.font.map(|bytes| Arc::new(FontData::from_owned(bytes)));
                    theme::install_fonts(ctx, self.custom_font.clone(), self.fallback_fonts);
                }
                loaded.theme.apply(ctx);
                self.theme = loaded.theme;
                if config.equalizer != self.config.equalizer {
                    self.set_equalizer(config.equalizer.clone(), false);
                }
                if config.pitch != self.config.pitch {
                    self.set_pitch(config.pitch.semitones, false);
                }
                if config.player.volume != self.config.player.volume {
                    self.set_volume(config.player.volume.min(100) as f32 / 100.0, false);
                }
                self.downloads.set_dir(paths::downloads_dir(&config.downloads.folder));
                self.config = *config;
                self.list_themes();
                match config_error.or(loaded.error) {
                    Some(e) => self.notify(e),
                    None => self.notify(format!("Theme: {}", self.config.theme)),
                }
            }
            Response::LoggedOut => self.signed_out = true,
            Response::SignedOut => {
                self.signed_out = true;
                self.library = None;
                self.albums = None;
                self.library_loading = false;
                self.albums_loading = false;
                self.likes.clear();
                if self.view == View::Playlist {
                    self.view = View::Library;
                }
                self.playlist = None;
                self.history.clear();
                self.resolver.reset();
                self.notify("Signed out");
            }
            Response::SignInFinished(ok) => {
                self.signing_in = false;
                if ok {
                    self.signed_out = false;
                    self.requests.push(Request::ReloadSession);
                    // Streams resolved for the previous account (or none) are stale.
                    self.resolver.reset();
                    self.library = None;
                    self.albums = None;
                    self.ensure_library();
                    self.notify("Signed in");
                } else {
                    self.notify("Sign-in was cancelled");
                }
            }
            Response::Themes(names) => self.themes = names,
            Response::Edited { message, refresh_library, refresh_playlist } => {
                self.notify(message);
                if refresh_library {
                    self.library = None;
                    self.albums = None;
                    self.ensure_library();
                }
                if refresh_playlist.is_some() && self.playlist.as_ref().map(|p| &p.meta.id) == refresh_playlist.as_ref() {
                    self.reload_open_playlist();
                }
            }
            Response::Created(playlist) => {
                self.notify(format!("Created \u{201c}{}\u{201d}", playlist.title));
                self.library_set(&playlist, true);
            }
            Response::LikeFailed { video_id, liked, error } => {
                self.likes.insert(video_id, liked);
                self.notify(format!("Couldn't update likes: {error}"));
            }
            Response::Player(event) => self.handle_player(event),
            // Progress needs only the repaint that came with it.
            Response::Download(message) => {
                if let Some(m) = message {
                    self.notify(m);
                }
            }
            Response::Resolved { video_id, result } => {
                let Some((_, at)) = self.pending_play.take_if(|(id, _)| *id == video_id) else {
                    return; // a background prefetch; it's cached in the resolver
                };
                match result {
                    Ok(url) => {
                        self.player.load(&url, at);
                        self.player.set_paused(false);
                    }
                    Err(e) => {
                        self.notify(format!("Couldn't load track: {e}"));
                        self.next();
                    }
                }
            }
        }
    }

    fn handle_player(&mut self, event: PlayerEvent) {
        let pb = &mut self.playback;
        match event {
            PlayerEvent::Loading => pb.loading = true,
            PlayerEvent::Position(t) => {
                if pb.loading {
                    trace!("app: audio playing");
                }
                pb.loading = false;
                pb.set_position(t);
            }
            PlayerEvent::Duration(d) => pb.duration = d,
            PlayerEvent::Paused(p) => {
                let pos = pb.position();
                pb.paused = p;
                pb.set_position(pos);
            }
            PlayerEvent::Finished => self.next(),
            PlayerEvent::Error(e) => {
                self.notify(e);
                self.next();
            }
            // The engine let the paused track go: like the track restored at
            // startup, play loads it again here (with a fresh URL if needed).
            PlayerEvent::Released(at) => {
                pb.set_position(at);
                self.resume_at = Some(at);
                if !self.playback.paused {
                    self.start(at); // play was pressed just as it was let go
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (reload, space, find, nav, back) = ctx.input(|i| {
            let nav = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3]
                .iter()
                .position(|k| i.modifiers.command && i.key_pressed(*k));
            (
                i.key_pressed(egui::Key::F5),
                !typing && i.key_pressed(egui::Key::Space),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                nav,
                i.pointer.button_pressed(egui::PointerButton::Extra1)
                    || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)),
            )
        });
        if reload {
            self.reload();
        }
        if space {
            self.toggle_pause();
        }
        if find {
            self.go(ctx, View::Search);
        }
        if back && self.can_go_back() {
            self.back(ctx);
        }
        if let Some(view) = nav.and_then(|n| self.config.layout.sidebar_items.get(n)).and_then(|n| View::from_name(n)) {
            self.go(ctx, view);
        }
    }
}

impl App {
    pub fn logic(&mut self, ctx: &egui::Context) {
        while let Ok(response) = self.responses.try_recv() {
            self.handle(ctx, response);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        // Messages that arrived since the last wake-up (cheap if none).
        self.logic(&ui.ctx().clone());
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let (now, input) = ctx.input(|i| (i.time, !i.events.is_empty()));
        if input {
            self.last_input = now;
        }
        let blink = now - self.last_input < CARET_TIMEOUT;
        if ctx.global_style().visuals.text_cursor.blink != blink {
            ctx.all_styles_mut(|s| s.visuals.text_cursor.blink = blink);
        }
        // Typed or pasted text in another script: get its fonts right away.
        if !self.fallback_fonts && theme::needs_fallback(&self.search.query) {
            self.use_fallback_fonts(&ctx);
        }
        let c = self.theme.colors.clone();
        let pad = self.theme.padding.clamp(0.0, 60.0) as i8;

        let bar_frame = Frame::NONE.fill(c.panel).inner_margin(Margin::symmetric(pad + 4, pad));
        let bar_panel = match self.config.layout.bar_position {
            BarPosition::Top => Panel::top("now-playing"),
            BarPosition::Bottom => Panel::bottom("now-playing"),
        };
        bar_panel
            .frame(bar_frame)
            .show_separator_line(false)
            .exact_size(bar::HEIGHT + 2.0 * pad as f32)
            .show_inside(ui, |ui| bar::show(self, ui));

        if self.config.layout.sidebar {
            Panel::left("sidebar")
                .resizable(false)
                .show_separator_line(false)
                .exact_size(210.0)
                .frame(Frame::NONE.fill(c.panel).inner_margin(Margin::same(pad)))
                .show_inside(ui, |ui| views::sidebar(self, ui));
        }

        CentralPanel::default()
            .frame(Frame::NONE.fill(c.background).inner_margin(Margin::symmetric(pad.saturating_mul(2), pad)))
            .show_inside(ui, |ui| views::content(self, ui));

        self.images.evict();

        // Clear old notices.
        if let Some((_, at)) = &self.notice {
            let age = at.elapsed();
            if age >= Duration::from_secs(8) {
                self.notice = None;
            } else {
                ctx.request_repaint_after(Duration::from_secs(8) - age);
            }
        }
        // Tick the clock once per displayed second while playing.
        if self.queue.current.is_some() && !self.playback.paused && !self.playback.loading {
            let frac = self.playback.position().fract();
            ctx.request_repaint_after(Duration::from_secs_f64((1.0 - frac).max(0.05)));
        }
    }
}

fn track_needs_fallback(t: &Track) -> bool {
    theme::needs_fallback(&t.title) || theme::needs_fallback(&t.artists) || t.album.as_deref().is_some_and(theme::needs_fallback)
}

/// Whether a response carries text Inter can't draw.
fn response_needs_fallback(response: &Response) -> bool {
    let lists = |l: &[Playlist]| l.iter().any(|p| theme::needs_fallback(&p.title) || theme::needs_fallback(&p.subtitle));
    match response {
        Response::Search { result: Ok(tracks), .. } | Response::PlaylistPage { tracks, .. } | Response::Radio { tracks, .. } => {
            tracks.iter().any(track_needs_fallback)
        }
        Response::SearchAlbums { albums, .. } | Response::Library { result: Ok(albums), .. } => lists(albums),
        Response::Artist { result: Ok(a), .. } => {
            theme::needs_fallback(&a.name) || a.songs.iter().any(track_needs_fallback) || lists(&a.albums) || lists(&a.singles)
        }
        _ => false,
    }
}

/// Tells the resolver about a likely play, unless it is downloaded (it
/// plays from disk then, nothing to fetch).
fn hint(resolver: &Resolver, downloads: &Downloads, id: &str, hint: Hint) {
    if !downloads.has(id) {
        resolver.hint(id, hint);
    }
}

/// Fisher–Yates with a time-seeded xorshift (no RNG crate needed).
fn shuffle_tracks(tracks: &mut [Track]) {
    let mut s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
    for i in (1..tracks.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        tracks.swap(i, (s % (i as u64 + 1)) as usize);
    }
}

// The last-played file: one field per line, position last. Plain text keeps
// it readable and needs no serializer.
const LAST_FIELDS: usize = 10;

fn save_last_played(track: Option<&Track>, position: f64) {
    let path = paths::last_played_file();
    let Some(t) = track else {
        let _ = std::fs::remove_file(path);
        return;
    };
    let opt = |o: &Option<String>| o.clone().unwrap_or_default();
    let fields = [
        t.video_id.clone(),
        t.title.clone(),
        t.artists.clone(),
        opt(&t.album),
        opt(&t.duration),
        opt(&t.thumb),
        t.artist_links.iter().map(|(r, id)| format!("{}:{}:{id}", r.start, r.end)).collect::<Vec<_>>().join(" "),
        opt(&t.album_id),
        t.liked.map_or(String::new(), |l| l.to_string()),
        format!("{position:.1}"),
    ];
    let text: Vec<String> = fields.iter().map(|f| f.replace('\n', " ")).collect();
    let _ = std::fs::write(path, text.join("\n"));
}

fn load_last_played() -> Option<(Track, f64)> {
    let text = std::fs::read_to_string(paths::last_played_file()).ok()?;
    let f: Vec<&str> = text.split('\n').collect();
    if f.len() != LAST_FIELDS || f[0].is_empty() {
        return None;
    }
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    // "start:end:id" per artist; older files hold one id for all the artists.
    let artist_links = f[6]
        .split(' ')
        .filter(|l| !l.is_empty())
        .filter_map(|l| match l.splitn(3, ':').collect::<Vec<_>>()[..] {
            [id] => Some((0..f[2].len(), id.to_string())),
            [s, e, id] => Some((s.parse().ok()?..e.parse().ok()?, id.to_string())),
            _ => None,
        })
        .collect();
    let track = Track {
        video_id: f[0].to_string(),
        title: f[1].to_string(),
        artists: f[2].to_string(),
        album: opt(f[3]),
        duration: opt(f[4]),
        thumb: opt(f[5]),
        set_video_id: None,
        artist_links,
        album_id: opt(f[7]),
        liked: f[8].parse().ok(),
        plays: None,
    };
    Some((track, f[9].parse().unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: usize, duration: Option<&str>) -> Track {
        Track {
            video_id: id.to_string(),
            title: String::new(),
            artists: String::new(),
            album: None,
            duration: duration.map(str::to_string),
            thumb: None,
            set_video_id: None,
            artist_links: Vec::new(),
            album_id: None,
            liked: None,
            plays: None,
        }
    }

    #[test]
    fn clock_strings_parse() {
        assert_eq!(track(0, Some("3:45")).seconds(), 225.0);
        assert_eq!(track(0, Some("1:02:03")).seconds(), 3723.0);
        assert_eq!(track(0, Some("live")).seconds(), 0.0);
        assert_eq!(track(0, None).seconds(), 0.0);
    }

    #[test]
    fn shuffle_keeps_every_track() {
        let mut tracks: Vec<Track> = (0..50).map(|i| track(i, None)).collect();
        shuffle_tracks(&mut tracks);
        let mut ids: Vec<usize> = tracks.iter().map(|t| t.video_id.parse().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, (0..50).collect::<Vec<_>>());
    }
}

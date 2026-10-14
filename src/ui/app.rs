//! Application state and the glue between UI, worker and player.
//!
//! `logic` runs whenever something woke us (input, a worker message, an
//! animation) and drains messages, even while the window is hidden, so the
//! queue keeps advancing. `ui` lays out the frame; the shell (shell.rs) only
//! calls it while the window is visible and something changed.

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{CentralPanel, Frame, Margin, Panel, Ui};

use super::images::Images;
use super::{bar, views};
use crate::api::client;
use crate::api::models::{Playlist, Track};
use crate::auth;
use crate::config::{BarPosition, Config};
use crate::player::{Player, PlayerEvent};
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Search,
    Library,
    Playlist,
    Queue,
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
            View::Playlist => "Playlist",
            View::Queue => "Queue",
        }
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<Track>,
    pub loading: bool,
    pub searched: bool,
    pub focus: bool,
}

pub struct PlaylistState {
    pub meta: Playlist,
    pub tracks: Vec<Track>,
    pub loading: bool,
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
    requests: Sender<Request>,
    responses: Receiver<Response>,
    notifier: Notifier,
    player: Option<Player>,
    resolver: Resolver,
    /// Track we're waiting on a stream URL for.
    pending_play: Option<String>,
    /// Row under the mouse, since when, and whether it was prefetched.
    hover: Option<(String, Instant, bool)>,
    /// Row currently pressed (prefetched immediately, before the click lands).
    pressed: Option<String>,
    /// Rows on screen, since when, and whether they were handed to the resolver.
    visible: (Vec<String>, Instant, bool),
    pub images: Images,

    pub view: View,
    /// egui time the current view appeared, for the fade-in.
    pub view_since: f64,
    pub search: SearchState,
    pub library: Option<Vec<Playlist>>,
    pub library_loading: bool,
    pub playlist: Option<PlaylistState>,

    pub queue: Vec<Track>,
    pub pos: Option<usize>,
    radio_pending: bool,
    radio_seed: Option<String>,
    advance_on_radio: bool,
    pub playback: Playback,

    pub notice: Option<(String, Instant)>,
    pub signed_out: bool,
    pub signing_in: bool,
}

impl App {
    pub fn new(ctx: &egui::Context, config: Config, loaded: theme::Loaded, config_error: Option<String>) -> App {
        theme::install_fonts(ctx, loaded.font);
        loaded.theme.apply(ctx);

        let agent = client::agent();
        let (requests, responses, notifier) = worker::spawn(ctx.clone(), agent.clone());
        let images = Images::new(ctx, agent.clone());

        let mut notice = config_error.or(loaded.error);
        let player_notifier = notifier.clone();
        // Shares the agent (and its kept-alive connection to YouTube Music) with the API worker.
        let resolver = Resolver::new(&config.player.ytdlp, notifier.clone(), agent.clone());
        let player = match Player::spawn(&config.player, agent, move |e| player_notifier.send(Response::Player(e))) {
            Ok(p) => Some(p),
            Err(e) => {
                notice = Some(format!("Couldn't start the audio engine ({e})."));
                None
            }
        };

        let view = config.layout.sidebar_items.iter().find_map(|n| View::from_name(n)).unwrap_or(View::Library);
        let _ = requests.send(Request::Library);
        App {
            font_file: loaded.theme.font_file.clone(),
            playback: Playback {
                loading: false,
                paused: false,
                base: 0.0,
                at: Instant::now(),
                duration: 0.0,
                volume: config.player.volume.min(100) as f32 / 100.0,
            },
            config,
            theme: loaded.theme,
            requests,
            responses,
            notifier,
            player,
            resolver,
            pending_play: None,
            hover: None,
            pressed: None,
            visible: (Vec::new(), Instant::now(), true),
            images,
            view,
            view_since: 0.0,
            search: SearchState { focus: view == View::Search, ..Default::default() },
            library: None,
            library_loading: true,
            playlist: None,
            queue: Vec::new(),
            pos: None,
            radio_pending: false,
            radio_seed: None,
            advance_on_radio: false,
            notice: notice.map(|n| (n, Instant::now())),
            signed_out: false,
            signing_in: false,
        }
    }

    // ---- navigation -------------------------------------------------------

    pub fn go(&mut self, ctx: &egui::Context, view: View) {
        if self.view != view {
            self.view = view;
            self.view_since = ctx.input(|i| i.time);
        }
        if view == View::Search {
            self.search.focus = true;
        }
        if view == View::Library && self.library.is_none() && !self.library_loading {
            self.library_loading = true;
            let _ = self.requests.send(Request::Library);
        }
    }

    pub fn open_playlist(&mut self, ctx: &egui::Context, meta: Playlist) {
        let _ = self.requests.send(Request::Playlist(meta.id.clone()));
        self.playlist = Some(PlaylistState { meta, tracks: Vec::new(), loading: true });
        self.go(ctx, View::Playlist);
    }

    pub fn submit_search(&mut self) {
        let q = self.search.query.trim().to_string();
        if !q.is_empty() {
            self.search.loading = true;
            let _ = self.requests.send(Request::Search(q));
        }
    }

    pub fn reload(&self) {
        let _ = self.requests.send(Request::Reload);
    }

    pub fn sign_in_again(&mut self) {
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

    // ---- playback ---------------------------------------------------------

    pub fn now_playing(&self) -> Option<&Track> {
        self.pos.and_then(|i| self.queue.get(i))
    }

    /// Replaces the queue with `list` and starts at `index`. `radio` fills the
    /// queue with related tracks (used when playing a single search result).
    pub fn play_list(&mut self, list: Vec<Track>, index: usize, radio: bool) {
        if index >= list.len() {
            return;
        }
        self.queue = list;
        self.radio_pending = false;
        self.radio_seed = None;
        self.advance_on_radio = false;
        self.start(index);
        if radio {
            self.request_radio(false);
        }
    }

    pub fn play_queue_index(&mut self, index: usize) {
        if index < self.queue.len() {
            self.start(index);
        }
    }

    fn start(&mut self, index: usize) {
        self.pos = Some(index);
        let Some(player) = &self.player else {
            self.notify("Playback unavailable: the audio engine isn't running.");
            return;
        };
        let id = self.queue[index].video_id.clone();
        let ready = self.resolver.ready(&id);
        trace!("app: play {id} ({})", if ready.is_some() { "url ready" } else { "resolving" });
        match ready {
            Some(url) => {
                self.resolver.playing(&url);
                player.load(&url);
                player.set_paused(false);
                self.pending_play = None;
            }
            None => {
                // Silence the previous track while the new one resolves.
                player.stop();
                self.pending_play = Some(id.clone());
                self.resolver.want(&id);
            }
        }
        self.playback.loading = true;
        self.playback.paused = false;
        self.playback.duration = 0.0;
        self.playback.set_position(0.0);
        self.prefetch_ahead();
    }

    /// Gets the next queue tracks ready: the very next one fully (stream
    /// resolved and its start preloaded), the ones after resolved.
    fn prefetch_ahead(&self) {
        let Some(pos) = self.pos else { return };
        for (i, t) in self.queue.iter().skip(pos + 1).take(LOOKAHEAD).enumerate() {
            self.resolver.hint(&t.video_id, if i == 0 { Hint::Likely } else { Hint::Visible });
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
                self.resolver.hint(id, Hint::Visible);
            }
        }
    }

    /// A row is being pressed: the click is coming, start right away.
    pub fn pressing(&mut self, video_id: Option<&str>) {
        if let Some(id) = video_id
            && self.pressed.as_deref() != Some(id)
        {
            self.resolver.hint(id, Hint::Likely);
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
                    self.resolver.hint(id, Hint::Likely);
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
        if let Some(seed) = self.queue.last() {
            self.radio_pending = true;
            self.radio_seed = Some(seed.video_id.clone());
            let _ = self.requests.send(Request::Radio { video_id: seed.video_id.clone(), autoplay });
        }
    }

    pub fn next(&mut self) {
        match self.pos {
            Some(i) if i + 1 < self.queue.len() => self.start(i + 1),
            // A prefetch is already on its way: play its first track when it lands.
            Some(_) if self.radio_pending => self.advance_on_radio = true,
            Some(_) => self.request_radio(true),
            None => {}
        }
    }

    pub fn prev(&mut self) {
        match self.pos {
            Some(i) if self.playback.position() < 3.0 && i > 0 => self.start(i - 1),
            Some(_) => self.seek(0.0),
            None => {}
        }
    }

    pub fn toggle_pause(&mut self) {
        let (Some(player), Some(_)) = (&self.player, self.pos) else { return };
        // Optimistic: flip immediately so the button reacts on click.
        let pos = self.playback.position();
        self.playback.paused = !self.playback.paused;
        self.playback.set_position(pos);
        player.set_paused(self.playback.paused);
    }

    pub fn seek(&mut self, seconds: f64) {
        if let Some(player) = &self.player {
            player.seek(seconds);
            self.playback.set_position(seconds);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.playback.volume = volume;
        if let Some(player) = &self.player {
            player.set_volume((volume * 100.0).round() as f64);
        }
    }

    // ---- messages ---------------------------------------------------------

    fn handle(&mut self, ctx: &egui::Context, response: Response) {
        match response {
            Response::Search { query, result } => {
                if query == self.search.query.trim() {
                    self.search.loading = false;
                    self.search.searched = true;
                    match result {
                        Ok(tracks) => {
                            // The top result is the most likely click: get it ready.
                            if let Some(top) = tracks.first() {
                                self.resolver.hint(&top.video_id, Hint::Likely);
                            }
                            self.search.results = tracks;
                        }
                        Err(e) => self.notify(format!("Search failed: {e}")),
                    }
                }
            }
            Response::Library(result) => {
                self.library_loading = false;
                match result {
                    Ok(list) => self.library = Some(list),
                    Err(e) => self.notify(format!("Couldn't load library: {e}")),
                }
            }
            Response::PlaylistPage { id, tracks, done } => {
                if let Some(pl) = self.playlist.as_mut().filter(|p| p.meta.id == id) {
                    pl.tracks.extend(tracks);
                    pl.loading = !done;
                }
            }
            Response::PlaylistFailed { id, error } => {
                if let Some(pl) = self.playlist.as_mut().filter(|p| p.meta.id == id) {
                    pl.loading = false;
                    self.notify(format!("Couldn't load playlist: {error}"));
                }
            }
            Response::Radio { seed, tracks, autoplay } => {
                // Ignore answers for a queue that has since been replaced.
                if self.radio_seed.as_deref() != Some(seed.as_str()) {
                    return;
                }
                self.radio_seed = None;
                self.radio_pending = false;
                let before = self.queue.len();
                for t in tracks {
                    if !self.queue.iter().any(|q| q.video_id == t.video_id) {
                        self.queue.push(t);
                    }
                }
                let advance = autoplay || std::mem::take(&mut self.advance_on_radio);
                if advance && self.queue.len() > before {
                    self.start(before);
                } else {
                    self.prefetch_ahead();
                }
            }
            Response::Reloaded { config, theme: loaded, config_error } => {
                if loaded.theme.font_file != self.font_file {
                    self.font_file = loaded.theme.font_file.clone();
                    theme::install_fonts(ctx, loaded.font);
                }
                loaded.theme.apply(ctx);
                self.theme = loaded.theme;
                self.config = config;
                match config_error.or(loaded.error) {
                    Some(e) => self.notify(e),
                    None => self.notify("Theme and config reloaded"),
                }
            }
            Response::LoggedOut => self.signed_out = true,
            Response::SignInFinished(ok) => {
                self.signing_in = false;
                if ok {
                    self.signed_out = false;
                    let _ = self.requests.send(Request::ReloadSession);
                    self.library_loading = true;
                    let _ = self.requests.send(Request::Library);
                    self.notify("Signed in");
                } else {
                    self.notify("Sign-in was cancelled");
                }
            }
            Response::Player(event) => self.handle_player(event),
            Response::Resolved { video_id, result } => {
                if self.pending_play.as_deref() != Some(video_id.as_str()) {
                    return; // a background prefetch; it's cached in the resolver
                }
                self.pending_play = None;
                match (result, &self.player) {
                    (Ok(url), Some(player)) => {
                        player.load(&url);
                        player.set_paused(false);
                    }
                    (Ok(_), None) => {}
                    (Err(e), _) => {
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
            PlayerEvent::Loading => {
                pb.loading = true;
                pb.set_position(0.0);
            }
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
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (reload, space, find, nav) = ctx.input(|i| {
            let nav = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3]
                .iter()
                .position(|k| i.modifiers.command && i.key_pressed(*k));
            (
                i.key_pressed(egui::Key::F5),
                !typing && i.key_pressed(egui::Key::Space),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                nav,
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
        if self.pos.is_some() && !self.playback.paused && !self.playback.loading {
            let frac = self.playback.position().fract();
            ctx.request_repaint_after(Duration::from_secs_f64((1.0 - frac).max(0.05)));
        }
    }
}

//! The single background thread that does all network and disk work for the
//! UI. The UI sends a `Request`, the worker answers with a `Response` and
//! wakes the UI. Long playlists are fetched one page per request, so a
//! search typed meanwhile is never stuck behind a 5,000-track download.

use std::sync::mpsc::{self, Receiver, Sender};

use crate::api::client::{ApiError, Client};
use crate::api::models::{Artist, Continuation, Header, Kind, Playlist, Track};
use crate::auth::session::Session;
use crate::config::{self, Config, EqualizerConfig};
use crate::paths;
use crate::player::PlayerEvent;
use crate::theme::{self, Theme};

pub enum Request {
    /// Songs, then albums.
    Search(String),
    /// Saved playlists (`albums` false) or saved albums.
    Library { albums: bool },
    Playlist(Playlist),
    PlaylistMore(Playlist, Continuation),
    Artist(String),
    /// Up-next tracks seeded from a video; `autoplay` starts the first one.
    Radio { video_id: String, autoplay: bool },
    /// Re-read config.toml and the theme it names.
    Reload,
    /// Re-read cookies.txt after signing in.
    ReloadSession,
    /// Forget the local session (cookies.txt).
    SignOut,
    ListThemes,
    /// Save the theme choice to config.toml, then reload.
    SetTheme(String),
    SaveEqualizer(EqualizerConfig),
    SavePitch(i32),
    CreatePlaylist(String),
    DeletePlaylist(Playlist),
    AddToPlaylist { id: String, title: String, video_id: String },
    /// Move an entry to just before `before` (or to the end).
    MoveInPlaylist { id: String, set_video_id: String, before: Option<String> },
    Like { video_id: String, like: bool },
    /// Save a playlist or album to the library (by `save_id`), or remove it.
    Save { list: Playlist, save_id: String, save: bool },
}

pub enum Response {
    Search { query: String, result: Result<Vec<Track>, String> },
    SearchAlbums { query: String, albums: Vec<Playlist> },
    Library { albums: bool, result: Result<Vec<Playlist>, String> },
    /// `header` is set on the first page only.
    PlaylistPage { id: String, tracks: Vec<Track>, done: bool, header: Option<Box<Header>> },
    PlaylistFailed { id: String, error: String },
    Artist { id: String, result: Result<Box<Artist>, String> },
    Radio { seed: String, tracks: Vec<Track>, autoplay: bool },
    Reloaded { config: Box<Config>, theme: Box<theme::Loaded>, config_error: Option<String> },
    LoggedOut,
    SignedOut,
    Themes(Vec<String>),
    /// A library edit finished: a message for the user, and whether the
    /// library (or the open playlist) should be fetched again.
    Edited { message: String, refresh_library: bool, refresh_playlist: Option<String> },
    /// A playlist was created. The library listing lags behind edits, so the
    /// UI adds it locally instead of fetching the library again.
    Created(Playlist),
    /// A like/unlike didn't go through: the UI puts the old state back.
    LikeFailed { video_id: String, liked: bool, error: String },
    Player(PlayerEvent),
    /// A stream URL lookup finished (see stream/mod.rs).
    Resolved { video_id: String, result: Result<String, String> },
    SignInFinished(bool),
}

/// Sends a response and wakes the UI. While the window is hidden only
/// `App::logic` runs for it (no layout, no painting).
#[derive(Clone)]
pub struct Notifier {
    tx: Sender<Response>,
    ctx: egui::Context,
}

impl Notifier {
    pub fn send(&self, response: Response) {
        if self.tx.send(response).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

pub fn spawn(ctx: egui::Context, agent: ureq::Agent) -> (Sender<Request>, Receiver<Response>, Notifier) {
    let (req_tx, req_rx) = mpsc::channel();
    let (resp_tx, resp_rx) = mpsc::channel();
    let notifier = Notifier { tx: resp_tx, ctx };
    let worker_notifier = notifier.clone();
    let requeue = req_tx.clone();
    std::thread::Builder::new()
        .name("api-worker".into())
        .spawn(move || run(req_rx, requeue, worker_notifier, agent))
        .expect("failed to start worker thread");
    (req_tx, resp_rx, notifier)
}

fn run(requests: Receiver<Request>, requeue: Sender<Request>, out: Notifier, agent: ureq::Agent) {
    let mut client = Client::new(agent, Session::load(&paths::cookies_file()));
    // Turns an API error into a message for the UI, reporting expired sessions once.
    let err = |e: ApiError| -> String {
        match e {
            ApiError::LoggedOut => {
                out.send(Response::LoggedOut);
                "signed out".into()
            }
            ApiError::Other(msg) => msg,
        }
    };
    let edited = |message: String, refresh_library: bool, refresh_playlist: Option<String>| {
        out.send(Response::Edited { message, refresh_library, refresh_playlist });
    };

    for request in requests {
        match request {
            Request::Search(query) => {
                let result = client.search(&query).map_err(err);
                let ok = result.is_ok();
                out.send(Response::Search { query: query.clone(), result });
                if ok {
                    let albums = client.search_albums(&query).unwrap_or_default();
                    out.send(Response::SearchAlbums { query, albums });
                }
            }
            Request::Library { albums } => out.send(Response::Library { albums, result: client.library(albums).map_err(err) }),
            Request::Playlist(list) => playlist_page(&client, &requeue, &out, list, None, err),
            Request::PlaylistMore(list, cont) => playlist_page(&client, &requeue, &out, list, Some(cont), err),
            Request::Artist(id) => {
                let result = client.artist(&id).map(Box::new).map_err(err);
                out.send(Response::Artist { id, result });
            }
            Request::Radio { video_id, autoplay } => {
                let tracks = client.radio(&video_id).map_err(err).unwrap_or_default();
                out.send(Response::Radio { seed: video_id, tracks, autoplay });
            }
            Request::Reload => reload(&out),
            Request::ReloadSession => client.set_session(Session::load(&paths::cookies_file())),
            Request::SignOut => {
                let _ = std::fs::remove_file(paths::cookies_file());
                let _ = std::fs::write(paths::signed_out_marker(), "");
                client.set_session(Session::default());
                out.send(Response::SignedOut);
            }
            Request::ListThemes => out.send(Response::Themes(config::list_themes())),
            Request::SetTheme(name) => match config::save_theme(&name) {
                Ok(()) => reload(&out),
                Err(e) => edited(format!("Couldn't save theme: {e}"), false, None),
            },
            Request::SaveEqualizer(eq) => {
                if let Err(e) = config::save_equalizer(&eq) {
                    edited(format!("Couldn't save equalizer: {e}"), false, None);
                }
            }
            Request::SavePitch(semitones) => {
                if let Err(e) = config::save_pitch(semitones) {
                    edited(format!("Couldn't save pitch: {e}"), false, None);
                }
            }
            Request::CreatePlaylist(title) => match client.create_playlist(&title) {
                Ok(id) => out.send(Response::Created(Playlist {
                    id,
                    kind: Kind::Playlist,
                    title,
                    subtitle: "Private".into(),
                    thumb: None,
                    owned: true,
                    save_id: None,
                })),
                Err(e) => edited(format!("Couldn't create playlist: {}", err(e)), false, None),
            },
            Request::DeletePlaylist(list) => match client.delete_playlist(&list.id) {
                Ok(()) => edited(format!("Deleted \u{201c}{}\u{201d}", list.title), false, None),
                // The UI already removed it: bring the real library back.
                Err(e) => edited(format!("Couldn't delete playlist: {}", err(e)), true, None),
            },
            Request::AddToPlaylist { id, title, video_id } => match client.add_to_playlist(&id, &video_id) {
                Ok(()) => edited(format!("Added to \u{201c}{title}\u{201d}"), false, Some(id)),
                Err(e) => edited(format!("Couldn't add to playlist: {}", err(e)), false, None),
            },
            Request::MoveInPlaylist { id, set_video_id, before } => {
                if let Err(e) = client.move_in_playlist(&id, &set_video_id, before.as_deref()) {
                    // The local order is now wrong: show the real one again.
                    edited(format!("Couldn't reorder: {}", err(e)), false, Some(id));
                }
            }
            Request::Like { video_id, like } => {
                if let Err(e) = client.like(&video_id, like) {
                    out.send(Response::LikeFailed { video_id, liked: !like, error: err(e) });
                }
            }
            Request::Save { list, save_id, save } => match client.save(&save_id, save) {
                Ok(()) => {
                    let verb = if save { "Saved" } else { "Removed" };
                    edited(format!("{verb} \u{201c}{}\u{201d}", list.title), false, None);
                }
                // The UI already changed the library: bring the real one back.
                Err(e) => edited(format!("Couldn't update library: {}", err(e)), true, None),
            },
        }
    }
}

fn reload(out: &Notifier) {
    let (config, config_error) = Config::load();
    let theme = Theme::load(&config.theme);
    out.send(Response::Reloaded { config: Box::new(config), theme: Box::new(theme), config_error });
}

fn playlist_page(
    client: &Client,
    requeue: &Sender<Request>,
    out: &Notifier,
    list: Playlist,
    cont: Option<Continuation>,
    err: impl Fn(ApiError) -> String,
) {
    match client.playlist_page(&list, cont.as_ref()) {
        Ok(page) => {
            // Stop if a page comes back empty, so a bad token can't loop forever.
            let next = page.next.filter(|_| !page.tracks.is_empty());
            let done = next.is_none();
            let id = list.id.clone();
            if let Some(next) = next {
                let _ = requeue.send(Request::PlaylistMore(list, next));
            }
            out.send(Response::PlaylistPage { id, tracks: page.tracks, done, header: page.header.map(Box::new) });
        }
        Err(e) => {
            let error = err(e);
            out.send(Response::PlaylistFailed { id: list.id, error });
        }
    }
}

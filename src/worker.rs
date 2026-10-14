//! The single background thread that does all network and disk work for the
//! UI. The UI sends a `Request`, the worker answers with a `Response` and
//! wakes the UI. Long playlists are fetched one page per request, so a
//! search typed meanwhile is never stuck behind a 5,000-track download.

use std::sync::mpsc::{self, Receiver, Sender};

use crate::api::client::{ApiError, Client};
use crate::api::models::{Continuation, Playlist, Track};
use crate::auth::session::Session;
use crate::config::Config;
use crate::paths;
use crate::player::PlayerEvent;
use crate::theme::{self, Theme};

pub enum Request {
    Search(String),
    Library,
    Playlist(String),
    PlaylistMore(String, Continuation),
    /// Up-next tracks seeded from a video; `autoplay` starts the first one.
    Radio { video_id: String, autoplay: bool },
    /// Re-read config.toml and the theme it names.
    Reload,
    /// Re-read cookies.txt after signing in again.
    ReloadSession,
}

pub enum Response {
    Search { query: String, result: Result<Vec<Track>, String> },
    Library(Result<Vec<Playlist>, String>),
    PlaylistPage { id: String, tracks: Vec<Track>, done: bool },
    PlaylistFailed { id: String, error: String },
    Radio { seed: String, tracks: Vec<Track>, autoplay: bool },
    Reloaded { config: Config, theme: theme::Loaded, config_error: Option<String> },
    LoggedOut,
    Player(PlayerEvent),
    /// A stream URL lookup finished (see resolver.rs).
    Resolved { video_id: String, result: Result<String, String> },
    SignInFinished(bool),
}

/// Sends a response and wakes the UI. While the window is hidden eframe only
/// runs `App::logic` for this (no layout, no painting).
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

    for request in requests {
        match request {
            Request::Search(query) => {
                let result = client.search(&query).map_err(err);
                out.send(Response::Search { query, result });
            }
            Request::Library => out.send(Response::Library(client.library().map_err(err))),
            Request::Playlist(id) => playlist_page(&client, &requeue, &out, id, None, err),
            Request::PlaylistMore(id, cont) => playlist_page(&client, &requeue, &out, id, Some(cont), err),
            Request::Radio { video_id, autoplay } => {
                let tracks = client.radio(&video_id).map_err(err).unwrap_or_default();
                out.send(Response::Radio { seed: video_id, tracks, autoplay });
            }
            Request::Reload => {
                let (config, config_error) = Config::load();
                let theme = Theme::load(&config.theme);
                out.send(Response::Reloaded { config, theme, config_error });
            }
            Request::ReloadSession => client.set_session(Session::load(&paths::cookies_file())),
        }
    }
}

fn playlist_page(
    client: &Client,
    requeue: &Sender<Request>,
    out: &Notifier,
    id: String,
    cont: Option<Continuation>,
    err: impl Fn(ApiError) -> String,
) {
    match client.playlist_page(&id, cont.as_ref()) {
        Ok((tracks, next)) => {
            // Stop if a page comes back empty, so a bad token can't loop forever.
            let next = next.filter(|_| !tracks.is_empty());
            let done = next.is_none();
            if let Some(next) = next {
                let _ = requeue.send(Request::PlaylistMore(id.clone(), next));
            }
            out.send(Response::PlaylistPage { id, tracks, done });
        }
        Err(e) => {
            let error = err(e);
            out.send(Response::PlaylistFailed { id, error });
        }
    }
}

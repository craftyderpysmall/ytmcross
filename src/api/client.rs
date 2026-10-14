//! InnerTube client: the same private JSON API the YouTube Music website uses.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::models::{Artist, Continuation, Header, Kind, Playlist, Track};
use super::parse;
use crate::auth::session::{ORIGIN, Session};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// Search filters (same values the website sends).
const SONGS_FILTER: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";
const ALBUMS_FILTER: &str = "EgWKAQIYAWoMEA4QChADEAQQCRAF";

#[derive(Debug)]
pub enum ApiError {
    LoggedOut,
    Other(String),
}

impl From<ureq::Error> for ApiError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(401 | 403) => ApiError::LoggedOut,
            e => ApiError::Other(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

pub struct PlaylistPage {
    pub tracks: Vec<Track>,
    pub next: Option<Continuation>,
    /// Set on the first page only.
    pub header: Option<Header>,
}

pub struct Client {
    agent: ureq::Agent,
    session: Session,
    client_version: String,
}

/// WEB_REMIX client version, dated today (as the website's own is).
pub fn client_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| format!("1.{}.01.00", yyyymmdd()))
}

impl Client {
    pub fn new(agent: ureq::Agent, session: Session) -> Client {
        Client { agent, session, client_version: client_version().to_string() }
    }

    pub fn set_session(&mut self, session: Session) {
        self.session = session;
    }

    fn post(&self, endpoint: &str, mut body: Value, query: &str) -> Result<Value> {
        body["context"] = json!({"client": {"clientName": "WEB_REMIX", "clientVersion": self.client_version, "hl": "en"}});
        let url = format!("{ORIGIN}/youtubei/v1/{endpoint}?prettyPrint=false{query}");
        let mut req = self
            .agent
            .post(&url)
            .header("Origin", ORIGIN)
            .header("X-Origin", ORIGIN)
            .header("X-Goog-AuthUser", "0");
        if let Some(auth) = self.session.auth_header() {
            req = req.header("Cookie", self.session.cookie_header()).header("Authorization", auth);
        }
        let resp: Value = req.send_json(&body)?.body_mut().read_json()?;
        // Signed out on purpose, every answer says logged out: only report
        // it when there was a session to lose (it expired).
        if self.session.is_signed_in() && parse::logged_in(&resp) == Some(false) {
            return Err(ApiError::LoggedOut);
        }
        Ok(resp)
    }

    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.post("search", json!({"query": query, "params": SONGS_FILTER}), "")?;
        Ok(parse::tracks(&resp))
    }

    pub fn search_albums(&self, query: &str) -> Result<Vec<Playlist>> {
        let resp = self.post("search", json!({"query": query, "params": ALBUMS_FILTER}), "")?;
        let mut albums = parse::playlists(&resp);
        albums.retain(|a| a.kind == Kind::Album);
        Ok(albums)
    }

    /// Saved playlists (`albums` false) or saved albums.
    pub fn library(&self, albums: bool) -> Result<Vec<Playlist>> {
        let id = if albums { "FEmusic_liked_albums" } else { "FEmusic_liked_playlists" };
        Ok(parse::playlists(&self.post("browse", json!({"browseId": id}), "")?))
    }

    pub fn artist(&self, id: &str) -> Result<Artist> {
        Ok(parse::artist(&self.post("browse", json!({"browseId": id}), "")?))
    }

    /// One page of a playlist or album. Pass the returned continuation to
    /// get the next. The first page also carries the header.
    pub fn playlist_page(&self, list: &Playlist, cont: Option<&Continuation>) -> Result<PlaylistPage> {
        let resp = match cont {
            None => self.post("browse", json!({"browseId": list.browse_id()}), "")?,
            Some(Continuation::Command(t)) => self.post("browse", json!({"continuation": t}), "")?,
            Some(Continuation::Next(t)) => self.post("browse", json!({}), &format!("&ctoken={t}&continuation={t}&type=next"))?,
        };
        let header = cont.is_none().then(|| parse::header(&resp));
        let scope = parse::playlist_scope(&resp);
        let mut tracks = parse::tracks(scope);
        // Album rows leave out what the header already says.
        if list.kind == Kind::Album
            && let Some(h) = &header
        {
            for t in &mut tracks {
                if t.artists.is_empty()
                    && let Some((name, id)) = &h.artist
                {
                    t.artists = name.clone();
                    t.artist_id = id.clone();
                }
                t.album.get_or_insert_with(|| list.title.clone());
                t.album_id.get_or_insert_with(|| list.id.clone());
                if t.thumb.is_none() {
                    t.thumb = h.thumb.clone().or_else(|| list.thumb.clone());
                }
            }
        }
        Ok(PlaylistPage { tracks, next: parse::continuation(scope), header })
    }

    /// Adds a song to Liked Music, or takes it out.
    pub fn like(&self, video_id: &str, like: bool) -> Result<()> {
        let endpoint = if like { "like/like" } else { "like/removelike" };
        self.post(endpoint, json!({"target": {"videoId": video_id}}), "").map(|_| ())
    }

    /// Saves a playlist or album (by its playlist id) to the library, or removes it.
    pub fn save(&self, playlist_id: &str, save: bool) -> Result<()> {
        let endpoint = if save { "like/like" } else { "like/removelike" };
        self.post(endpoint, json!({"target": {"playlistId": playlist_id}}), "").map(|_| ())
    }

    /// Creates a private playlist; returns its id.
    pub fn create_playlist(&self, title: &str) -> Result<String> {
        let resp = self.post("playlist/create", json!({"title": title, "description": "", "privacyStatus": "PRIVATE"}), "")?;
        resp["playlistId"].as_str().map(str::to_string).ok_or_else(|| ApiError::Other("no playlist id returned".into()))
    }

    pub fn delete_playlist(&self, id: &str) -> Result<()> {
        self.post("playlist/delete", json!({"playlistId": id}), "").map(|_| ())
    }

    pub fn add_to_playlist(&self, id: &str, video_id: &str) -> Result<()> {
        self.edit_playlist(id, json!({"action": "ACTION_ADD_VIDEO", "addedVideoId": video_id, "dedupeOption": "DEDUPE_OPTION_SKIP"}))
    }

    /// Moves an entry to just before `before` (another entry), or to the end.
    pub fn move_in_playlist(&self, id: &str, set_video_id: &str, before: Option<&str>) -> Result<()> {
        let mut action = json!({"action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": set_video_id});
        if let Some(before) = before {
            action["movedSetVideoIdSuccessor"] = json!(before);
        }
        self.edit_playlist(id, action)
    }

    fn edit_playlist(&self, id: &str, action: Value) -> Result<()> {
        let resp = self.post("browse/edit_playlist", json!({"playlistId": id, "actions": [action]}), "")?;
        if parse::edit_succeeded(&resp) { Ok(()) } else { Err(ApiError::Other("YouTube rejected the change".into())) }
    }

    /// Radio / up-next queue seeded from a track.
    pub fn radio(&self, video_id: &str) -> Result<Vec<Track>> {
        let body = json!({
            "videoId": video_id,
            "playlistId": format!("RDAMVM{video_id}"),
            "isAudioOnly": true,
            "enablePersistentPlaylistPanel": true,
            "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {
                "hasPersistentPlaylistPanel": true, "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}},
        });
        let mut tracks = parse::tracks(&self.post("next", body, "")?);
        let mut seen = std::collections::HashSet::new();
        tracks.retain(|t| seen.insert(t.video_id.clone()));
        Ok(tracks)
    }
}

/// The one HTTP client of the app (API, art, stream lookups, audio), so
/// every part benefits from connections another part already opened.
/// ureq's defaults (128 KiB buffers ×2, 10 idle connections) cost ~3 MB;
/// our requests are small and responses are streamed, so smaller is enough.
/// Each connection holds two input and two output buffers (HTTP and TLS
/// layers): request heads stay under 4 KB (the cookie header is ~2.3 KB)
/// and response heads are a few KB.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(USER_AGENT)
        .input_buffer_size(16 * 1024)
        .output_buffer_size(8 * 1024)
        .max_idle_connections(8)
        .max_idle_connections_per_host(2)
        // Long enough that the next track's audio reuses a warm connection.
        .max_idle_age(Duration::from_secs(90))
        .build()
        .new_agent()
}

/// Today's date as YYYYMMDD (UTC), for the client version string.
fn yyyymmdd() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}")
}

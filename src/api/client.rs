//! InnerTube client: the same private JSON API the YouTube Music website uses.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::models::{Continuation, Playlist, Track};
use super::parse;
use crate::auth::session::{ORIGIN, Session};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// "Songs" filter for search (same value the website sends).
const SONGS_FILTER: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";

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
            .header("X-Goog-AuthUser", "0")
            .header("Cookie", self.session.cookie_header());
        if let Some(auth) = self.session.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp: Value = req.send_json(&body)?.body_mut().read_json()?;
        if parse::logged_in(&resp) == Some(false) {
            return Err(ApiError::LoggedOut);
        }
        Ok(resp)
    }

    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.post("search", json!({"query": query, "params": SONGS_FILTER}), "")?;
        Ok(parse::tracks(&resp))
    }

    pub fn library(&self) -> Result<Vec<Playlist>> {
        let resp = self.post("browse", json!({"browseId": "FEmusic_liked_playlists"}), "")?;
        Ok(parse::playlists(&resp))
    }

    /// One page of a playlist. Pass the returned continuation to get the next.
    pub fn playlist_page(&self, id: &str, cont: Option<&Continuation>) -> Result<(Vec<Track>, Option<Continuation>)> {
        let resp = match cont {
            None => self.post("browse", json!({"browseId": format!("VL{id}")}), "")?,
            Some(Continuation::Command(t)) => self.post("browse", json!({"continuation": t}), "")?,
            Some(Continuation::Next(t)) => self.post("browse", json!({}), &format!("&ctoken={t}&continuation={t}&type=next"))?,
        };
        let scope = parse::playlist_scope(&resp);
        Ok((parse::tracks(scope), parse::continuation(scope)))
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
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(USER_AGENT)
        .input_buffer_size(32 * 1024)
        .output_buffer_size(16 * 1024)
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

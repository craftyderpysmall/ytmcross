#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration: Option<String>,
    pub thumb: Option<String>,
    /// Identifies this entry within a playlist (needed to move it).
    pub set_video_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    /// Playlist id without the "VL" browse prefix.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub thumb: Option<String>,
}

/// Token for fetching the next page of a long list.
#[derive(Clone, Debug, PartialEq)]
pub enum Continuation {
    /// Newer style: sent in the request body as `continuation`.
    Command(String),
    /// Older style: sent as `ctoken`/`continuation` query parameters.
    Next(String),
}

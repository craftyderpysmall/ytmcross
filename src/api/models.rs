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
    /// Browse ids of the (first) artist and the album, for their pages.
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    /// Whether it's in Liked Music, when the response said.
    pub liked: Option<bool>,
    /// "388M plays" / "1.2M views", when listed.
    pub plays: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Playlist,
    Album,
}

/// A playlist or an album: anything with a track list.
#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    /// Playlist id without the "VL" browse prefix, or an album's browse id.
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub subtitle: String,
    pub thumb: Option<String>,
    /// The user's own playlist (editable, deletable).
    pub owned: bool,
    /// Playlist id that saves it to the library or removes it (albums have
    /// their own). None for the user's own playlists and auto playlists.
    pub save_id: Option<String>,
}

impl Playlist {
    pub fn browse_id(&self) -> String {
        match self.kind {
            Kind::Playlist => format!("VL{}", self.id),
            Kind::Album => self.id.clone(),
        }
    }
}

/// What a playlist or album page says about itself (first page only).
#[derive(Clone, Debug, Default)]
pub struct Header {
    /// "Album • 2001 • 14 songs • 1 hour, 1 minute".
    pub subtitle: String,
    /// Album artist and their browse id.
    pub artist: Option<(String, Option<String>)>,
    pub thumb: Option<String>,
    /// Playlist id that saves it to (or removes it from) the library, and
    /// whether it's saved now. None for the user's own playlists.
    pub save: Option<(String, bool)>,
    pub editable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Artist {
    pub name: String,
    pub subtitle: String,
    pub thumb: Option<String>,
    pub songs: Vec<Track>,
    pub albums: Vec<Playlist>,
    pub singles: Vec<Playlist>,
}

/// Token for fetching the next page of a long list.
#[derive(Clone, Debug, PartialEq)]
pub enum Continuation {
    /// Newer style: sent in the request body as `continuation`.
    Command(String),
    /// Older style: sent as `ctoken`/`continuation` query parameters.
    Next(String),
}

//! The only file that knows the shape of YouTube Music's JSON.
//!
//! Responses are deeply nested renderer trees whose wrapping changes often,
//! while the leaf renderers (one per list row) stay stable. So instead of
//! hard-coding paths we search the tree for the leaf renderers we know.
//! Every lookup is optional: a layout change drops rows, it never panics.

use std::ops::Range;

use serde_json::Value;

use super::models::{Artist, Continuation, Header, Kind, Playlist, Track};

/// Subtrees that duplicate rows we already collect (video counterparts of songs).
const SKIP_KEYS: &[&str] = &["counterpart"];

const ARTIST_PAGE: &str = "MUSIC_PAGE_TYPE_ARTIST";
const ALBUM_PAGE: &str = "MUSIC_PAGE_TYPE_ALBUM";

/// Collects every value stored under `key`, anywhere in the tree.
fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if SKIP_KEYS.contains(&k.as_str()) {
                    continue;
                }
                if k == key {
                    out.push(child);
                } else {
                    find_all(child, key, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| find_all(i, key, out)),
        _ => {}
    }
}

/// First value stored under `key`, depth-first.
fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|c| find(c, key))),
        Value::Array(items) => items.iter().find_map(|i| find(i, key)),
        _ => None,
    }
}

fn find_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    find(v, key)?.as_str()
}

/// Text of a `{runs: [...]}` or `{simpleText: ...}` node.
fn text(v: &Value) -> Option<String> {
    if let Some(s) = v["simpleText"].as_str() {
        return Some(s.to_string());
    }
    let runs = v["runs"].as_array()?;
    let s: String = runs.iter().filter_map(|r| r["text"].as_str()).collect();
    (!s.is_empty()).then_some(s)
}

/// Splits runs at " • " separators: [artist, " • ", album, " • ", 3:45] -> groups.
fn run_groups(v: &Value) -> Vec<String> {
    let mut groups = vec![String::new()];
    for run in v["runs"].as_array().into_iter().flatten() {
        let t = run["text"].as_str().unwrap_or_default();
        if t.trim() == "•" {
            groups.push(String::new());
        } else if let Some(last) = groups.last_mut() {
            last.push_str(t);
        }
    }
    groups.retain(|g| !g.trim().is_empty());
    groups
}

fn looks_like_duration(s: &str) -> bool {
    !s.is_empty() && s.split(':').count() >= 2 && s.split(':').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Largest thumbnail URL in the first `thumbnails` array under `v`.
fn thumb(v: &Value) -> Option<String> {
    let list = find(v, "thumbnails")?.as_array()?;
    list.last()?["url"].as_str().map(str::to_string)
}

/// Browse id and page type a node navigates to.
fn browse(v: &Value) -> Option<(&str, &str)> {
    let b = &v["navigationEndpoint"]["browseEndpoint"];
    let page = b["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"].as_str();
    Some((b["browseId"].as_str()?, page.unwrap_or_default()))
}

/// First run linking to a page of type `page`: (text, browse id).
fn link<'a>(runs: &[&'a Value], page: &str) -> Option<(&'a str, &'a str)> {
    runs.iter().find_map(|run| {
        let (id, p) = browse(run)?;
        (p == page).then(|| (run["text"].as_str().unwrap_or_default(), id))
    })
}

/// Finds each linked name (in order) in `artists`: its byte range there and
/// browse id. Names not found are left out.
fn artist_links<'a>(artists: &str, links: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<(Range<usize>, String)> {
    let mut from = 0;
    links
        .filter(|(name, _)| !name.trim().is_empty())
        .filter_map(|(name, id)| {
            let start = from + artists.get(from..)?.find(name)?;
            from = start + name.len();
            Some((start..from, id.to_string()))
        })
        .collect()
}

fn runs_of(v: &Value) -> impl Iterator<Item = &Value> {
    v["runs"].as_array().into_iter().flatten()
}

/// Like status from a row's menu: a like button, or the like action it
/// offers ("LIKE" when not liked yet, "INDIFFERENT" to un-like).
fn liked(menu: &Value) -> Option<bool> {
    if let Some(status) = find_str(menu, "likeStatus") {
        return Some(status == "LIKE");
    }
    match find(menu, "likeEndpoint")?["status"].as_str()? {
        "LIKE" => Some(false),
        "INDIFFERENT" => Some(true),
        _ => None,
    }
}

/// Rows from search results, playlists and the up-next queue.
pub fn tracks(resp: &Value) -> Vec<Track> {
    let mut nodes = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut nodes);
    let mut out: Vec<Track> = nodes.into_iter().filter_map(list_item).collect();

    let mut panel = Vec::new();
    find_all(resp, "playlistPanelVideoRenderer", &mut panel);
    out.extend(panel.into_iter().filter_map(panel_item));
    out
}

fn list_item(r: &Value) -> Option<Track> {
    let col = |i: usize| &r["flexColumns"][i]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let video_id = r["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| find_str(&r["overlay"], "videoId"))
        .or_else(|| find_str(col(0), "videoId"))?
        .to_string();
    let title = text(col(0))?;

    let mut groups = run_groups(col(1));
    groups.retain(|g| g != "Song" && g != "Video");
    let mut duration = groups.iter().position(|g| looks_like_duration(g)).map(|i| groups.remove(i));
    if duration.is_none() {
        duration = text(&r["fixedColumns"][0]["musicResponsiveListItemFixedColumnRenderer"]["text"]);
    }
    // Artist and album links can be in any column after the title.
    let runs: Vec<&Value> = r["flexColumns"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(1)
        .flat_map(|c| runs_of(&c["musicResponsiveListItemFlexColumnRenderer"]["text"]))
        .collect();
    let album_link = link(&runs, ALBUM_PAGE);
    let plays = runs.iter().filter_map(|r| r["text"].as_str()).find(|t| is_play_count(t)).map(str::to_string);
    groups.retain(|g| !is_play_count(g));
    let artists = groups.first().cloned().unwrap_or_default();
    let artist_runs = runs.iter().filter_map(|run| {
        let (id, page) = browse(run)?;
        (page == ARTIST_PAGE).then(|| (run["text"].as_str().unwrap_or_default(), id))
    });

    Some(Track {
        video_id,
        title,
        artist_links: artist_links(&artists, artist_runs),
        artists,
        album: album_link.map(|(t, _)| t.to_string()).or_else(|| groups.get(1).cloned()),
        duration,
        thumb: thumb(&r["thumbnail"]),
        set_video_id: r["playlistItemData"]["playlistSetVideoId"].as_str().map(str::to_string),
        album_id: album_link.map(|(_, id)| id.to_string()),
        liked: liked(&r["menu"]),
        plays,
    })
}

fn panel_item(r: &Value) -> Option<Track> {
    let groups = run_groups(&r["longBylineText"]);
    let runs: Vec<&Value> = runs_of(&r["longBylineText"]).collect();
    let album_link = link(&runs, ALBUM_PAGE);
    let artists = groups.first().cloned().unwrap_or_default();
    // Panel links carry no page type: artists are channels (UC…).
    let artist_runs = runs.iter().filter_map(|run| {
        let (id, _) = browse(run).filter(|(id, _)| id.starts_with("UC"))?;
        Some((run["text"].as_str().unwrap_or_default(), id))
    });
    Some(Track {
        video_id: r["videoId"].as_str()?.to_string(),
        title: text(&r["title"])?,
        artist_links: artist_links(&artists, artist_runs),
        artists,
        album: album_link.map(|(t, _)| t.to_string()).or_else(|| groups.get(1).cloned()),
        duration: text(&r["lengthText"]),
        thumb: thumb(&r["thumbnail"]),
        set_video_id: None,
        album_id: album_link.or_else(|| runs.iter().find_map(|run| browse(run).filter(|(id, _)| id.starts_with("MPRE")).map(|(id, _)| ("", id)))).map(|(_, id)| id.to_string()),
        liked: liked(&r["menu"]),
        plays: None,
    })
}

/// "388M plays", "1.2K views".
fn is_play_count(s: &str) -> bool {
    (s.ends_with(" plays") || s.ends_with(" views")) && s.starts_with(|c: char| c.is_ascii_digit())
}

/// "852M plays" -> 852_000_000.
pub fn play_count(s: &str) -> Option<u64> {
    let num = s.split(' ').next()?;
    let (digits, scale) = match num.chars().last()? {
        'K' => (&num[..num.len() - 1], 1e3),
        'M' => (&num[..num.len() - 1], 1e6),
        'B' => (&num[..num.len() - 1], 1e9),
        _ => (num, 1.0),
    };
    Some((digits.replace(',', "").parse::<f64>().ok()? * scale) as u64)
}

/// 1_934_000_000 -> "1.9B", the way YouTube abbreviates.
pub fn short_count(n: u64) -> String {
    let (v, unit) = match n {
        n if n >= 1_000_000_000 => (n as f64 / 1e9, "B"),
        n if n >= 1_000_000 => (n as f64 / 1e6, "M"),
        n if n >= 1_000 => (n as f64 / 1e3, "K"),
        n => return n.to_string(),
    };
    let s = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    format!("{}{unit}", s.trim_end_matches(".0"))
}

/// Playlists and albums: library and carousel cards, and search result rows.
pub fn playlists(resp: &Value) -> Vec<Playlist> {
    let mut cards = Vec::new();
    find_all(resp, "musicTwoRowItemRenderer", &mut cards);
    let mut out: Vec<Playlist> = cards.into_iter().filter_map(card).collect();
    let mut rows = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut rows);
    out.extend(rows.into_iter().filter_map(collection_row));
    out
}

/// "VLxyz" -> playlist xyz; "MPREb_…" -> album.
fn collection_id<'a>(id: &'a str, page: &str) -> Option<(&'a str, Kind)> {
    if let Some(id) = id.strip_prefix("VL") {
        Some((id, Kind::Playlist))
    } else if page == ALBUM_PAGE || id.starts_with("MPRE") {
        Some((id, Kind::Album))
    } else {
        None
    }
}

fn card(r: &Value) -> Option<Playlist> {
    let (id, page) = browse(r).or_else(|| runs_of(&r["title"]).find_map(browse))?;
    let (id, kind) = collection_id(id, page)?;
    Some(Playlist {
        id: id.to_string(),
        kind,
        title: text(&r["title"])?,
        subtitle: text(&r["subtitle"]).unwrap_or_default(),
        thumb: thumb(&r["thumbnailRenderer"]),
        // Only the owner's menu offers to edit it.
        owned: find(&r["menu"], "playlistEditorEndpoint").is_some(),
        save_id: save_id(r, &r["thumbnailOverlay"]),
    })
}

/// The playlist id a card's "Save to library" acts on; albums also name
/// it in their play button. Own and auto playlists have none.
fn save_id(r: &Value, overlay: &Value) -> Option<String> {
    let like = find(&r["menu"], "likeEndpoint").and_then(|l| l["target"]["playlistId"].as_str());
    let album = browse(r).is_some_and(|(id, _)| id.starts_with("MPRE"));
    like.or_else(|| if album { find_str(overlay, "playlistId") } else { None }).map(str::to_string)
}

fn collection_row(r: &Value) -> Option<Playlist> {
    let (id, page) = browse(r)?;
    let (id, kind) = collection_id(id, page)?;
    let col = |i: usize| &r["flexColumns"][i]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    Some(Playlist {
        id: id.to_string(),
        kind,
        title: text(col(0))?,
        subtitle: text(col(1)).unwrap_or_default(),
        thumb: thumb(&r["thumbnail"]),
        owned: false,
        save_id: save_id(r, &r["overlay"]),
    })
}

/// Header of a playlist or album page.
pub fn header(resp: &Value) -> Header {
    let editable = find(resp, "musicEditablePlaylistDetailHeaderRenderer").is_some();
    let Some(h) = find(resp, "musicResponsiveHeaderRenderer") else { return Header { editable, ..Header::default() } };
    let subtitle: Vec<String> = [text(&h["subtitle"]), text(&h["secondSubtitle"])].into_iter().flatten().collect();
    let artist = runs_of(&h["straplineTextOne"])
        .next()
        .and_then(|r| Some((r["text"].as_str()?.to_string(), browse(r).map(|(id, _)| id.to_string()))));
    let save = (!editable)
        .then(|| {
            h["buttons"].as_array()?.iter().find_map(|b| {
                let t = &b["toggleButtonRenderer"];
                let id = t["defaultServiceEndpoint"]["likeEndpoint"]["target"]["playlistId"].as_str()?;
                Some((id.to_string(), t["isToggled"].as_bool().unwrap_or(false)))
            })
        })
        .flatten();
    Header { subtitle: subtitle.join(" • "), artist, thumb: thumb(&h["thumbnail"]), save, editable }
}

/// An artist page: header, top songs, albums and singles.
pub fn artist(resp: &Value) -> Artist {
    let h = find(resp, "musicImmersiveHeaderRenderer").or_else(|| find(resp, "musicVisualHeaderRenderer")).unwrap_or(&Value::Null);
    let subtitle = text(&h["monthlyListenerCount"]).or_else(|| find(&h["subscriptionButton"], "longSubscriberCountText").and_then(text));
    let mut out = Artist {
        name: text(&h["title"]).unwrap_or_default(),
        subtitle: subtitle.unwrap_or_default(),
        thumb: thumb(&h["thumbnail"]),
        songs: find(resp, "musicShelfRenderer").map(tracks).unwrap_or_default(),
        ..Artist::default()
    };
    let mut shelves = Vec::new();
    find_all(resp, "musicCarouselShelfRenderer", &mut shelves);
    for shelf in shelves {
        let title = text(&shelf["header"]["musicCarouselShelfBasicHeaderRenderer"]["title"]).unwrap_or_default();
        let items = || shelf["contents"].as_array().into_iter().flatten().filter_map(|c| card(&c["musicTwoRowItemRenderer"]));
        match title.as_str() {
            "Albums" => out.albums = items().collect(),
            t if t.starts_with("Singles") => out.singles = items().collect(),
            _ => {}
        }
    }
    out
}

/// Whether a playlist edit (add/move) went through.
pub fn edit_succeeded(resp: &Value) -> bool {
    resp["status"].as_str() == Some("STATUS_SUCCEEDED")
}

/// For playlist and album pages: narrows the response to the track list
/// itself, so "suggestions" shelves and their continuations are ignored.
pub fn playlist_scope(resp: &Value) -> &Value {
    find(resp, "musicPlaylistShelfRenderer")
        .or_else(|| find(resp, "musicShelfRenderer"))
        .or_else(|| find(resp, "appendContinuationItemsAction"))
        .or_else(|| find(resp, "musicPlaylistShelfContinuation"))
        .unwrap_or(resp)
}

pub fn continuation(scope: &Value) -> Option<Continuation> {
    if let Some(cmd) = find(scope, "continuationCommand") {
        return cmd["token"].as_str().map(|t| Continuation::Command(t.to_string()));
    }
    find(scope, "nextContinuationData")?["continuation"].as_str().map(|t| Continuation::Next(t.to_string()))
}

/// `Some(false)` when YouTube says the request was made without a valid session.
pub fn logged_in(resp: &Value) -> Option<bool> {
    let mut params = Vec::new();
    find_all(resp, "params", &mut params);
    params
        .iter()
        .filter_map(|p| p.as_array())
        .flatten()
        .find(|p| p["key"] == "logged_in")
        .and_then(|p| p["value"].as_str())
        .map(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link_run(text: &str, id: &str, page: &str) -> Value {
        json!({"text": text, "navigationEndpoint": {"browseEndpoint": {"browseId": id,
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": page}}}}})
    }

    #[test]
    fn parses_search_row() {
        let resp = json!({"contents": {"x": [{"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "abc"},
            "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "s"}, {"url": "big"}]}}},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Title"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    link_run("Artist", "UC1", ARTIST_PAGE), {"text": " • "}, link_run("Album", "MPREb_1", ALBUM_PAGE),
                    {"text": " • "}, {"text": "3:45"}]}}}
            ],
            "menu": {"menuRenderer": {"items": [{"toggleMenuServiceItemRenderer": {
                "defaultServiceEndpoint": {"likeEndpoint": {"status": "INDIFFERENT"}}}}]}}}}]}});
        let t = &tracks(&resp)[0];
        assert_eq!(t.video_id, "abc");
        assert_eq!(t.title, "Title");
        assert_eq!(t.artists, "Artist");
        assert_eq!(t.album.as_deref(), Some("Album"));
        assert_eq!(t.duration.as_deref(), Some("3:45"));
        assert_eq!(t.thumb.as_deref(), Some("big"));
        assert_eq!(t.artist_links, vec![(0..6, "UC1".to_string())]);
        assert_eq!(t.album_id.as_deref(), Some("MPREb_1"));
        assert_eq!(t.liked, Some(true));
    }

    #[test]
    fn links_every_artist() {
        let resp = json!({"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "abc"},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Title"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    {"text": "Song"}, {"text": " • "}, link_run("A", "UC1", ARTIST_PAGE), {"text": ", "},
                    link_run("B", "UC2", ARTIST_PAGE), {"text": " & "}, link_run("A B", "UC3", ARTIST_PAGE),
                    {"text": " • "}, {"text": "3:45"}]}}}
            ]}});
        let t = &tracks(&resp)[0];
        assert_eq!(t.artists, "A, B & A B");
        let ids: Vec<_> = t.artist_links.iter().map(|(r, id)| (&t.artists[r.clone()], id.as_str())).collect();
        assert_eq!(ids, [("A", "UC1"), ("B", "UC2"), ("A B", "UC3")]);
    }

    #[test]
    fn play_counts_round_trip() {
        assert_eq!(play_count("852M plays"), Some(852_000_000));
        assert_eq!(play_count("1.2B plays"), Some(1_200_000_000));
        assert_eq!(play_count("126 plays"), Some(126));
        assert_eq!(short_count(1_934_000_000), "1.9B");
        assert_eq!(short_count(852_000_000), "852M");
        assert_eq!(short_count(2_000_000), "2M");
        assert_eq!(short_count(126), "126");
    }

    #[test]
    fn skips_rows_without_video() {
        let resp = json!({"musicResponsiveListItemRenderer": {"flexColumns": []}});
        assert!(tracks(&resp).is_empty());
    }

    #[test]
    fn library_cards_know_kind_and_owner() {
        let resp = json!({"items": [
            {"musicTwoRowItemRenderer": {"title": {"runs": [{"text": "Mine"}]},
                "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPL1"}},
                "menu": {"menuRenderer": {"items": [{"menuNavigationItemRenderer": {"navigationEndpoint": {"playlistEditorEndpoint": {}}}}]}}}},
            {"musicTwoRowItemRenderer": {"title": {"runs": [{"text": "Disc"}]},
                "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREb_2"}}}},
            {"musicTwoRowItemRenderer": {"title": {"runs": [{"text": "New playlist"}]}}}
        ]});
        let list = playlists(&resp);
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].id.as_str(), list[0].kind, list[0].owned), ("PL1", Kind::Playlist, true));
        assert_eq!((list[1].id.as_str(), list[1].kind, list[1].owned), ("MPREb_2", Kind::Album, false));
    }

    #[test]
    fn reads_login_state() {
        let resp = json!({"responseContext": {"serviceTrackingParams": [{"params": [{"key": "logged_in", "value": "0"}]}]}});
        assert_eq!(logged_in(&resp), Some(false));
    }
}

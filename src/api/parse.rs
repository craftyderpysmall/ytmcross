//! The only file that knows the shape of YouTube Music's JSON.
//!
//! Responses are deeply nested renderer trees whose wrapping changes often,
//! while the leaf renderers (one per list row) stay stable. So instead of
//! hard-coding paths we search the tree for the leaf renderers we know.
//! Every lookup is optional: a layout change drops rows, it never panics.

use serde_json::Value;

use super::models::{Continuation, Playlist, Track};

/// Subtrees that duplicate rows we already collect (video counterparts of songs).
const SKIP_KEYS: &[&str] = &["counterpart"];

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
    let artists = groups.first().cloned().unwrap_or_default();
    // Search rows carry the album in column 1; playlist rows in column 2.
    let album = groups.get(1).cloned().or_else(|| text(col(2)));

    Some(Track { video_id, title, artists, album, duration, thumb: thumb(&r["thumbnail"]) })
}

fn panel_item(r: &Value) -> Option<Track> {
    let groups = run_groups(&r["longBylineText"]);
    Some(Track {
        video_id: r["videoId"].as_str()?.to_string(),
        title: text(&r["title"])?,
        artists: groups.first().cloned().unwrap_or_default(),
        album: groups.get(1).cloned(),
        duration: text(&r["lengthText"]),
        thumb: thumb(&r["thumbnail"]),
    })
}

/// Playlist cards from the library page.
pub fn playlists(resp: &Value) -> Vec<Playlist> {
    let mut nodes = Vec::new();
    find_all(resp, "musicTwoRowItemRenderer", &mut nodes);
    nodes
        .into_iter()
        .filter_map(|r| {
            let browse_id = find_str(&r["navigationEndpoint"], "browseId").or_else(|| find_str(&r["title"], "browseId"))?;
            let id = browse_id.strip_prefix("VL")?.to_string();
            Some(Playlist {
                id,
                title: text(&r["title"])?,
                subtitle: text(&r["subtitle"]).unwrap_or_default(),
                thumb: thumb(&r["thumbnailRenderer"]),
            })
        })
        .collect()
}

/// For playlist pages: narrows the response to the track list itself, so
/// "suggestions" shelves and their continuations are ignored.
pub fn playlist_scope(resp: &Value) -> &Value {
    find(resp, "musicPlaylistShelfRenderer")
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

    #[test]
    fn parses_search_row() {
        let resp = json!({"contents": {"x": [{"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "abc"},
            "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "s"}, {"url": "big"}]}}},
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Title"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    {"text": "Artist"}, {"text": " • "}, {"text": "Album"}, {"text": " • "}, {"text": "3:45"}]}}}
            ]}}]}});
        let t = &tracks(&resp)[0];
        assert_eq!(t.video_id, "abc");
        assert_eq!(t.title, "Title");
        assert_eq!(t.artists, "Artist");
        assert_eq!(t.album.as_deref(), Some("Album"));
        assert_eq!(t.duration.as_deref(), Some("3:45"));
        assert_eq!(t.thumb.as_deref(), Some("big"));
    }

    #[test]
    fn skips_rows_without_video() {
        let resp = json!({"musicResponsiveListItemRenderer": {"flexColumns": []}});
        assert!(tracks(&resp).is_empty());
    }

    #[test]
    fn reads_login_state() {
        let resp = json!({"responseContext": {"serviceTrackingParams": [{"params": [{"key": "logged_in", "value": "0"}]}]}});
        assert_eq!(logged_in(&resp), Some(false));
    }
}

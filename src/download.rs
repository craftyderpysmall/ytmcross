//! Downloads. Songs are saved the way YouTube serves them (AAC in MP4, no
//! re-encoding) with title/artist/album tags, as "Artist - Title [id].m4a",
//! all in one folder. A downloaded playlist or album is an .m3u8 listing
//! its songs ("Title [id].m3u8"), so a song is never stored twice.
//!
//! The folder is the record: it is read once at startup (and when another
//! folder is chosen), so a deleted file just isn't downloaded any more.
//! Downloaded songs play from disk.
//!
//! One worker thread, only while there is work (pool.rs). Songs are written
//! to ".part" files and renamed once complete.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::api::client::{ApiError, Client};
use crate::api::models::{Playlist, Track};
use crate::auth::session::Session;
use crate::pool::Pool;
use crate::stream::Resolver;
use crate::worker::{Notifier, Response};
use crate::{paths, player};

/// Bytes per range request: a song in one or two requests, while staying
/// under the size googlevideo throttles.
const CHUNK: u64 = 4 << 20;
/// The start of a file, fetched first: its moov box (where tags go) is in it.
const HEAD: u64 = 64 << 10;
/// Songs resolved per solver load.
const BATCH: usize = 6;
const RETRIES: u32 = 3;
/// File names stay well under the usual 255-byte limit.
const MAX_NAME: usize = 150;

enum Job {
    /// Reads what a folder holds.
    Scan(PathBuf),
    Song(Track),
    /// A playlist or album, with its tracks if they are already loaded.
    List(Playlist, Option<Vec<Track>>),
    /// Deletes a playlist's or album's download.
    Remove(Playlist),
}

#[derive(Default)]
struct State {
    dir: PathBuf,
    /// Downloaded songs: video id -> file name.
    songs: HashMap<String, String>,
    /// Downloaded playlists and albums.
    lists: HashSet<String>,
    /// Songs and lists waiting or in progress: id -> (done, total).
    active: HashMap<String, (usize, usize)>,
}

struct Shared {
    state: Mutex<State>,
    resolver: Resolver,
    agent: ureq::Agent,
    out: Notifier,
}

pub struct Downloads {
    shared: Arc<Shared>,
    jobs: Pool<Job>,
}

impl Downloads {
    pub fn new(dir: PathBuf, resolver: Resolver, agent: ureq::Agent, out: Notifier) -> Downloads {
        let state = Mutex::new(State { dir: dir.clone(), ..State::default() });
        let shared = Arc::new(Shared { state, resolver, agent, out });
        let worker = shared.clone();
        let jobs = Pool::new("download", 1, move |job, _: &Pool<Job>| worker.run(job));
        // No folder yet (nothing downloaded): no thread needed.
        if dir.is_dir() {
            jobs.push(Job::Scan(dir));
        }
        Downloads { shared, jobs }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }

    pub fn dir(&self) -> PathBuf {
        self.state().dir.clone()
    }

    /// Switches to another folder and reads what it holds.
    pub fn set_dir(&self, dir: PathBuf) {
        let mut s = self.state();
        if s.dir != dir {
            s.dir = dir.clone();
            s.songs.clear();
            s.lists.clear();
            drop(s);
            self.jobs.push(Job::Scan(dir));
        }
    }

    /// The downloaded file of a song.
    pub fn file(&self, video_id: &str) -> Option<PathBuf> {
        let s = self.state();
        s.songs.get(video_id).map(|name| s.dir.join(name))
    }

    pub fn has(&self, video_id: &str) -> bool {
        self.state().songs.contains_key(video_id)
    }

    pub fn has_list(&self, id: &str) -> bool {
        self.state().lists.contains(id)
    }

    /// (done, total) while a song or list is waiting or downloading; total
    /// is 0 until a list's tracks are known.
    pub fn progress(&self, id: &str) -> Option<(usize, usize)> {
        self.state().active.get(id).copied()
    }

    pub fn song(&self, track: Track) {
        let mut s = self.state();
        if !s.songs.contains_key(&track.video_id) && !s.active.contains_key(&track.video_id) {
            s.active.insert(track.video_id.clone(), (0, 1));
            drop(s);
            self.jobs.push(Job::Song(track));
        }
    }

    /// Downloads a playlist's or album's songs. `tracks` are its loaded
    /// tracks; None fetches them.
    pub fn list(&self, list: Playlist, tracks: Option<Vec<Track>>) {
        let mut s = self.state();
        if !s.active.contains_key(&list.id) {
            s.active.insert(list.id.clone(), (0, 0));
            drop(s);
            self.jobs.push(Job::List(list, tracks));
        }
    }

    /// Deletes a downloaded playlist or album: its .m3u8, and its songs that
    /// no other downloaded list uses.
    pub fn remove_list(&self, list: Playlist) {
        self.jobs.push(Job::Remove(list));
    }
}

impl Shared {
    fn run(&self, job: Job) {
        match job {
            Job::Scan(dir) => {
                let (songs, lists) = scan(&dir);
                let mut s = self.state.lock().unwrap();
                if s.dir == dir && !(songs.is_empty() && lists.is_empty()) {
                    (s.songs, s.lists) = (songs, lists);
                    drop(s);
                    self.out.send(Response::Download(None));
                }
            }
            Job::Song(track) => {
                let dir = self.state.lock().unwrap().dir.clone();
                let (failed, error) = self.save_all(&dir, std::slice::from_ref(&track), None);
                self.state.lock().unwrap().active.remove(&track.video_id);
                let message = match error.filter(|_| failed > 0) {
                    None => format!("Downloaded \u{201c}{}\u{201d}", track.title),
                    Some(e) => format!("Couldn't download \u{201c}{}\u{201d}: {e}", track.title),
                };
                self.out.send(Response::Download(Some(message)));
            }
            Job::List(list, tracks) => {
                let message = self.download_list(&list, tracks);
                self.state.lock().unwrap().active.remove(&list.id);
                self.out.send(Response::Download(Some(message)));
            }
            Job::Remove(list) => {
                let message = match self.remove_list(&list) {
                    Ok(()) => format!("Removed the download of \u{201c}{}\u{201d}", list.title),
                    Err(e) => format!("Couldn't remove the download of \u{201c}{}\u{201d}: {e}", list.title),
                };
                self.out.send(Response::Download(Some(message)));
            }
        }
    }

    fn remove_list(&self, list: &Playlist) -> io::Result<()> {
        let dir = self.state.lock().unwrap().dir.clone();
        // Song files of this list, and those other lists still use.
        let (mut own, mut kept) = (HashSet::new(), HashSet::new());
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| file_id(n, ".m3u8")) else { continue };
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let files = playlist_files(&text).map(str::to_string);
            if id == list.id {
                own.extend(files);
                std::fs::remove_file(entry.path())?;
            } else {
                kept.extend(files);
            }
        }
        // Only our own song files in this folder, never a path elsewhere
        // (the .m3u8 may have been edited).
        own.retain(|f: &String| !kept.contains(f) && !f.contains(['/', '\\']) && file_id(f, ".m4a").is_some());
        for file in &own {
            let _ = std::fs::remove_file(dir.join(file));
        }
        let mut s = self.state.lock().unwrap();
        if s.dir == dir {
            s.lists.remove(&list.id);
            s.songs.retain(|_, name| !own.contains(name));
        }
        Ok(())
    }

    fn download_list(&self, list: &Playlist, tracks: Option<Vec<Track>>) -> String {
        let title = &list.title;
        let tracks = match tracks.map_or_else(|| self.fetch_tracks(list), Ok) {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => return format!("\u{201c}{title}\u{201d} has no songs"),
            Err(e) => return format!("Couldn't download \u{201c}{title}\u{201d}: {e}"),
        };
        let dir = self.state.lock().unwrap().dir.clone();
        let (failed, error) = self.save_all(&dir, &tracks, Some(&list.id));
        match self.write_playlist(&dir, list, &tracks) {
            Err(e) => format!("Couldn't save \u{201c}{title}\u{201d}: {e}"),
            Ok(_) if failed == 0 => format!("Downloaded \u{201c}{title}\u{201d}"),
            Ok(saved) => format!(
                "Downloaded {saved} of {} songs of \u{201c}{title}\u{201d}: {}",
                saved + failed,
                error.unwrap_or_default()
            ),
        }
    }

    /// Every track of a playlist or album, page by page.
    fn fetch_tracks(&self, list: &Playlist) -> Result<Vec<Track>, String> {
        let err = |e: ApiError| match e {
            ApiError::LoggedOut => "signed out".to_string(),
            ApiError::Other(m) => m,
        };
        let client = Client::new(self.agent.clone(), Session::load(&paths::cookies_file()));
        let mut page = client.playlist_page(list, None).map_err(err)?;
        let mut tracks = std::mem::take(&mut page.tracks);
        let mut next = page.next.filter(|_| !tracks.is_empty());
        while let Some(cont) = next {
            let page = client.playlist_page(list, Some(&cont)).map_err(err)?;
            next = page.next.filter(|_| !page.tracks.is_empty());
            tracks.extend(page.tracks);
        }
        Ok(tracks)
    }

    /// Downloads the tracks not on disk yet, reporting progress under
    /// `list`. Returns how many failed, and the last error.
    fn save_all(&self, dir: &Path, tracks: &[Track], list: Option<&str>) -> (usize, Option<String>) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return (tracks.len(), Some(e.to_string()));
        }
        let mut seen = HashSet::new();
        let todo: Vec<&Track> = {
            let s = self.state.lock().unwrap();
            tracks.iter().filter(|t| !s.songs.contains_key(&t.video_id) && seen.insert(t.video_id.as_str())).collect()
        };
        let total = tracks.len();
        let mut done = total - todo.len();
        let bump = |done: usize| {
            if let Some(id) = list {
                self.state.lock().unwrap().active.insert(id.to_string(), (done, total));
                self.out.send(Response::Download(None));
            }
        };
        bump(done);
        let (mut failed, mut error) = (0, None);
        for batch in todo.chunks(BATCH) {
            let ids: Vec<String> = batch.iter().map(|t| t.video_id.clone()).collect();
            let urls = self.resolver.resolve_now(&ids);
            for track in batch {
                let url = urls.iter().find(|(id, _)| *id == track.video_id).map_or(Err("no result".into()), |(_, r)| r.clone());
                let mut result = url.and_then(|u| self.save(dir, track, &u));
                // Refused: the solver is stale. Rebuild it and try once more.
                if result.as_ref().is_err_and(|e| e.contains("403")) {
                    self.resolver.refused();
                    let retry = self.resolver.resolve_now(std::slice::from_ref(&track.video_id)).pop();
                    result = retry.map_or(Err("no result".into()), |(_, r)| r).and_then(|u| self.save(dir, track, &u));
                }
                match result {
                    Ok(name) => {
                        let mut s = self.state.lock().unwrap();
                        // Recorded only if that's still the folder in use.
                        if s.dir == dir {
                            s.songs.insert(track.video_id.clone(), name);
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        error = Some(e);
                    }
                }
                done += 1;
                bump(done);
            }
        }
        (failed, error)
    }

    /// Downloads one song from its stream URL; returns its file name.
    fn save(&self, dir: &Path, track: &Track, url: &str) -> Result<String, String> {
        let name = format!("{} [{}].m4a", clean(&track_name(track)), clean(&track.video_id));
        let part = dir.join(format!("{name}.part"));
        let result = write_song(&self.agent, url, &part, track).and_then(|()| std::fs::rename(&part, dir.join(&name)));
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            return Err(e.to_string());
        }
        Ok(name)
    }

    /// Writes "Title [id].m3u8" with the list's songs that are on disk;
    /// returns how many.
    fn write_playlist(&self, dir: &Path, list: &Playlist, tracks: &[Track]) -> io::Result<usize> {
        let mut text = String::from("#EXTM3U\n");
        let mut count = 0;
        {
            let s = self.state.lock().unwrap();
            for t in tracks {
                if let Some(file) = s.songs.get(&t.video_id) {
                    let secs = t.seconds();
                    let secs = if secs > 0.0 { secs.round() as i64 } else { -1 };
                    text.push_str(&format!("#EXTINF:{secs},{}\n{file}\n", track_name(t)));
                    count += 1;
                }
            }
        }
        if count > 0 {
            std::fs::write(dir.join(format!("{} [{}].m3u8", clean(&list.title), clean(&list.id))), text)?;
            let mut s = self.state.lock().unwrap();
            if s.dir == dir {
                s.lists.insert(list.id.clone());
            }
        }
        Ok(count)
    }
}

/// The songs and lists in a folder; a list counts only while all its songs
/// are there. Leftovers of interrupted downloads are deleted (no download
/// runs while a folder is read).
fn scan(dir: &Path) -> (HashMap<String, String>, HashSet<String>) {
    let (mut songs, mut lists) = (HashMap::new(), HashSet::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (songs, lists) };
    let mut playlists = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".m4a.part") {
            let _ = std::fs::remove_file(entry.path());
        } else if let Some(id) = file_id(name, ".m4a") {
            songs.insert(id.to_string(), name.to_string());
        } else if let Some(id) = file_id(name, ".m3u8") {
            playlists.push((id.to_string(), entry.path()));
        }
    }
    let present: HashSet<&str> = songs.values().map(String::as_str).collect();
    for (id, path) in playlists {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if playlist_files(&text).all(|f| present.contains(f)) {
            lists.insert(id);
        }
    }
    (songs, lists)
}

/// The files an .m3u8 lists.
fn playlist_files(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// The id in "… [id].ext".
fn file_id<'a>(name: &'a str, ext: &str) -> Option<&'a str> {
    let stem = name.strip_suffix(ext)?.strip_suffix(']')?;
    let id = &stem[stem.rfind('[')? + 1..];
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then_some(id)
}

/// "Artist - Title".
fn track_name(t: &Track) -> String {
    if t.artists.is_empty() { t.title.clone() } else { format!("{} - {}", t.artists, t.title) }
}

/// Safe as a file name on every OS (and on FAT drives).
fn clean(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    if out.len() > MAX_NAME {
        let mut cut = MAX_NAME;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    let out = out.trim().trim_end_matches('.');
    if out.is_empty() { "Untitled".into() } else { out.to_string() }
}

fn write_song(agent: &ureq::Agent, url: &str, path: &Path, track: &Track) -> io::Result<()> {
    let len = player::stream_len(agent, url)?;
    let head_len = HEAD.min(len);
    let mut head = Cursor::new(Vec::with_capacity(head_len as usize));
    get_range(agent, url, 0, head_len, &mut head)?;
    let mut head = head.into_inner();
    add_tags(&mut head, track);
    let mut file = BufWriter::with_capacity(64 << 10, File::create(path)?);
    file.write_all(&head)?;
    let mut at = head_len;
    while at < len {
        let end = (at + CHUNK).min(len);
        get_range(agent, url, at, end, &mut file)?;
        at = end;
    }
    file.into_inner().map_err(|e| e.into_error())?.sync_all()
}

/// Appends bytes `start..end` of `url` to `w`, with retries; a 403 (refused
/// URL) fails at once.
fn get_range<W: Write + Seek>(agent: &ureq::Agent, url: &str, start: u64, end: u64, w: &mut W) -> io::Result<()> {
    let want = end - start;
    let from = w.stream_position()?;
    let mut last = io::Error::other("no attempt");
    for attempt in 0..RETRIES {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
            w.seek(SeekFrom::Start(from))?;
        }
        let response = agent.get(url).header("Range", format!("bytes={start}-{}", end - 1)).call();
        let copied = match response {
            Ok(mut r) => io::copy(&mut r.body_mut().with_config().limit(want + 1).reader(), w),
            Err(ureq::Error::StatusCode(403)) => return Err(io::Error::other("stream refused (403)")),
            Err(e) => Err(io::Error::other(e)),
        };
        match copied {
            Ok(n) if n == want => return Ok(()),
            Ok(n) => last = io::Error::other(format!("stream ended after {n} of {want} bytes")),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Adds iTunes-style tags (title, artist, album) to the moov box: into its
/// moov/udta/meta/ilst list (YouTube's files have one, naming the encoder),
/// or as a new one. Everything after moov moves by the same amount, which
/// fragmented MP4 allows: fragments are found relative to the sidx box and
/// to their own moof.
fn add_tags(head: &mut Vec<u8>, track: &Track) {
    let Some(moov) = find_box(head, 0, head.len(), b"moov") else { return };
    let mut items = Vec::new();
    let album = track.album.as_deref().unwrap_or_default();
    for (key, value) in [(b"\xa9nam", track.title.as_str()), (b"\xa9ART", &track.artists), (b"\xa9alb", album)] {
        if !value.is_empty() {
            // A "data" box: type 1 (UTF-8 text), locale 0.
            let data = mp4_box(b"data", &[&[0, 0, 0, 1, 0, 0, 0, 0], value.as_bytes()].concat());
            items.extend(mp4_box(key, &data));
        }
    }
    let inside = |(at, size): (usize, usize), skip: usize, kind| find_box(head, at + 8 + skip, at + size, kind);
    let udta = inside(moov, 0, b"udta");
    let meta = udta.and_then(|u| inside(u, 0, b"meta"));
    let ilst = meta.and_then(|m| inside(m, 4, b"ilst")); // meta has version/flags first
    let (insert_at, added, grown) = match (udta, meta, ilst) {
        (Some(u), Some(m), Some(i)) => (i.0 + i.1, items, vec![moov, u, m, i]),
        _ => {
            // Version/flags, pre-defined, handler "mdir", reserved ("appl", 0, 0), empty name.
            let hdlr = mp4_box(b"hdlr", &[&[0u8; 8][..], b"mdirappl", &[0; 9]].concat());
            let meta = mp4_box(b"meta", &[&[0u8; 4][..], &hdlr, &mp4_box(b"ilst", &items)].concat());
            (moov.0 + moov.1, mp4_box(b"udta", &meta), vec![moov])
        }
    };
    // Sizes stay small: they are all inside the fetched head.
    for (at, size) in grown {
        head[at..at + 4].copy_from_slice(&((size + added.len()) as u32).to_be_bytes());
    }
    head.splice(insert_at..insert_at, added);
}

/// A box lying wholly inside `data[from..to]`, among boxes starting at
/// `from`: (offset, size).
fn find_box(data: &[u8], from: usize, to: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    let mut at = from;
    while at + 8 <= to {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        if size < 8 {
            return None; // 64-bit or open-ended: not one we look for
        }
        if &data[at + 4..at + 8] == kind {
            return (at + size <= to).then_some((at, size));
        }
        at += size;
    }
    None
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend(((8 + body.len()) as u32).to_be_bytes());
    out.extend(kind);
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            video_id: "abc-_123XYZ".into(),
            title: "So/What?".into(),
            artists: "Miles Davis".into(),
            album: Some("Kind of Blue".into()),
            duration: Some("9:22".into()),
            thumb: None,
            set_video_id: None,
            artist_id: None,
            album_id: None,
            liked: None,
            plays: None,
        }
    }

    #[test]
    fn scan_counts_only_complete_lists() {
        let dir = std::env::temp_dir().join(format!("ytmcross-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write("A - One [aaaaaaaaaaa].m4a", "");
        write("A - Two [bbbbbbbbbbb].m4a.part", "");
        write("Full [PL1].m3u8", "#EXTM3U\n#EXTINF:1,A - One\nA - One [aaaaaaaaaaa].m4a\n");
        write("Partial [PL2].m3u8", "#EXTM3U\nA - One [aaaaaaaaaaa].m4a\nA - Two [bbbbbbbbbbb].m4a\n");
        let (songs, lists) = scan(&dir);
        let leftover = dir.join("A - Two [bbbbbbbbbbb].m4a.part").exists();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(songs.get("aaaaaaaaaaa").map(String::as_str), Some("A - One [aaaaaaaaaaa].m4a"));
        assert_eq!(songs.len(), 1);
        assert_eq!(lists, HashSet::from(["PL1".to_string()]));
        assert!(!leftover);
    }

    #[test]
    fn names_are_safe_and_carry_the_id() {
        assert_eq!(clean(&track_name(&track())), "Miles Davis - So_What_");
        assert_eq!(clean(" dots... "), "dots");
        assert_eq!(clean("é".repeat(100).as_str()).len(), MAX_NAME);
        assert_eq!(file_id("Miles Davis - So_What_ [abc-_123XYZ].m4a", ".m4a"), Some("abc-_123XYZ"));
        assert_eq!(file_id("Mix [live] [PL1].m3u8", ".m3u8"), Some("PL1"));
        assert_eq!(file_id("notes.m4a", ".m4a"), None);
        assert_eq!(file_id("a [b c].m4a", ".m4a"), None);
    }

    /// Tags in `head`, and checks the boxes around moov are intact.
    fn tagged(moov_body: &[u8]) -> Vec<u8> {
        let moov = mp4_box(b"moov", moov_body);
        let mut head = [mp4_box(b"ftyp", b"dash"), moov.clone(), mp4_box(b"sidx", &[7; 20])].concat();
        add_tags(&mut head, &track());
        let (at, size) = find_box(&head, 0, head.len(), b"moov").unwrap();
        assert!(size > moov.len());
        let (sidx, _) = find_box(&head, 0, head.len(), b"sidx").unwrap();
        assert_eq!(sidx, at + size);
        assert_eq!(head[sidx + 8..], [7; 20]);
        head[at..at + size].to_vec()
    }

    #[test]
    fn tags_go_into_a_new_list() {
        let moov = tagged(&mp4_box(b"mvhd", &[0; 12]));
        assert!(moov.windows(4).any(|w| w == b"ilst") && moov.windows(12).any(|w| w == b"Kind of Blue"));
    }

    #[test]
    fn tags_join_an_existing_list() {
        // Like YouTube's: udta/meta/ilst naming the encoder.
        let too = mp4_box(b"\xa9too", &mp4_box(b"data", b"\0\0\0\x01\0\0\0\0Google"));
        let meta = mp4_box(b"meta", &[&[0u8; 4][..], &mp4_box(b"hdlr", &[0; 25]), &mp4_box(b"ilst", &too)].concat());
        let moov = tagged(&[mp4_box(b"mvhd", &[0; 12]), mp4_box(b"udta", &meta)].concat());
        assert_eq!(moov.windows(4).filter(|w| *w == b"udta").count(), 1);
        let (u, us) = find_box(&moov, 8, moov.len(), b"udta").unwrap();
        let (m, ms) = find_box(&moov, u + 8, u + us, b"meta").unwrap();
        let (i, is) = find_box(&moov, m + 12, m + ms, b"ilst").unwrap();
        assert_eq!((u + us, m + ms, i + is), (moov.len(), moov.len(), moov.len()));
        let list = &moov[i..i + is];
        assert!(list.windows(6).any(|w| w == b"Google") && list.windows(11).any(|w| w == b"Miles Davis"));
    }
}

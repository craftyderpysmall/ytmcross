//! A seekable byte stream over HTTP range requests, for symphonia.
//!
//! googlevideo URLs carry their size (`clen=`), so no extra request is
//! needed up front. Data is fetched in 256 KiB chunks (~8 s of 256 kbps
//! audio) over one kept-alive connection; only the current chunk is held.
//! To start mid-track, the stream can be spliced: the file's head, then
//! straight on from the fragment to play (see index.rs).

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use super::index::FragmentIndex;

const CHUNK: u64 = 256 * 1024;
const RETRIES: u32 = 3;
/// Total bytes of preloaded track starts kept in memory.
const PRELOAD_BUDGET: usize = 1280 * 1024;
/// How long opening a stream waits for a preload already in flight.
const JOIN_WAIT: Duration = Duration::from_secs(2);

enum Slot {
    /// Being downloaded right now: wait for it rather than fetch twice.
    Loading,
    Ready(Vec<u8>),
}

/// Starts of tracks that are likely to be played, keyed by URL, newest first.
/// Opening one of these streams needs no network at all.
static PRELOADED: Mutex<VecDeque<(String, Slot)>> = Mutex::new(VecDeque::new());
static PRELOAD_DONE: Condvar = Condvar::new();

/// Drops every preloaded start (e.g. after switching accounts).
pub fn clear_preloaded() {
    PRELOADED.lock().unwrap().retain(|(_, slot)| matches!(slot, Slot::Loading));
}

pub fn is_preloaded(url: &str) -> bool {
    PRELOADED.lock().unwrap().iter().any(|(u, _)| u == url)
}

/// Downloads the first `bytes` of `url` into the preload cache. The error
/// mentions "403" when YouTube refused the URL.
pub fn preload_first_chunk(agent: &ureq::Agent, url: &str, bytes: u64) -> Result<(), String> {
    {
        let mut cache = PRELOADED.lock().unwrap();
        if cache.iter().any(|(u, _)| u == url) {
            return Ok(());
        }
        cache.push_front((url.to_string(), Slot::Loading));
    }
    let len = content_length_from_url(url).unwrap_or(bytes);
    let fetched = fetch_range(agent, url, 0, bytes.min(len).max(1) - 1);
    let mut cache = PRELOADED.lock().unwrap();
    let i = cache.iter().position(|(u, _)| u == url);
    let result = match (fetched, i) {
        (Ok(data), Some(i)) => {
            cache[i].1 = Slot::Ready(data);
            Ok(())
        }
        (Ok(_), None) => Ok(()),
        (Err(e), i) => {
            if let Some(i) = i {
                cache.remove(i);
            }
            Err(e.to_string())
        }
    };
    // Evict the oldest finished entries beyond the budget.
    let mut total = 0;
    cache.retain(|(_, slot)| match slot {
        Slot::Loading => true,
        Slot::Ready(b) => {
            total += b.len();
            total <= PRELOAD_BUDGET
        }
    });
    drop(cache);
    PRELOAD_DONE.notify_all();
    result
}

/// Takes a preloaded start, waiting briefly if it is still downloading.
fn take_preloaded(url: &str) -> Option<Vec<u8>> {
    let deadline = Instant::now() + JOIN_WAIT;
    let mut cache = PRELOADED.lock().unwrap();
    loop {
        let i = cache.iter().position(|(u, _)| u == url)?;
        if matches!(cache[i].1, Slot::Ready(_)) {
            return match cache.remove(i) {
                Some((_, Slot::Ready(bytes))) => Some(bytes),
                _ => None,
            };
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        cache = PRELOAD_DONE.wait_timeout(cache, deadline - now).unwrap().0;
    }
}

/// GET `start..=end` with retries; a 403 (expired or refused URL) fails at once.
fn fetch_range(agent: &ureq::Agent, url: &str, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut last = io::Error::other("no attempt");
    for attempt in 0..RETRIES {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(300 * u64::from(attempt)));
        }
        let result = agent
            .get(url)
            .header("Range", format!("bytes={start}-{end}"))
            .call()
            .and_then(|mut r| r.body_mut().with_config().limit(end - start + 2).read_to_vec());
        match result {
            Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
            Ok(_) => last = io::Error::other("empty response"),
            Err(ureq::Error::StatusCode(403)) => return Err(io::Error::other("stream URL refused (403)")),
            Err(e) => last = io::Error::other(e),
        }
    }
    Err(last)
}

pub struct HttpSource {
    agent: ureq::Agent,
    url: String,
    /// Length of the file.
    len: u64,
    /// Read position in the stream the demuxer sees.
    pos: u64,
    chunk: Vec<u8>,
    /// File offset of `chunk`.
    chunk_start: u64,
    /// Starting at a later fragment: the stream is the file's head (up to
    /// its first fragment), then the file from this offset on.
    splice: Option<(Vec<u8>, u64)>,
}

impl HttpSource {
    pub fn open(agent: ureq::Agent, url: &str) -> io::Result<HttpSource> {
        let len = match content_length_from_url(url) {
            Some(len) => len,
            None => content_length_from_server(&agent, url)?,
        };
        let chunk = take_preloaded(url).unwrap_or_default();
        crate::trace!("source: open, {} preloaded bytes", chunk.len());
        Ok(HttpSource { agent, url: url.to_string(), len, pos: 0, chunk, chunk_start: 0, splice: None })
    }

    /// The file's fragment index, from its first chunk (fetched unless preloaded).
    pub fn fragment_index(&mut self) -> Option<FragmentIndex> {
        if self.chunk_start != 0 || self.chunk.is_empty() {
            self.fetch_at(0).ok()?;
        }
        FragmentIndex::parse(&self.chunk)
    }

    /// Makes the stream `head` followed by the file from offset `at`.
    pub fn splice(&mut self, head: Vec<u8>, at: u64) {
        self.splice = Some((head, at));
        self.pos = 0;
    }

    fn total_len(&self) -> u64 {
        match &self.splice {
            Some((head, at)) => head.len() as u64 + self.len.saturating_sub(*at),
            None => self.len,
        }
    }

    fn fetch_at(&mut self, at: u64) -> io::Result<()> {
        let end = (at + CHUNK).min(self.len) - 1;
        let t = std::time::Instant::now();
        self.chunk = fetch_range(&self.agent, &self.url, at, end)?;
        self.chunk_start = at;
        crate::trace!("source: fetched {} bytes at {at} in {:?}", self.chunk.len(), t.elapsed());
        Ok(())
    }
}

fn content_length_from_url(url: &str) -> Option<u64> {
    url.split(['?', '&']).find_map(|kv| kv.strip_prefix("clen=")).and_then(|v| v.parse().ok())
}

fn content_length_from_server(agent: &ureq::Agent, url: &str) -> io::Result<u64> {
    let resp = agent.get(url).header("Range", "bytes=0-0").call().map_err(io::Error::other)?;
    resp.headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| io::Error::other("unknown stream length"))
}

impl Read for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total_len() || buf.is_empty() {
            return Ok(0);
        }
        let file_pos = match &self.splice {
            Some((head, _)) if self.pos < head.len() as u64 => {
                let head = &head[self.pos as usize..];
                let n = buf.len().min(head.len());
                buf[..n].copy_from_slice(&head[..n]);
                self.pos += n as u64;
                return Ok(n);
            }
            Some((head, at)) => at + (self.pos - head.len() as u64),
            None => self.pos,
        };
        let chunk_end = self.chunk_start + self.chunk.len() as u64;
        if file_pos < self.chunk_start || file_pos >= chunk_end {
            self.fetch_at(file_pos)?;
        }
        let offset = (file_pos - self.chunk_start) as usize;
        let n = buf.len().min(self.chunk.len() - offset);
        buf[..n].copy_from_slice(&self.chunk[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.total_len() as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.total_len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_length_from_url() {
        let url = "https://x.googlevideo.com/videoplayback?expire=1&clen=6858497&dur=213.1";
        assert_eq!(content_length_from_url(url), Some(6_858_497));
        assert_eq!(content_length_from_url("https://x/?a=1"), None);
    }
}

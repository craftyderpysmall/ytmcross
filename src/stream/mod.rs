//! Turns a video id into a playable audio stream, ideally before it's needed.
//!
//! Native path (~200 ms): ask the `player` endpoint for the track's streams
//! and unscramble the URL in-process (`solver.rs`). yt-dlp (~4 s) is the
//! fallback when the native path can't (see `ytdlp.rs`).
//!
//! Getting ahead of the user:
//! - rows on screen are resolved in the background (`Hint::Visible`);
//! - likely next plays (row under the mouse or being pressed, next in queue,
//!   top search result) are resolved *and* their first 256 KiB of audio is
//!   downloaded (`Hint::Likely`), so playback starts without touching the
//!   network;
//! - a click never waits behind background work: it has its own thread.
//! URLs are valid for hours; they are cached in memory until they expire.

mod innertube;
mod solver;
mod ytdlp;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::player;
use crate::worker::{Notifier, Response};
use crate::{paths, trace};
use solver::{Solver, SolverStore};

/// Entry point of the `--build-solver` helper process.
pub fn build_solver(version: &str) -> i32 {
    solver::build_cli(version)
}

/// Background lookups waiting at most this many deep; older ones are dropped.
const MAX_QUEUED: usize = 16;
/// Background lookups are done in batches: one solver load for several tracks.
const BATCH: usize = 6;
/// Parallel player requests within a batch.
const PARALLEL: usize = 3;
/// After this many refused native URLs in a row, use yt-dlp for the session.
const MAX_NATIVE_FAILURES: u32 = 2;
/// Give up building a solver after this many failed attempts (offline, ...).
const MAX_REFRESH_FAILURES: u32 = 3;
/// Preloaded start of a likely next play (~4 s of 256 kbps audio).
const LIKELY_BYTES: u64 = 128 * 1024;
/// Preloaded start of any row on screen (~2 s): enough to start instantly;
/// the rest streams in over the already-warm connection (~40 ms).
const HEAD_BYTES: u64 = 64 * 1024;

/// A URL waiting to have its start preloaded.
#[derive(PartialEq)]
struct PreloadJob {
    url: String,
    native: bool,
    bytes: u64,
}

pub enum Hint {
    /// On screen: resolve in the background.
    Visible,
    /// Probably played next: resolve first, and preload the start of the audio.
    Likely,
}

enum Entry {
    Queued,
    Running,
    Ready { url: String, expires: SystemTime, native: bool },
    Failed,
}

#[derive(Default)]
struct State {
    cache: HashMap<String, Entry>,
    urgent: VecDeque<String>,
    background: VecDeque<String>,
    /// Ids that are likely to be played (or were clicked): they get a bigger
    /// preload, go first, and are worth a yt-dlp fallback.
    likely: HashSet<String>,
    /// Resolved URLs waiting to have their start preloaded.
    preload: VecDeque<PreloadJob>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    ytdlp: String,
    out: Notifier,
    agent: ureq::Agent,
    solver: SolverStore,
    native_failures: AtomicU32,
    refresh_failures: AtomicU32,
}

pub struct Resolver {
    shared: Arc<Shared>,
}

impl Resolver {
    pub fn new(ytdlp: &str, out: Notifier, agent: ureq::Agent) -> Resolver {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            wake: Condvar::new(),
            ytdlp: ytdlp.to_string(),
            out,
            solver: SolverStore::new(agent.clone()),
            agent,
            native_failures: AtomicU32::new(0),
            refresh_failures: AtomicU32::new(0),
        });
        for (name, urgent) in [("resolve-now", true), ("resolve-ahead", false)] {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || if urgent { urgent_worker(&shared) } else { background_worker(&shared) })
                .expect("failed to start resolver thread");
        }
        Resolver { shared }
    }

    /// A cached, still-valid URL for this video.
    pub fn ready(&self, id: &str) -> Option<String> {
        match self.shared.state.lock().unwrap().cache.get(id) {
            Some(Entry::Ready { url, expires, .. }) if fresh(*expires) => Some(url.clone()),
            _ => None,
        }
    }

    /// Resolve as soon as possible; a `Response::Resolved` follows.
    pub fn want(&self, id: &str) {
        let mut s = self.shared.state.lock().unwrap();
        s.likely.insert(id.to_string());
        match s.cache.get(id) {
            // Already being fetched: its result will arrive on its own (and,
            // being likely now, it gets the yt-dlp fallback if needed).
            Some(Entry::Running) => return,
            Some(Entry::Ready { url, expires, .. }) if fresh(*expires) => {
                let url = url.clone();
                drop(s);
                self.shared.out.send(Response::Resolved { video_id: id.to_string(), result: Ok(url) });
                return;
            }
            _ => {}
        }
        s.background.retain(|q| q != id);
        s.urgent.retain(|q| q != id);
        s.cache.insert(id.to_string(), Entry::Queued);
        s.urgent.push_back(id.to_string());
        self.shared.wake.notify_all();
    }

    /// The player is about to stream `url` itself: a queued preload of it
    /// would only download the same bytes again.
    pub fn playing(&self, url: &str) {
        self.shared.state.lock().unwrap().preload.retain(|j| j.url != url);
    }

    pub fn hint(&self, id: &str, hint: Hint) {
        let mut s = self.shared.state.lock().unwrap();
        let likely = matches!(hint, Hint::Likely);
        if likely {
            s.likely.insert(id.to_string());
        }
        match s.cache.get(id) {
            Some(Entry::Ready { url, expires, native }) if fresh(*expires) => {
                if likely && !player::is_preloaded(url) {
                    let job = PreloadJob { url: url.clone(), native: *native, bytes: LIKELY_BYTES };
                    s.preload.retain(|j| j.url != job.url);
                    s.preload.push_front(job);
                    self.shared.wake.notify_all();
                }
                return;
            }
            Some(Entry::Queued) => {
                if likely && let Some(i) = s.background.iter().position(|q| q == id) {
                    let id = s.background.remove(i).unwrap();
                    s.background.push_front(id);
                }
                return;
            }
            Some(Entry::Running) => return,
            Some(Entry::Ready { .. } | Entry::Failed) | None => {}
        }
        s.cache.insert(id.to_string(), Entry::Queued);
        if likely {
            s.background.push_front(id.to_string());
        } else {
            s.background.push_back(id.to_string());
        }
        while s.background.len() > MAX_QUEUED {
            if let Some(dropped) = s.background.pop_back() {
                s.cache.remove(&dropped);
            }
        }
        self.shared.wake.notify_all();
    }
}

fn fresh(expires: SystemTime) -> bool {
    expires > SystemTime::now() + Duration::from_secs(600)
}

/// googlevideo URLs carry their own expiry (`expire=<unix time>`).
fn expiry(url: &str) -> SystemTime {
    url.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("expire="))
        .and_then(|v| v.parse::<u64>().ok())
        .map(|t| UNIX_EPOCH + Duration::from_secs(t))
        .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(3 * 3600))
}

impl Shared {
    fn native_enabled(&self) -> bool {
        self.native_failures.load(Ordering::Relaxed) < MAX_NATIVE_FAILURES
    }

    /// Resolves ids natively; one result per id.
    fn resolve_native(&self, ids: &[String]) -> Vec<(String, Result<String, String>)> {
        let fail_all = |e: &str| ids.iter().map(|id| (id.clone(), Err(e.to_string()))).collect();
        if !self.native_enabled() {
            return fail_all("native resolving disabled");
        }
        let Some((sts, bytecode)) = self.solver.ready() else {
            return fail_all("solver not ready");
        };
        let t = Instant::now();
        // Player requests in parallel (network-bound), then one solver load.
        let scrambled: Vec<(String, Result<innertube::Scrambled, String>)> = std::thread::scope(|scope| {
            ids.chunks(ids.len().div_ceil(PARALLEL).max(1))
                .map(|chunk| scope.spawn(|| chunk.iter().map(|id| (id.clone(), innertube::player(&self.agent, id, sts))).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        trace!("native: {} player request(s) in {:?}", ids.len(), t.elapsed());
        let mem = || if trace::enabled() { trace::private_mb() } else { 0.0 };
        let before = mem();
        let solver = match Solver::load(&bytecode) {
            Ok(s) => s,
            Err(e) => {
                self.solver.invalidate();
                return fail_all(&e);
            }
        };
        let loaded = mem();
        let results = scrambled
            .into_iter()
            .map(|(id, s)| {
                let r = s.and_then(|s| {
                    let url = innertube::unlock(&s, &solver)?;
                    trace!("native: {id} -> itag {}", s.itag);
                    Ok(url)
                });
                if let Err(e) = &r {
                    trace!("native: {id} failed: {e}");
                }
                (id, r)
            })
            .collect();
        drop(solver);
        trace!("memory: {before:.1} MB -> solver loaded {loaded:.1} MB -> dropped {:.1} MB", mem());
        results
    }

    /// Downloads a URL's first chunk into the player's preload cache. For
    /// native URLs a refusal (403) means our unscrambling is wrong: the
    /// solver is rechecked and, if it keeps happening, native resolving is
    /// switched off for this session.
    fn preload(&self, url: &str, native: bool, bytes: u64) -> bool {
        if player::is_preloaded(url) {
            return true;
        }
        let t = Instant::now();
        match player::preload_first_chunk(&self.agent, url, bytes) {
            Ok(()) => {
                trace!("preload: {} KiB in {:?}", bytes / 1024, t.elapsed());
                if native {
                    self.native_failures.store(0, Ordering::Relaxed);
                }
                true
            }
            Err(e) => {
                trace!("preload: failed after {:?}: {e}", t.elapsed());
                if native && e.contains("403") {
                    self.native_failures.fetch_add(1, Ordering::Relaxed);
                    self.solver.invalidate();
                    self.wake.notify_all(); // the background thread rechecks the solver
                }
                false
            }
        }
    }

    /// Records a result and queues its audio start for preloading (likely
    /// plays first and bigger), unless the caller already preloaded it.
    fn finish(&self, id: &str, result: &Result<String, String>, native: bool, queue_preload: bool) {
        let mut s = self.state.lock().unwrap();
        match result {
            Ok(url) => {
                s.cache.insert(id.to_string(), Entry::Ready { url: url.clone(), expires: expiry(url), native });
                if queue_preload && !s.preload.iter().any(|j| &j.url == url) {
                    if s.likely.contains(id) {
                        s.preload.push_front(PreloadJob { url: url.clone(), native, bytes: LIKELY_BYTES });
                    } else {
                        s.preload.push_back(PreloadJob { url: url.clone(), native, bytes: HEAD_BYTES });
                    }
                }
            }
            Err(_) => {
                s.cache.insert(id.to_string(), Entry::Failed);
            }
        }
    }

    fn ytdlp_fallback(&self, ids: &[String], jar_name: &str) -> HashMap<String, String> {
        let jar = paths::data_dir().join(jar_name);
        let mut found = HashMap::new();
        let t = Instant::now();
        let _ = ytdlp::resolve(&self.ytdlp, &jar, ids, |id, url| {
            found.insert(id.to_string(), url);
        });
        trace!("yt-dlp: {}/{} resolved in {:?}", found.len(), ids.len(), t.elapsed());
        found
    }
}

/// Clicks: one at a time, never behind background work.
fn urgent_worker(shared: &Shared) {
    loop {
        let id = {
            let mut s = shared.state.lock().unwrap();
            loop {
                if let Some(id) = s.urgent.pop_front() {
                    s.cache.insert(id.clone(), Entry::Running);
                    break id;
                }
                s = shared.wake.wait(s).unwrap();
            }
        };
        let t = Instant::now();
        let ids = [id.clone()];
        let mut native = true;
        let mut result = shared.resolve_native(&ids).pop().map_or(Err("no result".into()), |(_, r)| r);
        // Verify a native URL by preloading its start: the player needs those
        // bytes first anyway, so this costs nothing extra.
        if let Ok(url) = &result
            && !shared.preload(url, true, LIKELY_BYTES)
        {
            result = Err("native URL refused".into());
        }
        if result.is_err() {
            native = false;
            result = shared.ytdlp_fallback(&ids, "ytdlp-cookies-now.txt").remove(&id).ok_or_else(|| "no playable stream".to_string());
        }
        trace!("urgent: {id} ready in {:?} ({})", t.elapsed(), if native { "native" } else { "yt-dlp" });
        shared.finish(&id, &result, native, false);
        shared.out.send(Response::Resolved { video_id: id, result });
    }
}

/// Everything ahead of the user: solver upkeep, batches, preloading.
fn background_worker(shared: &Shared) {
    enum Work {
        Preload(PreloadJob),
        Refresh,
        Batch(Vec<String>),
    }
    loop {
        let work = {
            let mut s = shared.state.lock().unwrap();
            loop {
                if let Some(job) = s.preload.pop_front() {
                    break Work::Preload(job);
                }
                if shared.solver.ready().is_none()
                    && shared.native_enabled()
                    && shared.refresh_failures.load(Ordering::Relaxed) < MAX_REFRESH_FAILURES
                {
                    break Work::Refresh;
                }
                let batch: Vec<String> = std::iter::from_fn(|| s.background.pop_front()).take(BATCH).collect();
                if !batch.is_empty() {
                    for id in &batch {
                        s.cache.insert(id.clone(), Entry::Running);
                    }
                    break Work::Batch(batch);
                }
                s = shared.wake.wait(s).unwrap();
            }
        };
        match work {
            Work::Preload(job) => {
                shared.preload(&job.url, job.native, job.bytes);
            }
            Work::Refresh => match shared.solver.refresh(true) {
                Ok(()) => shared.refresh_failures.store(0, Ordering::Relaxed),
                Err(e) => {
                    trace!("solver: refresh failed: {e}");
                    shared.refresh_failures.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_secs(10));
                }
            },
            Work::Batch(ids) => {
                let _ = shared.solver.refresh(false); // cheap unless the version check is due
                let t = Instant::now();
                let mut failed = Vec::new();
                for (id, result) in shared.resolve_native(&ids) {
                    match result {
                        Ok(url) => shared.finish(&id, &Ok(url), true, true),
                        Err(_) => failed.push(id),
                    }
                }
                // Only tracks likely to be played are worth a slow yt-dlp run;
                // the rest are simply forgotten and retried if hinted again.
                let (fallback, forget): (Vec<String>, Vec<String>) = {
                    let s = shared.state.lock().unwrap();
                    failed.into_iter().partition(|id| s.likely.contains(id))
                };
                if !forget.is_empty() {
                    let mut s = shared.state.lock().unwrap();
                    for id in &forget {
                        s.cache.remove(id);
                    }
                }
                if !fallback.is_empty() {
                    let mut found = shared.ytdlp_fallback(&fallback, "ytdlp-cookies-ahead.txt");
                    for id in &fallback {
                        let r = found.remove(id).ok_or_else(|| "no playable stream".to_string());
                        shared.finish(id, &r, false, true);
                    }
                }
                trace!("background: batch of {} in {:?}", ids.len(), t.elapsed());
                // A click may be waiting on one of these.
                for id in &ids {
                    let result = match shared.state.lock().unwrap().cache.get(id) {
                        Some(Entry::Ready { url, .. }) => Ok(url.clone()),
                        Some(Entry::Failed) => Err("no playable stream".to_string()),
                        _ => continue,
                    };
                    shared.out.send(Response::Resolved { video_id: id.clone(), result });
                }
            }
        }
    }
}

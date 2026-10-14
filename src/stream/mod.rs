//! Turns a video id into a playable audio stream, ideally before it's needed.
//!
//! Native path (~200 ms): ask the `player` endpoint for the track's streams
//! and unscramble the URL in-process (`solver.rs`).
//!
//! Getting ahead of the user:
//! - rows on screen are resolved in the background (`Hint::Visible`);
//! - likely next plays (row under the mouse or being pressed, next in queue,
//!   top search result) go first, and the first 128 KiB of their audio is
//!   preloaded (`Hint::Likely`): a row is hovered before it's clicked;
//! - a click never waits behind background work: it has its own thread.
//!
//! So playback usually starts without touching the network. URLs are valid
//! for hours; they are cached in memory (a few hundred at most) until then.

mod innertube;
mod solver;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::auth::session::Session;
use crate::{player, pool};
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
/// Give up building a solver after this many failed attempts (offline, ...).
const MAX_REFRESH_FAILURES: u32 = 3;
/// Preloaded start of a likely next play (~4 s of 256 kbps audio).
const LIKELY_BYTES: u64 = 128 * 1024;
/// Resolved URLs kept (~1 KB each); expired ones go first.
const MAX_CACHED: usize = 256;

pub enum Hint {
    /// On screen: resolve in the background.
    Visible,
    /// Probably played next: resolve first, and preload the start of the audio.
    Likely,
}

enum Entry {
    Queued,
    Running,
    Ready { url: String, expires: SystemTime },
    Failed,
}

#[derive(Default)]
struct State {
    cache: HashMap<String, Entry>,
    urgent: VecDeque<String>,
    background: VecDeque<String>,
    /// Ids that are likely to be played (or were clicked): they get a bigger
    /// preload and go first.
    likely: HashSet<String>,
    /// Resolved URLs waiting to have their start preloaded.
    preload: VecDeque<String>,
    /// The worker threads exist only while there's work (see `start`).
    urgent_running: bool,
    background_running: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    out: Notifier,
    agent: ureq::Agent,
    /// The account's cookies (reloaded on `reset`, i.e. after signing in or out).
    session: Mutex<Session>,
    solver: SolverStore,
    refresh_failures: AtomicU32,
}

/// Cheap to clone: clones share one cache and one set of workers.
#[derive(Clone)]
pub struct Resolver {
    shared: Arc<Shared>,
}

impl Resolver {
    pub fn new(out: Notifier, agent: ureq::Agent) -> Resolver {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            wake: Condvar::new(),
            out,
            session: Mutex::new(Session::load(&paths::cookies_file())),
            solver: SolverStore::new(agent.clone()),
            agent,
            refresh_failures: AtomicU32::new(0),
        });
        // Checks the player version (and builds a solver if needed) right away.
        start(&shared, shared.state.lock().unwrap());
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
            // Already being fetched: its result will arrive on its own.
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
        start(&self.shared, s);
    }

    /// Forgets everything resolved so far: stream URLs belong to the account
    /// that fetched them (Premium gets other formats), so after signing in or
    /// out they must be fetched again.
    pub fn reset(&self) {
        let mut s = self.shared.state.lock().unwrap();
        s.cache.retain(|_, e| matches!(e, Entry::Running));
        s.background.clear();
        s.likely.clear();
        s.preload.clear();
        drop(s);
        *self.shared.session.lock().unwrap() = Session::load(&paths::cookies_file());
        player::clear_preloaded();
        self.shared.refresh_failures.store(0, Ordering::Relaxed);
    }

    /// Resolves `ids` on the calling thread (downloads), with one solver
    /// load for all of them; builds the solver first if there is none.
    pub fn resolve_now(&self, ids: &[String]) -> Vec<(String, Result<String, String>)> {
        if let Err(e) = self.shared.solver.refresh(false) {
            return ids.iter().map(|id| (id.clone(), Err(e.clone()))).collect();
        }
        self.shared.resolve(ids)
    }

    /// YouTube refused a URL we unscrambled: the next lookup rebuilds the solver.
    pub fn refused(&self) {
        self.shared.solver.invalidate();
    }

    /// The player is about to stream `url` itself: a queued preload of it
    /// would only download the same bytes again.
    pub fn playing(&self, url: &str) {
        self.shared.state.lock().unwrap().preload.retain(|u| u != url);
    }

    pub fn hint(&self, id: &str, hint: Hint) {
        let mut s = self.shared.state.lock().unwrap();
        let likely = matches!(hint, Hint::Likely);
        if likely {
            s.likely.insert(id.to_string());
        }
        match s.cache.get(id) {
            Some(Entry::Ready { url, expires }) if fresh(*expires) => {
                if likely && !player::is_preloaded(url) {
                    let url = url.clone();
                    s.preload.retain(|u| *u != url);
                    s.preload.push_front(url);
                    start(&self.shared, s);
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
        start(&self.shared, s);
    }
}

/// Wakes the workers, first starting whichever has work but isn't running:
/// they exit after a few idle seconds (a thread that ran the solver would
/// otherwise keep its grown stack).
fn start(shared: &Arc<Shared>, mut s: MutexGuard<State>) {
    let spawn = |name: &str, work: fn(&Arc<Shared>)| {
        let shared = shared.clone();
        std::thread::Builder::new().name(name.into()).spawn(move || work(&shared)).is_ok()
    };
    if !s.urgent.is_empty() && !s.urgent_running {
        s.urgent_running = spawn("resolve-now", urgent_worker);
    }
    if !s.background_running && (!s.background.is_empty() || !s.preload.is_empty() || shared.needs_refresh()) {
        s.background_running = spawn("resolve-ahead", background_worker);
    }
    drop(s);
    shared.wake.notify_all();
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
    /// No solver for the current player yet, and building one may still work.
    fn needs_refresh(&self) -> bool {
        self.solver.ready().is_none() && self.refresh_failures.load(Ordering::Relaxed) < MAX_REFRESH_FAILURES
    }

    /// Resolves ids; one result per id.
    fn resolve(&self, ids: &[String]) -> Vec<(String, Result<String, String>)> {
        let fail_all = |e: &str| ids.iter().map(|id| (id.clone(), Err(e.to_string()))).collect();
        let Some((sts, bytecode)) = self.solver.ready() else {
            return fail_all("solver not ready");
        };
        let t = Instant::now();
        let session = self.session.lock().unwrap().clone();
        // Player requests in parallel (network-bound), then one solver load.
        let scrambled: Vec<(String, Result<innertube::Scrambled, String>)> = std::thread::scope(|scope| {
            ids.chunks(ids.len().div_ceil(PARALLEL).max(1))
                .map(|chunk| {
                    scope.spawn(|| chunk.iter().map(|id| (id.clone(), innertube::player(&self.agent, &session, id, sts))).collect::<Vec<_>>())
                })
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

    /// Downloads a URL's first chunk into the player's preload cache. A
    /// refusal (403) means our unscrambling is wrong: the solver is rebuilt.
    fn preload(&self, url: &str) -> bool {
        if player::is_preloaded(url) {
            return true;
        }
        let t = Instant::now();
        match player::preload_first_chunk(&self.agent, url, LIKELY_BYTES) {
            Ok(()) => {
                trace!("preload: {} KiB in {:?}", LIKELY_BYTES / 1024, t.elapsed());
                true
            }
            Err(e) => {
                trace!("preload: failed after {:?}: {e}", t.elapsed());
                if e.contains("403") {
                    self.solver.invalidate();
                    self.wake.notify_all(); // the background thread rechecks the solver
                }
                false
            }
        }
    }

    /// Records a result and, for a likely play, queues its audio start for
    /// preloading (unless the caller already preloaded it).
    fn finish(&self, id: &str, result: &Result<String, String>, queue_preload: bool) {
        let mut s = self.state.lock().unwrap();
        match result {
            Ok(url) => {
                s.cache.insert(id.to_string(), Entry::Ready { url: url.clone(), expires: expiry(url) });
                if queue_preload && s.likely.contains(id) && !s.preload.contains(url) {
                    s.preload.push_front(url.clone());
                }
            }
            Err(_) => {
                s.cache.insert(id.to_string(), Entry::Failed);
            }
        }
        if s.cache.len() > MAX_CACHED {
            // Expired URLs first, then any other finished lookup but this one.
            s.cache.retain(|_, e| !matches!(e, Entry::Ready { expires, .. } if !fresh(*expires)));
            let extra = s.cache.len().saturating_sub(MAX_CACHED);
            let old: Vec<String> =
                s.cache.iter().filter(|(k, e)| *k != id && matches!(e, Entry::Ready { .. })).map(|(k, _)| k.clone()).take(extra).collect();
            for k in old {
                s.cache.remove(&k);
                s.likely.remove(&k);
            }
        }
    }
}

/// Clicks: one at a time, never behind background work.
fn urgent_worker(shared: &Arc<Shared>) {
    loop {
        let id = {
            let mut s = shared.state.lock().unwrap();
            loop {
                if let Some(id) = s.urgent.pop_front() {
                    s.cache.insert(id.clone(), Entry::Running);
                    break id;
                }
                let (guard, wait) = shared.wake.wait_timeout(s, pool::IDLE).unwrap();
                s = guard;
                if wait.timed_out() && s.urgent.is_empty() {
                    s.urgent_running = false;
                    return;
                }
            }
        };
        let t = Instant::now();
        let mut result = shared.resolve(std::slice::from_ref(&id)).pop().map_or(Err("no result".into()), |(_, r)| r);
        // Verify the URL by preloading its start: the player needs those
        // bytes first anyway, so this costs nothing extra.
        if let Ok(url) = &result
            && !shared.preload(url)
        {
            result = Err("stream refused".into());
            start(shared, shared.state.lock().unwrap()); // a refusal may need a new solver
        }
        trace!("urgent: {id} done in {:?} ({})", t.elapsed(), if result.is_ok() { "ok" } else { "failed" });
        shared.finish(&id, &result, false);
        shared.out.send(Response::Resolved { video_id: id, result });
    }
}

/// Everything ahead of the user: solver upkeep, batches, preloading.
fn background_worker(shared: &Arc<Shared>) {
    enum Work {
        Preload(String),
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
                if shared.needs_refresh() {
                    break Work::Refresh;
                }
                let batch: Vec<String> = std::iter::from_fn(|| s.background.pop_front()).take(BATCH).collect();
                if !batch.is_empty() {
                    for id in &batch {
                        s.cache.insert(id.clone(), Entry::Running);
                    }
                    break Work::Batch(batch);
                }
                let (guard, wait) = shared.wake.wait_timeout(s, pool::IDLE).unwrap();
                s = guard;
                if wait.timed_out() && s.preload.is_empty() && s.background.is_empty() && !shared.needs_refresh() {
                    s.background_running = false;
                    return;
                }
            }
        };
        match work {
            Work::Preload(job) => {
                shared.preload(&job);
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
                for (id, result) in shared.resolve(&ids) {
                    // A failed likely play is recorded, so a click waiting on it
                    // hears back; others are forgotten (a later hint retries).
                    let likely = shared.state.lock().unwrap().likely.contains(&id);
                    if result.is_ok() || likely {
                        shared.finish(&id, &result, true);
                    } else {
                        shared.state.lock().unwrap().cache.remove(&id);
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

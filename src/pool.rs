//! Worker threads that exist only while there is work: each exits after a
//! few idle seconds and the next job starts a fresh one. Nothing waits
//! around in the background, and a thread that did deep work (big JSON,
//! image decoding) doesn't keep its grown stack for the rest of the session.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long an idle worker waits for more work before it exits.
pub const IDLE: Duration = Duration::from_secs(5);

/// Runs jobs on up to `max` threads.
pub struct Pool<J: Send + 'static> {
    inner: Arc<Inner<J>>,
}

impl<J: Send + 'static> Clone for Pool<J> {
    fn clone(&self) -> Self {
        Pool { inner: self.inner.clone() }
    }
}

/// Handles one job; gets the pool to queue follow-ups.
type Run<J> = Box<dyn Fn(J, &Pool<J>) + Send + Sync>;

struct Inner<J: Send + 'static> {
    state: Mutex<State<J>>,
    more: Condvar,
    name: &'static str,
    max: usize,
    run: Run<J>,
}

struct State<J> {
    queue: VecDeque<J>,
    workers: usize,
    idle: usize,
}

impl<J: Send + 'static> Pool<J> {
    pub fn new(name: &'static str, max: usize, run: impl Fn(J, &Pool<J>) + Send + Sync + 'static) -> Pool<J> {
        let state = Mutex::new(State { queue: VecDeque::new(), workers: 0, idle: 0 });
        Pool { inner: Arc::new(Inner { state, more: Condvar::new(), name, max: max.max(1), run: Box::new(run) }) }
    }

    pub fn push(&self, job: J) {
        let mut s = self.inner.state.lock().unwrap();
        s.queue.push_back(job);
        if s.idle > 0 {
            self.inner.more.notify_one();
        } else if s.workers < self.inner.max {
            s.workers += 1;
            let pool = self.clone();
            let spawned = std::thread::Builder::new().name(self.inner.name.into()).spawn(move || pool.work());
            if spawned.is_err() {
                s.workers -= 1;
            }
        }
    }

    fn work(&self) {
        loop {
            let job = {
                let mut s = self.inner.state.lock().unwrap();
                loop {
                    if let Some(job) = s.queue.pop_front() {
                        break job;
                    }
                    s.idle += 1;
                    let (guard, wait) = self.inner.more.wait_timeout(s, IDLE).unwrap();
                    s = guard;
                    s.idle -= 1;
                    if wait.timed_out() && s.queue.is_empty() {
                        s.workers -= 1;
                        return;
                    }
                }
            };
            (self.inner.run)(job, self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    #[test]
    fn runs_every_job_and_follow_ups() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = Pool::new("test", 2, move |n: u32, pool: &Pool<u32>| {
            if n > 0 {
                pool.push(n - 1);
            }
            tx.lock().unwrap().send(n).unwrap();
        });
        pool.push(3);
        pool.push(10);
        let mut got: Vec<u32> = (0..15).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        got.sort();
        assert_eq!(got, [0, 0, 1, 1, 2, 2, 3, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn never_runs_more_than_max_at_once() {
        let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (r, p) = (running.clone(), peak.clone());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = Pool::new("test", 3, move |_: (), _: &Pool<()>| {
            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            r.fetch_sub(1, Ordering::SeqCst);
            tx.lock().unwrap().send(()).unwrap();
        });
        (0..12).for_each(|_| pool.push(()));
        (0..12).for_each(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }
}

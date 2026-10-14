//! In-process audio playback: no mpv, no extra process.
//!
//! One engine thread streams the track over HTTP (`source.rs`), demuxes and
//! decodes it with symphonia (AAC in MP4: YouTube Music's itag 141/140), and
//! keeps ~2 s of samples queued for the audio device (`output.rs`).
//!
//! Wake-ups: while playing, the engine refills the buffer about four times
//! a second; paused or idle, it blocks on its command channel and the
//! audio stream is paused, so nothing runs at all. After 30 s of that, the
//! track and the audio device are closed too (the app reloads the track
//! where it was when play is pressed).

pub mod eq;
mod index;
mod output;
mod pitch;
mod source;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

use crate::config::{EqualizerConfig, PlayerConfig};
use crate::{pool, trace};
use index::FragmentIndex;
use output::{Output, Shared};
use source::HttpSource;
pub use source::{clear_preloaded, is_preloaded, preload_first_chunk, stream_len};

/// How often the engine tops up the buffer while playing.
const REFILL_EVERY: Duration = Duration::from_millis(250);
/// Paused or stopped this long, the track and the audio device are let go.
const RELEASE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// A new track started loading.
    Loading,
    /// Audio is playing from this position (after load, seek or pause toggle).
    Position(f64),
    Duration(f64),
    Paused(bool),
    /// The current track reached its end.
    Finished,
    Error(String),
    /// Paused for long, the track was let go: play loads it again from here.
    Released(f64),
}

/// What to play.
#[derive(Clone)]
enum Media {
    /// A stream URL from the resolver.
    Url(String),
    /// A downloaded file.
    File(std::path::PathBuf),
}

enum Command {
    /// What to play and the position to start at.
    Load(Media, f64),
    Pause(bool),
    Seek(f64),
    Stop,
}

/// Told what the engine does (called on the engine's thread).
type Notify = Arc<dyn Fn(PlayerEvent) + Send + Sync>;
/// The way to the engine thread, while one runs.
type Link = Arc<Mutex<Option<Sender<Command>>>>;

pub struct Player {
    shared: Arc<Shared>,
    agent: ureq::Agent,
    notify: Notify,
    /// The engine thread starts with the first command and exits once it
    /// holds nothing (see `Engine::run`); commands are sent under this lock.
    engine: Link,
}

impl Player {
    /// `agent` is shared with the resolver: preloads warm up the same
    /// connections the player then streams over.
    pub fn new(
        cfg: &PlayerConfig,
        eq: &EqualizerConfig,
        pitch: i32,
        agent: ureq::Agent,
        notify: impl Fn(PlayerEvent) + Send + Sync + 'static,
    ) -> Player {
        let shared = Shared::new(eq, pitch);
        shared.set_volume(cfg.volume.min(100) as f32 / 100.0);
        Player { shared, agent, notify: Arc::new(notify), engine: Link::default() }
    }

    fn send(&self, cmd: Command) {
        let mut link = self.engine.lock().unwrap();
        let tx = link.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let engine = Engine::new(self.shared.clone(), self.notify.clone(), self.agent.clone());
            let link = self.engine.clone();
            let _ = std::thread::Builder::new().name("audio-engine".into()).spawn(move || engine.run(rx, &link));
            tx
        });
        let _ = tx.send(cmd);
    }

    /// Plays a direct stream URL (from the resolver) from `start` seconds.
    pub fn load(&self, url: &str, start: f64) {
        self.send(Command::Load(Media::Url(url.to_string()), start));
    }

    /// Plays a downloaded file from `start` seconds.
    pub fn load_file(&self, path: std::path::PathBuf, start: f64) {
        self.send(Command::Load(Media::File(path), start));
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Command::Pause(paused));
    }

    pub fn seek(&self, seconds: f64) {
        self.send(Command::Seek(seconds));
    }

    /// 0..=100, applied immediately in the audio callback.
    pub fn set_volume(&self, volume: f64) {
        self.shared.set_volume((volume / 100.0) as f32);
    }

    /// Applied immediately in the audio callback.
    pub fn set_equalizer(&self, eq: &EqualizerConfig) {
        self.shared.eq.set(eq);
    }

    /// Semitones, applied immediately in the audio callback.
    pub fn set_pitch(&self, semitones: i32) {
        self.shared.pitch.store(semitones.clamp(-12, 12), std::sync::atomic::Ordering::Relaxed);
    }
}

/// The track being played.
struct Current {
    media: Media,
    /// Where its fragments start, to seek straight to the right one
    /// (streams only: a file seeks fast enough without).
    index: Option<Arc<FragmentIndex>>,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    channels: u16,
    /// Track time at which the played-frames counter was last reset.
    base: f64,
    /// First audio since load/seek has been queued (Position was reported).
    started: bool,
    /// The decoder ran out of data.
    eof: bool,
}

struct Engine {
    shared: Arc<Shared>,
    notify: Notify,
    agent: ureq::Agent,
    output: Option<Output>,
    current: Option<Current>,
    paused: bool,
    scratch: Vec<f32>,
}

impl Engine {
    fn new(shared: Arc<Shared>, notify: Notify, agent: ureq::Agent) -> Engine {
        Engine { shared, notify, agent, output: None, current: None, paused: false, scratch: Vec::new() }
    }

    fn run(mut self, commands: Receiver<Command>, link: &Mutex<Option<Sender<Command>>>) {
        loop {
            let active = self.current.is_some() && !self.paused;
            let next = if active {
                commands.recv_timeout(REFILL_EVERY)
            } else if self.output.is_some() {
                commands.recv_timeout(RELEASE_AFTER)
            } else {
                commands.recv_timeout(pool::IDLE)
            };
            match next {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) if active => {}
                Err(RecvTimeoutError::Timeout) if self.output.is_some() => self.release(),
                // Holding nothing: exit, unless a command slipped in (they're
                // sent under the lock, so none can arrive after this check).
                Err(RecvTimeoutError::Timeout) => {
                    let mut link = link.lock().unwrap();
                    match commands.try_recv() {
                        Ok(cmd) => {
                            drop(link);
                            self.handle(cmd);
                        }
                        Err(_) => {
                            *link = None;
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if self.current.is_some() && !self.paused {
                self.fill();
            }
        }
    }

    fn position(&self) -> f64 {
        match (&self.current, &self.output) {
            (Some(c), Some(o)) => c.base + self.shared.played_frames() as f64 / o.clock_rate() as f64,
            _ => 0.0,
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Load(media, start) => {
                trace!("engine: load");
                self.stop();
                (self.notify)(PlayerEvent::Loading);
                self.paused = false;
                let t = std::time::Instant::now();
                let opened = self.open(&media, start, None);
                trace!("engine: opened in {:?}", t.elapsed());
                match opened {
                    Ok(current) => {
                        if let Some(d) = duration(current.format.as_ref(), current.track_id) {
                            (self.notify)(PlayerEvent::Duration(d));
                        }
                        self.current = Some(current);
                    }
                    Err(e) => (self.notify)(PlayerEvent::Error(format!("playback failed: {e}"))),
                }
            }
            Command::Stop => self.stop(),
            Command::Pause(paused) => {
                // Nothing loaded (e.g. just released): nothing to pause or resume.
                if self.paused == paused || self.current.is_none() {
                    return;
                }
                self.paused = paused;
                if let Some(out) = &mut self.output {
                    if paused { out.pause() } else if self.current.is_some() { out.play() }
                }
                (self.notify)(PlayerEvent::Paused(paused));
                (self.notify)(PlayerEvent::Position(self.position()));
            }
            Command::Seek(seconds) => self.seek(seconds),
        }
    }

    fn stop(&mut self) {
        self.current = None;
        self.shared.reset();
        if let Some(out) = &mut self.output {
            out.pause();
        }
    }

    /// Paused (or stopped) for a while: close the track and the audio
    /// device, so nothing waits in the background and their memory goes.
    fn release(&mut self) {
        if self.current.is_some() {
            (self.notify)(PlayerEvent::Released(self.position()));
        }
        trace!("engine: released the track and the audio device");
        self.current = None;
        self.output = None;
        self.shared.free();
        self.scratch = Vec::new();
    }

    /// Opens `media` at `start` seconds and prepares the decoder and an
    /// output that fits it. `index` is the stream's, if already known.
    fn open(&mut self, media: &Media, start: f64, index: Option<Arc<FragmentIndex>>) -> Result<Current, String> {
        let mut skipped = 0.0;
        let (source, index): (Box<dyn MediaSource>, _) = match media {
            Media::File(path) => (Box::new(std::fs::File::open(path).map_err(|e| e.to_string())?), None),
            Media::Url(url) => {
                let mut source = HttpSource::open(self.agent.clone(), url).map_err(|e| e.to_string())?;
                let index = index.or_else(|| source.fragment_index().map(Arc::new));
                // Starting later: the demuxer sees the file's head, then the fragment
                // holding `start`, so it never fetches the fragments before it.
                if let Some(ix) = &index
                    && let Some((at, from)) = ix.fragment_at(start).filter(|f| f.1 > 0.0)
                {
                    source.splice(ix.head.clone(), at);
                    skipped = from;
                }
                (Box::new(source), index)
            }
        };
        let stream = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("m4a");
        let format = symphonia::default::get_probe()
            .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| format!("unsupported stream ({e})"))?;
        let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("not an audio track")?.clone();
        let (track_id, time_base) = (track.id, track.time_base);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec ({e})"))?;
        let rate = params.sample_rate.ok_or("unknown sample rate")?;
        let channels = params.channels.as_ref().map_or(2, |c| c.count()) as u16;

        if !self.output.as_ref().is_some_and(|o| o.matches(rate, channels)) {
            self.output = None; // close the old stream before opening a new one
            self.output = Some(Output::open(&self.shared, rate, channels)?);
        }
        self.shared.reset();
        let mut current = Current {
            media: media.clone(),
            index,
            format,
            decoder,
            track_id,
            time_base,
            channels,
            base: skipped,
            started: false,
            eof: false,
        };
        if start > skipped {
            current.base = skipped + seek_format(&mut current, start - skipped)?;
        }
        Ok(current)
    }

    fn seek(&mut self, seconds: f64) {
        let Some(current) = &mut self.current else { return };
        self.shared.reset();
        // Without an index, the demuxer walks to the target fragment by fragment.
        if current.index.is_none()
            && let Ok(actual) = seek_format(current, seconds)
        {
            current.base = actual;
            current.started = false;
            current.eof = false;
            return;
        }
        // With one, reopening starts right at the target's fragment (and
        // reopening also recovers a reader that hit the end of the stream).
        let (media, index) = (current.media.clone(), current.index.clone());
        let t = std::time::Instant::now();
        match self.open(&media, seconds, index) {
            Ok(reopened) => self.current = Some(reopened),
            Err(e) => {
                self.stop();
                (self.notify)(PlayerEvent::Error(format!("seek failed: {e}")));
            }
        }
        trace!("engine: seek reopened in {:?}", t.elapsed());
    }

    /// Decodes until the output buffer is full, the track ends, or it fails.
    fn fill(&mut self) {
        let (Some(current), Some(out)) = (&mut self.current, &mut self.output) else { return };
        if current.eof {
            if self.shared.buffered() == 0 {
                self.current = None;
                out.pause();
                (self.notify)(PlayerEvent::Finished);
            }
            return;
        }
        while out.wants_more(&self.shared) {
            let packet = match current.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    current.eof = true;
                    return;
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    current.eof = true;
                    return;
                }
                Err(e) => {
                    self.current = None;
                    out.pause();
                    (self.notify)(PlayerEvent::Error(format!("playback failed: {e}")));
                    return;
                }
            };
            if packet.track_id != current.track_id {
                continue;
            }
            match current.decoder.decode(&packet) {
                Ok(audio) => {
                    self.scratch.clear();
                    audio.copy_to_vec_interleaved(&mut self.scratch);
                }
                // A corrupt packet: skip it, like any player would.
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    self.current = None;
                    out.pause();
                    (self.notify)(PlayerEvent::Error(format!("playback failed: {e}")));
                    return;
                }
            }
            out.push(&self.shared, &self.scratch, current.channels);
            if !current.started {
                current.started = true;
                out.play();
                trace!("engine: first audio queued, output started");
                (self.notify)(PlayerEvent::Position(current.base));
            }
        }
    }
}

/// Seeks the reader; returns the time actually landed on.
fn seek_format(current: &mut Current, seconds: f64) -> Result<f64, String> {
    let time = Time::try_from_secs_f64(seconds.max(0.0)).ok_or("bad seek time")?;
    let seeked = current
        .format
        .seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(current.track_id) })
        .map_err(|e| e.to_string())?;
    current.decoder.reset();
    Ok(current
        .time_base
        .and_then(|tb| tb.calc_time(seeked.actual_ts))
        .map_or(seconds, |t| t.as_secs_f64()))
}

fn duration(format: &dyn FormatReader, track_id: u32) -> Option<f64> {
    let track = format.tracks().iter().find(|t| t.id == track_id)?;
    Some(track.time_base?.calc_duration(track.duration?)?.as_secs_f64())
}

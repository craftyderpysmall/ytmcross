//! In-process audio playback: no mpv, no extra process.
//!
//! One engine thread streams the track over HTTP (`source.rs`), demuxes and
//! decodes it with symphonia (AAC in MP4: YouTube Music's itag 141/140), and
//! keeps ~2 s of samples queued for the audio device (`output.rs`).
//!
//! Wake-ups: while playing, the engine refills the buffer about four times
//! a second; paused or idle, it blocks on its command channel and the
//! audio stream is paused, so nothing runs at all.

pub mod eq;
mod output;
mod source;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

use crate::config::{EqualizerConfig, PlayerConfig};
use crate::trace;
use output::{Output, Shared};
use source::HttpSource;
pub use source::{clear_preloaded, is_preloaded, preload_first_chunk};

/// How often the engine tops up the buffer while playing.
const REFILL_EVERY: Duration = Duration::from_millis(250);

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
}

enum Command {
    Load(String),
    Pause(bool),
    Seek(f64),
    Stop,
}

pub struct Player {
    commands: Sender<Command>,
    shared: Arc<Shared>,
}

impl Player {
    /// `agent` is shared with the resolver: preloads warm up the same
    /// connections the player then streams over.
    pub fn spawn(
        cfg: &PlayerConfig,
        eq: &EqualizerConfig,
        agent: ureq::Agent,
        notify: impl Fn(PlayerEvent) + Send + 'static,
    ) -> std::io::Result<Player> {
        let shared = Shared::new(eq);
        shared.set_volume(cfg.volume.min(100) as f32 / 100.0);
        let (tx, rx) = mpsc::channel();
        let engine_shared = shared.clone();
        std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || Engine::new(engine_shared, Box::new(notify), agent).run(rx))?;
        Ok(Player { commands: tx, shared })
    }

    /// Plays a direct stream URL (from the resolver).
    pub fn load(&self, url: &str) {
        let _ = self.commands.send(Command::Load(url.to_string()));
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.commands.send(Command::Pause(paused));
    }

    pub fn seek(&self, seconds: f64) {
        let _ = self.commands.send(Command::Seek(seconds));
    }

    /// 0..=100, applied immediately in the audio callback.
    pub fn set_volume(&self, volume: f64) {
        self.shared.set_volume((volume / 100.0) as f32);
    }

    /// Applied immediately in the audio callback.
    pub fn set_equalizer(&self, eq: &EqualizerConfig) {
        self.shared.eq.set(eq);
    }
}

/// The track being played.
struct Current {
    url: String,
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
    notify: Box<dyn Fn(PlayerEvent) + Send>,
    agent: ureq::Agent,
    output: Option<Output>,
    current: Option<Current>,
    paused: bool,
    scratch: Vec<f32>,
}

impl Engine {
    fn new(shared: Arc<Shared>, notify: Box<dyn Fn(PlayerEvent) + Send>, agent: ureq::Agent) -> Engine {
        Engine { shared, notify, agent, output: None, current: None, paused: false, scratch: Vec::new() }
    }

    fn run(mut self, commands: Receiver<Command>) {
        loop {
            let active = self.current.is_some() && !self.paused;
            let next = if !active {
                commands.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                commands.recv_timeout(REFILL_EVERY)
            };
            match next {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
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
            Command::Load(url) => {
                trace!("engine: load");
                self.stop();
                (self.notify)(PlayerEvent::Loading);
                self.paused = false;
                let t = std::time::Instant::now();
                let opened = self.open(&url, 0.0);
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
                if self.paused == paused {
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

    /// Opens `url`, prepares the decoder and an output that fits it, and
    /// optionally seeks.
    fn open(&mut self, url: &str, start: f64) -> Result<Current, String> {
        let source = HttpSource::open(self.agent.clone(), url).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
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
            url: url.to_string(),
            format,
            decoder,
            track_id,
            time_base,
            channels,
            base: 0.0,
            started: false,
            eof: false,
        };
        if start > 0.0 {
            current.base = seek_format(&mut current, start)?;
        }
        Ok(current)
    }

    fn seek(&mut self, seconds: f64) {
        let Some(current) = &mut self.current else { return };
        self.shared.reset();
        match seek_format(current, seconds) {
            Ok(actual) => {
                current.base = actual;
                current.started = false;
                current.eof = false;
            }
            // Seeking after the reader hit the end of the stream can fail;
            // reopening the stream and seeking from scratch always works.
            Err(_) => {
                let url = current.url.clone();
                match self.open(&url, seconds) {
                    Ok(reopened) => self.current = Some(reopened),
                    Err(e) => {
                        self.stop();
                        (self.notify)(PlayerEvent::Error(format!("seek failed: {e}")));
                    }
                }
            }
        }
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

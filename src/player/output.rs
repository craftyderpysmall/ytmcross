//! Audio output: a cpal stream fed from a small sample buffer.
//!
//! The audio callback is the only real-time code. It never blocks: it
//! `try_lock`s the buffer and plays silence if it can't get it. Volume and
//! the equalizer are applied here (so changes are heard at once), and it
//! counts the frames actually played, which is where the position clock
//! comes from.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::eq::{EqShared, EqState};
use crate::config::EqualizerConfig;

/// Seconds of decoded audio kept ahead of the speakers. Covers network
/// hiccups between 256 KiB fetches without holding much memory (~700 KB).
const BUFFER_SECS: usize = 2;

pub struct Shared {
    samples: Mutex<VecDeque<f32>>,
    /// f32 bits; gain applied in the callback.
    gain: AtomicU32,
    /// Frames handed to the device since the last reset.
    played: AtomicU64,
    pub eq: EqShared,
}

impl Shared {
    pub fn new(eq: &EqualizerConfig) -> Arc<Shared> {
        Arc::new(Shared {
            samples: Mutex::new(VecDeque::new()),
            gain: AtomicU32::new(1.0f32.to_bits()),
            played: AtomicU64::new(0),
            eq: EqShared::new(eq),
        })
    }

    /// `volume` is 0..=1; a cubic curve matches perceived loudness (as mpv does).
    pub fn set_volume(&self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        self.gain.store((v * v * v).to_bits(), Ordering::Relaxed);
    }

    pub fn played_frames(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.samples.lock().unwrap().clear();
        self.played.store(0, Ordering::Relaxed);
    }

    pub fn buffered(&self) -> usize {
        self.samples.lock().unwrap().len()
    }
}

/// An open output stream at a fixed rate and channel count.
pub struct Output {
    stream: cpal::Stream,
    pub rate: u32,
    pub channels: u16,
    /// Source rate when we had to resample ourselves (device refused the source rate).
    resample_from: Option<u32>,
    resample_pos: f64,
    playing: bool,
}

impl Output {
    /// Opens the default device at the source's rate/channels. Most systems
    /// resample for us (WASAPI autoconvert, CoreAudio, PipeWire/Pulse); if the
    /// device refuses, fall back to its native format and resample here.
    pub fn open(shared: &Arc<Shared>, rate: u32, channels: u16) -> Result<Output, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let wanted = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        if let Ok(stream) = build(&device, &wanted, shared) {
            return Ok(Output { stream, rate, channels, resample_from: None, resample_pos: 0.0, playing: false });
        }
        let native = device.default_output_config().map_err(|e| e.to_string())?.config();
        let stream = build(&device, &native, shared)?;
        Ok(Output {
            stream,
            rate: native.sample_rate,
            channels: native.channels,
            resample_from: Some(rate),
            resample_pos: 0.0,
            playing: false,
        })
    }

    pub fn matches(&self, rate: u32, channels: u16) -> bool {
        match self.resample_from {
            Some(src) => src == rate,
            None => self.rate == rate && self.channels == channels,
        }
    }

    /// The rate the position clock counts in (frames played per second).
    pub fn clock_rate(&self) -> u32 {
        self.rate
    }

    pub fn play(&mut self) {
        if !self.playing {
            self.playing = self.stream.play().is_ok();
        }
    }

    /// Pausing the stream stops the audio thread's wake-ups entirely.
    pub fn pause(&mut self) {
        if self.playing {
            let _ = self.stream.pause();
            self.playing = false;
        }
    }

    /// True if there's room for more decoded audio.
    pub fn wants_more(&self, shared: &Shared) -> bool {
        shared.buffered() < self.rate as usize * self.channels as usize * BUFFER_SECS
    }

    /// Queues interleaved samples (`src_channels` wide) for playback.
    pub fn push(&mut self, shared: &Shared, samples: &[f32], src_channels: u16) {
        let (out_ch, src_ch) = (self.channels as usize, src_channels.max(1) as usize);
        let mut q = shared.samples.lock().unwrap();
        match self.resample_from {
            None => {
                if out_ch == src_ch {
                    q.extend(samples.iter().copied());
                } else {
                    for frame in samples.chunks_exact(src_ch) {
                        q.extend((0..out_ch).map(|c| map_channel(frame, c)));
                    }
                }
            }
            Some(src_rate) => {
                // Linear interpolation: only used when the OS can't resample.
                let step = src_rate as f64 / self.rate as f64;
                let frames: Vec<&[f32]> = samples.chunks_exact(src_ch).collect();
                while (self.resample_pos as usize) + 1 < frames.len() {
                    let i = self.resample_pos as usize;
                    let t = (self.resample_pos - i as f64) as f32;
                    for c in 0..out_ch {
                        let (a, b) = (map_channel(frames[i], c), map_channel(frames[i + 1], c));
                        q.push_back(a + (b - a) * t);
                    }
                    self.resample_pos += step;
                }
                self.resample_pos -= (frames.len().saturating_sub(1)) as f64;
            }
        }
    }
}

/// Output channel `c` from a source frame: copy if present, else reuse/mix.
fn map_channel(frame: &[f32], c: usize) -> f32 {
    match frame.len() {
        0 => 0.0,
        1 => frame[0],
        n if c < n => frame[c],
        _ => (frame[0] + frame[1]) * 0.5,
    }
}

fn build(device: &cpal::Device, config: &cpal::StreamConfig, shared: &Arc<Shared>) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let shared = shared.clone();
    let mut eq = EqState::new(config.sample_rate, config.channels);
    device
        .build_output_stream(
            *config,
            move |out: &mut [f32], _| {
                let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
                let mut written = 0;
                if let Ok(mut q) = shared.samples.try_lock() {
                    let n = out.len().min(q.len());
                    for (dst, src) in out[..n].iter_mut().zip(q.drain(..n)) {
                        *dst = src * gain;
                    }
                    written = n;
                }
                eq.process(&shared.eq, &mut out[..written]);
                out[written..].fill(0.0);
                shared.played.fetch_add((written / channels) as u64, Ordering::Relaxed);
            },
            |e| eprintln!("ytmcross: audio output error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

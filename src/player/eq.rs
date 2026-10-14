//! A small 5-band equalizer, applied in the audio callback.
//!
//! Standard RBJ biquads: a low shelf, three peaking bands and a high shelf.
//! Settings arrive through atomics (the callback never waits on a lock) and
//! coefficients are only recomputed when they change. Flat bands are
//! skipped, so a flat or disabled EQ costs nothing.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::config::{EQ_BANDS, EqualizerConfig};

/// Centre/corner frequencies in Hz.
pub const BANDS: [f32; EQ_BANDS] = [60.0, 250.0, 1000.0, 4000.0, 12000.0];
pub const MAX_GAIN_DB: f32 = 12.0;
const PEAK_Q: f32 = 1.0;

/// Settings shared with the audio callback.
pub struct EqShared {
    gains: [AtomicU32; EQ_BANDS],
    enabled: AtomicBool,
    version: AtomicU32,
}

impl EqShared {
    pub fn new(cfg: &EqualizerConfig) -> EqShared {
        let eq = EqShared {
            gains: std::array::from_fn(|_| AtomicU32::new(0)),
            enabled: AtomicBool::new(false),
            version: AtomicU32::new(0),
        };
        eq.set(cfg);
        eq
    }

    pub fn set(&self, cfg: &EqualizerConfig) {
        for (slot, gain) in self.gains.iter().zip(cfg.gains) {
            slot.store(gain.clamp(-MAX_GAIN_DB, MAX_GAIN_DB).to_bits(), Ordering::Relaxed);
        }
        self.enabled.store(cfg.enabled, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

enum Kind {
    LowShelf,
    Peak,
    HighShelf,
}

impl Biquad {
    /// RBJ "Audio EQ Cookbook" formulas.
    fn new(kind: Kind, freq: f32, gain_db: f32, rate: f32) -> Biquad {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq.min(rate * 0.45) / rate;
        let (sin, cos) = w0.sin_cos();
        let (b0, b1, b2, a0, a1, a2) = match kind {
            Kind::Peak => {
                let alpha = sin / (2.0 * PEAK_Q);
                (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            // Shelf slope 1: alpha = sin/2 * sqrt(2).
            Kind::LowShelf => {
                let k = 2.0 * a.sqrt() * (sin / 2.0 * 2f32.sqrt());
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            Kind::HighShelf => {
                let k = 2.0 * a.sqrt() * (sin / 2.0 * 2f32.sqrt());
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
        };
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }
}

/// The callback's private EQ state: coefficients and filter memory.
pub struct EqState {
    rate: f32,
    channels: usize,
    version: u32,
    /// Active (non-flat) bands only.
    filters: Vec<Biquad>,
    /// Per filter, per channel: (z1, z2).
    memory: Vec<[f32; 2]>,
    preamp: f32,
}

impl EqState {
    pub fn new(rate: u32, channels: u16) -> EqState {
        EqState {
            rate: rate as f32,
            channels: channels.max(1) as usize,
            version: u32::MAX,
            filters: Vec::new(),
            memory: Vec::new(),
            preamp: 1.0,
        }
    }

    fn update(&mut self, shared: &EqShared) {
        let version = shared.version.load(Ordering::Acquire);
        if version == self.version {
            return;
        }
        self.version = version;
        self.filters.clear();
        let gains: [f32; EQ_BANDS] = std::array::from_fn(|i| f32::from_bits(shared.gains[i].load(Ordering::Relaxed)));
        if shared.enabled.load(Ordering::Relaxed) {
            for (i, (&freq, &gain)) in BANDS.iter().zip(&gains).enumerate() {
                if gain.abs() < 0.05 {
                    continue;
                }
                let kind = match i {
                    0 => Kind::LowShelf,
                    i if i == EQ_BANDS - 1 => Kind::HighShelf,
                    _ => Kind::Peak,
                };
                self.filters.push(Biquad::new(kind, freq, gain, self.rate));
            }
        }
        // Boosts could clip: lower the overall level by the largest boost.
        let max_boost = if self.filters.is_empty() { 0.0 } else { gains.iter().copied().fold(0.0, f32::max) };
        self.preamp = 10f32.powf(-max_boost / 20.0);
        self.memory = vec![[0.0; 2]; self.filters.len() * self.channels];
    }

    /// Filters interleaved samples in place.
    pub fn process(&mut self, shared: &EqShared, samples: &mut [f32]) {
        self.update(shared);
        if self.filters.is_empty() {
            return;
        }
        for frame in samples.chunks_exact_mut(self.channels) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample * self.preamp;
                for (f, filter) in self.filters.iter().enumerate() {
                    let z = &mut self.memory[f * self.channels + ch];
                    // Transposed direct form II.
                    let y = filter.b0 * x + z[0];
                    z[0] = filter.b1 * x - filter.a1 * y + z[1];
                    z[1] = filter.b2 * x - filter.a2 * y;
                    // Keep decaying tails out of slow denormal territory.
                    if z[0].abs() < 1e-20 {
                        z[0] = 0.0;
                    }
                    if z[1].abs() < 1e-20 {
                        z[1] = 0.0;
                    }
                    x = y;
                }
                *sample = x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_at(freq: f32, cfg: &EqualizerConfig) -> f32 {
        // Steady-state amplitude of a sine through the EQ.
        let rate = 48_000.0;
        let shared = EqShared::new(cfg);
        let mut eq = EqState::new(rate as u32, 1);
        let mut buf: Vec<f32> = (0..48_000).map(|i| (2.0 * PI * freq * i as f32 / rate).sin()).collect();
        eq.process(&shared, &mut buf);
        buf[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn flat_or_disabled_is_transparent() {
        let flat = EqualizerConfig::default();
        assert!((response_at(1000.0, &flat) - 1.0).abs() < 1e-3);
        let off = EqualizerConfig { enabled: false, gains: [12.0; EQ_BANDS] };
        assert!((response_at(1000.0, &off) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn bass_boost_lifts_lows_relative_to_mids() {
        let bass = EqualizerConfig { enabled: true, gains: [9.0, 0.0, 0.0, 0.0, 0.0] };
        // Well below the 60 Hz corner, where the shelf reaches its full gain.
        let low = response_at(20.0, &bass);
        let mid = response_at(2000.0, &bass);
        // ~ +9 dB difference between the shelf and the untouched band.
        let diff_db = 20.0 * (low / mid).log10();
        assert!((diff_db - 9.0).abs() < 1.5, "diff {diff_db} dB");
        // The preamp keeps the boosted band from exceeding full scale.
        assert!(low <= 1.05);
    }
}

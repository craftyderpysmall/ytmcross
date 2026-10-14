//! Pitch shift without changing tempo, in the audio callback.
//!
//! Two classic steps (as SoundTouch does): WSOLA stretches the audio in
//! time by the pitch ratio without changing its pitch, then a cubic
//! resampler plays that back faster or slower by the same ratio, which
//! restores the length and moves the pitch. WSOLA splices ~80 ms pieces of
//! the input, choosing each splice point where the waveform lines up with
//! the previous piece, so there's no phasing or clicks.
//!
//! At 0 semitones it's bypassed entirely and holds no buffers; they are
//! sized once, the first time pitch is used (~0.2 MB), so the callback
//! never allocates while shifting.

use std::collections::VecDeque;

const SEQUENCE_SECS: f32 = 0.080;
const SEEK_SECS: f32 = 0.020;
const OVERLAP_SECS: f32 = 0.010;
/// Coarse search step for the splice point (refined around the best hit).
const COARSE_STEP: usize = 4;

pub struct PitchShifter {
    channels: usize,
    /// Pitch factor: 2^(semitones/12).
    ratio: f64,
    sequence: usize,
    seek: usize,
    overlap: usize,
    /// Interleaved input not consumed yet.
    input: Vec<f32>,
    /// Fractional part of the input skip, carried between splices.
    skip_frac: f64,
    /// Tail of the previous piece, cross-faded into the next one.
    mid: Vec<f32>,
    /// Time-stretched audio waiting for the resampler.
    stretched: Vec<f32>,
    /// Resampler read position in `stretched`, in frames.
    read: f64,
    started: bool,
}

impl PitchShifter {
    pub fn new(rate: u32, channels: u16) -> PitchShifter {
        let frames = |secs: f32| ((rate as f32 * secs) as usize).max(16);
        let channels = channels.max(1) as usize;
        let (sequence, seek, overlap) = (frames(SEQUENCE_SECS), frames(SEEK_SECS), frames(OVERLAP_SECS));
        PitchShifter {
            channels,
            ratio: 1.0,
            sequence,
            seek,
            overlap,
            input: Vec::new(),
            skip_frac: 0.0,
            mid: Vec::new(),
            stretched: Vec::new(),
            read: 1.0,
            started: false,
        }
    }

    /// Sizes the buffers, the first time they're needed.
    fn allocate(&mut self) {
        if self.mid.is_empty() {
            let ch = self.channels;
            self.input.reserve_exact((self.sequence + self.seek) * 2 * ch);
            self.mid = vec![0.0; self.overlap * ch];
            self.stretched.reserve_exact(self.sequence * 4 * ch);
        }
    }

    pub fn set_semitones(&mut self, semitones: i32) {
        self.ratio = 2f64.powf(semitones.clamp(-12, 12) as f64 / 12.0);
    }

    /// Forgets buffered audio (after a seek or track change).
    pub fn clear(&mut self) {
        self.input.clear();
        self.stretched.clear();
        self.skip_frac = 0.0;
        self.read = 1.0;
        self.started = false;
    }

    /// Fills `out` (interleaved) from `src`; returns how many samples were
    /// written (fewer when `src` runs dry).
    pub fn process(&mut self, src: &mut VecDeque<f32>, out: &mut [f32]) -> usize {
        self.allocate();
        let ch = self.channels;
        let mut written = 0;
        while written + ch <= out.len() {
            // The cubic needs frames read-1 ..= read+2.
            let need = self.read as usize + 3;
            if self.stretched.len() / ch < need {
                if !self.splice(src) {
                    break;
                }
                continue;
            }
            let i = self.read as usize;
            let t = (self.read - i as f64) as f32;
            for c in 0..ch {
                let s = |k: usize| self.stretched[k * ch + c];
                out[written + c] = catmull_rom(s(i - 1), s(i), s(i + 1), s(i + 2), t);
            }
            written += ch;
            self.read += self.ratio;
        }
        // Drop what the resampler has passed (keep one frame of history).
        let done = (self.read as usize).saturating_sub(1).min(self.stretched.len() / ch);
        if done > 0 {
            self.stretched.drain(..done * ch);
            self.read -= done as f64;
        }
        written
    }

    /// One WSOLA step: appends `sequence - overlap` stretched frames.
    /// Returns false when there isn't enough input yet.
    fn splice(&mut self, src: &mut VecDeque<f32>) -> bool {
        let ch = self.channels;
        // Enough for the search window, and for the whole skip when pitching
        // down (which consumes more input per piece than it outputs).
        let skip_max = ((self.sequence - self.overlap) as f64 / self.ratio).ceil() as usize + 1;
        let want = (self.sequence + self.seek).max(skip_max) * ch;
        if self.input.len() < want {
            let take = (want - self.input.len()).min(src.len());
            self.input.extend(src.drain(..take));
            if self.input.len() < want {
                return false;
            }
        }
        let ov = self.overlap * ch;
        if !self.started {
            // First piece: start from silence-free audio, nothing to align with.
            self.started = true;
            self.mid.copy_from_slice(&self.input[..ov]);
        }
        let offset = self.best_offset() * ch;
        // Cross-fade the previous tail into the new piece.
        for f in 0..self.overlap {
            let w = f as f32 / self.overlap as f32;
            for c in 0..ch {
                let k = f * ch + c;
                self.stretched.push(self.mid[k] * (1.0 - w) + self.input[offset + k] * w);
            }
        }
        let body_end = offset + (self.sequence - self.overlap) * ch;
        self.stretched.extend_from_slice(&self.input[offset + ov..body_end]);
        self.mid.copy_from_slice(&self.input[body_end..body_end + ov]);

        // Consume input at 1/ratio of the output rate: the result is longer
        // (or shorter) by `ratio`, at the original pitch.
        let skip = (self.sequence - self.overlap) as f64 / self.ratio + self.skip_frac;
        let whole = (skip as usize).min(self.input.len() / ch);
        self.skip_frac = skip - whole as f64;
        self.input.drain(..whole * ch);
        true
    }

    /// Where in the seek window the input best continues the previous piece
    /// (normalized cross-correlation of channel sums; coarse, then refined).
    fn best_offset(&self) -> usize {
        let ch = self.channels;
        let mono = |buf: &[f32], f: usize| buf[f * ch..f * ch + ch].iter().sum::<f32>();
        let score = |k: usize| {
            let (mut corr, mut energy) = (0.0f32, 1e-9f32);
            for f in 0..self.overlap {
                let x = mono(&self.input, k + f);
                corr += mono(&self.mid, f) * x;
                energy += x * x;
            }
            corr / energy.sqrt()
        };
        let best_in = |range: std::iter::StepBy<std::ops::Range<usize>>| {
            range.map(|k| (k, score(k))).fold((0, f32::MIN), |b, c| if c.1 > b.1 { c } else { b })
        };
        let (coarse, _) = best_in((0..self.seek).step_by(COARSE_STEP));
        let lo = coarse.saturating_sub(COARSE_STEP - 1);
        let hi = (coarse + COARSE_STEP).min(self.seek);
        best_in((lo..hi).step_by(1)).0
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Dominant frequency by counting zero crossings.
    fn frequency(samples: &[f32], rate: f32) -> f32 {
        let crossings = samples.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 * rate / samples.len() as f32
    }

    fn shift(semitones: i32, freq: f32) -> (Vec<f32>, usize) {
        let rate = 48_000;
        let mut p = PitchShifter::new(rate, 1);
        p.set_semitones(semitones);
        let input: Vec<f32> = (0..rate * 2).map(|i| (2.0 * PI * freq * i as f32 / rate as f32).sin() * 0.5).collect();
        let mut src: VecDeque<f32> = input.iter().copied().collect();
        let mut out = vec![0.0; rate as usize];
        let n = p.process(&mut src, &mut out);
        out.truncate(n);
        (out, src.len())
    }

    #[test]
    fn octave_up_doubles_frequency() {
        let (out, _) = shift(12, 220.0);
        let f = frequency(&out[4800..], 48_000.0);
        assert!((f - 440.0).abs() < 12.0, "got {f} Hz");
    }

    #[test]
    fn fifth_down_keeps_tempo() {
        let (out, left) = shift(-7, 440.0);
        let f = frequency(&out[4800..], 48_000.0);
        assert!((f - 440.0 * 2f32.powf(-7.0 / 12.0)).abs() < 10.0, "got {f} Hz");
        // One second out consumed about one second of input (plus look-ahead).
        let consumed = 96_000 - left;
        assert!((48_000..60_000).contains(&consumed), "consumed {consumed}");
    }
}

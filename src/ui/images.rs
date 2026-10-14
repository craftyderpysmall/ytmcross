//! Album art: fetched and decoded off the UI thread at exactly the size it
//! is drawn, kept only in GPU textures, and evicted once it hasn't been on
//! screen for a while, so memory stays flat no matter how far you scroll.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};

const WORKERS: usize = 4;
const FADE_IN: f32 = 0.2;
const KEEP_UNUSED: Duration = Duration::from_secs(45);
const EVICT_EVERY: Duration = Duration::from_secs(10);

enum State {
    Pending,
    Ready { texture: TextureHandle, loaded: Instant },
    Failed,
}

struct Slot {
    state: State,
    last_used: Instant,
}

type Store = Arc<Mutex<HashMap<String, Slot>>>;

pub struct Images {
    store: Store,
    jobs: Sender<(String, u32)>,
    last_evict: Instant,
}

impl Images {
    pub fn new(ctx: &egui::Context, agent: ureq::Agent) -> Images {
        let store: Store = Arc::default();
        let (jobs, rx) = mpsc::channel::<(String, u32)>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..WORKERS {
            let (rx, store, ctx, agent) = (rx.clone(), store.clone(), ctx.clone(), agent.clone());
            std::thread::Builder::new()
                .name(format!("art-{i}"))
                .spawn(move || {
                    loop {
                        let Ok((url, px)) = rx.lock().unwrap().recv() else { return };
                        let state = match fetch(&agent, &url, px) {
                            Some(image) => State::Ready {
                                texture: ctx.load_texture(&url, image, TextureOptions::LINEAR),
                                loaded: Instant::now(),
                            },
                            None => State::Failed,
                        };
                        if let Some(slot) = store.lock().unwrap().get_mut(&url) {
                            slot.state = state;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("failed to start art worker");
        }
        Images { store, jobs, last_evict: Instant::now() }
    }

    /// Texture and fade-in opacity for `url` drawn at `px` physical pixels.
    /// Starts loading it on first use. Returns None while not ready.
    pub fn get(&self, ctx: &egui::Context, url: &str, px: u32) -> Option<(TextureId, f32)> {
        let key = sized_url(url, px);
        let now = Instant::now();
        let mut store = self.store.lock().unwrap();
        let slot = store.entry(key.clone()).or_insert_with(|| {
            let _ = self.jobs.send((key, px));
            Slot { state: State::Pending, last_used: now }
        });
        slot.last_used = now;
        match &slot.state {
            State::Ready { texture, loaded } => {
                let t = (now.duration_since(*loaded).as_secs_f32() / FADE_IN).min(1.0);
                if t < 1.0 {
                    ctx.request_repaint();
                }
                Some((texture.id(), egui::emath::easing::cubic_out(t)))
            }
            State::Pending | State::Failed => None,
        }
    }

    /// Drops textures that haven't been drawn recently. Cheap; called every frame,
    /// does real work at most every few seconds.
    pub fn evict(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_evict) < EVICT_EVERY {
            return;
        }
        self.last_evict = now;
        self.store.lock().unwrap().retain(|_, slot| match slot.state {
            State::Pending => true,
            _ => now.duration_since(slot.last_used) < KEEP_UNUSED,
        });
    }
}

/// Google's image CDN resizes on the fly: ask for exactly what we draw.
fn sized_url(url: &str, px: u32) -> String {
    if (url.contains("googleusercontent.com") || url.contains("ggpht.com"))
        && let Some(i) = url.rfind('=')
    {
        return format!("{}=w{px}-h{px}-l90-rj", &url[..i]);
    }
    url.to_string()
}

fn fetch(agent: &ureq::Agent, url: &str, px: u32) -> Option<ColorImage> {
    let bytes = agent.get(url).call().ok()?.body_mut().read_to_vec().ok()?;
    let (rgba, w, h) = decode(&bytes)?;
    // Video thumbnails are 16:9 with bars; crop the centre square.
    let side = w.min(h);
    let px = (px as usize).clamp(16, 512).min(side);
    let out = shrink(&rgba, w, ((w - side) / 2, (h - side) / 2), side, px);
    Some(ColorImage::from_rgba_unmultiplied([px, px], &out))
}

/// RGBA pixels and size of a JPEG (art and thumbnails) or PNG (icons).
fn decode(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    if bytes.starts_with(b"\x89PNG") {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[l, a]| [l, l, l, a]).collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return None, // expanded by the transformations
        };
        return Some((rgba, info.width as usize, info.height as usize));
    }
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let options = DecoderOptions::default().set_strict_mode(false).jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let rgba = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    (rgba.len() == w * h * 4).then_some((rgba, w, h))
}

/// The `side` x `side` square at `at` of an RGBA image `width` pixels wide,
/// shrunk to `px` x `px` (`px` <= `side`): each pixel the mean of its block.
fn shrink(rgba: &[u8], width: usize, at: (usize, usize), side: usize, px: usize) -> Vec<u8> {
    let span = |i: usize| (i * side / px, (i + 1) * side / px);
    let mut out = Vec::with_capacity(px * px * 4);
    for oy in 0..px {
        let (y0, y1) = span(oy);
        for ox in 0..px {
            let (x0, x1) = span(ox);
            let mut sum = [0u32; 4];
            for y in at.1 + y0..at.1 + y1 {
                let row = &rgba[(y * width + at.0 + x0) * 4..(y * width + at.0 + x1) * 4];
                for p in row.as_chunks::<4>().0 {
                    sum.iter_mut().zip(p).for_each(|(s, &c)| *s += u32::from(c));
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_averages_blocks_of_the_centre_square() {
        // 4x2 image: the centre 2x2 square is columns 1..3.
        let px = |v: u8| [v, v, v, 255];
        let rgba: Vec<u8> = [0, 10, 30, 0, 0, 50, 70, 0].iter().flat_map(|&v| px(v)).collect();
        assert_eq!(shrink(&rgba, 4, (1, 0), 2, 1), px(40).to_vec());
        assert_eq!(shrink(&rgba, 4, (1, 0), 2, 2), [px(10), px(30), px(50), px(70)].concat());
    }
}

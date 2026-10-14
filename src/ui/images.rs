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
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    // Video thumbnails are 16:9 with bars; crop the centre square.
    let (w, h) = img.dimensions();
    let side = w.min(h);
    let square = image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
    let px = px.clamp(16, 512);
    let out = if side > px { image::imageops::thumbnail(&square, px, px) } else { square };
    Some(ColorImage::from_rgba_unmultiplied([out.width() as usize, out.height() as usize], out.as_raw()))
}

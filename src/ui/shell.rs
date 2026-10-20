//! The window and event loop. egui is rasterized on the CPU: no GPU driver
//! is ever loaded, which is most of the memory a GPU-backed egui app uses.
//!
//! Frame policy, all in one place:
//! - Something woke us (input, a worker message, an animation tick):
//!   run `App::logic` and schedule a frame.
//! - Frames are paced to the monitor's refresh rate, never faster; while
//!   animating, on Windows, in step with the compositor's vblank.
//! - A frame lays the UI out, then rasterizes only what changed since the
//!   previous one (damage.rs): often a row, or nothing at all.
//! - Minimized or fully covered: logic still runs (the queue keeps
//!   advancing), but nothing is laid out or drawn.
//! - Nothing scheduled: the thread sleeps in the OS until the next event,
//!   and the frame buffer has been given back (present.rs).

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use super::app::App;
use super::clipboard::Clipboard;
use super::damage::{self, Damage, Region, Scene};
use super::present::{Pixels, Presenter};

/// The frame buffer is kept when the next frame is due sooner than this.
const KEEP_BUFFER: Duration = Duration::from_millis(100);

/// Sent from any thread to wake the event loop.
enum Wake {
    /// Run the app's logic, then draw after this delay.
    After(Duration),
    /// A taskbar thumbnail button or a system-wide shortcut.
    #[cfg(windows)]
    Control(crate::config::Control),
}

struct Surface {
    /// Dropped first: on Wayland it holds the window's display.
    clipboard: Clipboard,
    window: Rc<Window>,
    presenter: Presenter,
    egui_winit: egui_winit::State,
}

struct Shell {
    ctx: egui::Context,
    app: App,
    viewport: egui::ViewportBuilder,
    renderer: EguiSoftwareRender,
    win: Option<Surface>,
    occluded: bool,
    next_frame: Option<Instant>,
    /// Earliest start of the next frame (refresh-rate pacing).
    ready_at: Instant,
    frame_interval: Duration,
    /// What the window shows, to find what the next frame changes.
    scene: Scene,
    /// Window size, scale and fonts `scene` was drawn with.
    drawn: ([u32; 2], f32, usize),
    /// The window's content can't be trusted: redraw all of it.
    redraw_all: bool,
    /// We asked for the coming redraw (others come from the system).
    requested: bool,
    /// Held modifier keys (to spot a paste).
    modifiers: winit::keyboard::ModifiersState,
    /// `YTMCROSS_STATS=1`: frame time and share of pixels drawn, in the title.
    stats: Option<Stats>,
    first_frame: bool,
    #[cfg(windows)]
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

#[derive(Default)]
struct Stats {
    frames: u32,
    /// Layout, damage, tessellation, rasterization.
    phases: [Duration; 4],
    drawn: u64,
    total: u64,
}

pub fn run(
    viewport: egui::ViewportBuilder,
    make_app: impl FnOnce(&egui::Context, crate::worker::Waker) -> App,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let ctx = egui::Context::default();
    let proxy = event_loop.create_proxy();
    let waker = proxy.clone();
    #[cfg(windows)]
    let thumbs = proxy.clone();
    ctx.set_request_repaint_callback(move |info| {
        let _ = proxy.send_event(Wake::After(info.delay));
    });
    crate::trace!("shell: event loop ready");
    let app = make_app(&ctx, Arc::new(move || drop(waker.send_event(Wake::After(Duration::ZERO)))));
    crate::trace!("shell: app created");
    let mut shell = Shell {
        // Measured: the per-primitive cache doubles our RAM (22 vs 12 MB) and is
        // slower while scrolling (5.7 vs 2.6 ms/frame), since scrolling
        // invalidates it every frame anyway.
        renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra).with_caching(false),
        stats: std::env::var_os("YTMCROSS_STATS").map(|_| Stats::default()),
        first_frame: true,
        #[cfg(windows)]
        proxy: thumbs,
        ctx,
        app,
        viewport,
        win: None,
        occluded: false,
        next_frame: Some(Instant::now()),
        ready_at: Instant::now(),
        frame_interval: Duration::from_micros(16_667),
        scene: Scene::default(),
        drawn: ([0, 0], 0.0, 0),
        redraw_all: true,
        requested: false,
        modifiers: Default::default(),
    };
    event_loop.run_app(&mut shell)?;
    shell.app.on_exit();
    Ok(())
}

impl Shell {
    fn schedule(&mut self, when: Instant) {
        self.next_frame = Some(self.next_frame.map_or(when, |t| t.min(when)));
    }

    fn visible(&self) -> bool {
        !self.occluded && self.win.as_ref().is_some_and(|w| w.window.is_minimized() != Some(true))
    }

    fn paint(&mut self) {
        let Some(win) = self.win.as_mut() else { return };
        let started = Instant::now();
        let size = win.window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        // Before egui runs: the OS may have dropped what the window showed.
        let lost = win.presenter.invalidated(!std::mem::take(&mut self.requested));

        let input = win.egui_winit.take_egui_input(&win.window);
        let app = &mut self.app;
        let out = self.ctx.run_ui(input, |ui| app.ui(ui));
        for command in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                win.clipboard.set(&win.window, text);
            }
        }
        win.egui_winit.handle_platform_output(&win.window, out.platform_output);
        let ppp = out.pixels_per_point;
        let t_ui = started.elapsed();

        let (scene, shapes) = Scene::new(out.shapes, ppp);
        // New fonts (another script's, or the theme's) can change glyphs
        // without changing any layout: redraw everything.
        let fonts = self.ctx.fonts(|f| f.definitions().font_data.values().fold(0, |k, d| k * 31 + Arc::as_ptr(d) as usize));
        let all = std::mem::take(&mut self.redraw_all) | lost | (self.drawn != ([w, h], ppp, fonts));
        let damage = match if all { Damage::Full } else { damage::diff(&self.scene, &scene, w, h) } {
            Damage::None => Vec::new(),
            Damage::Regions(r) => r,
            Damage::Full => vec![Region::full(w, h)],
        };
        let t_scene = started.elapsed();
        let mut t_tess = t_scene;
        let mut regions = Vec::new();
        if damage.is_empty() {
            if !out.textures_delta.is_empty() {
                // Nothing to draw, but the renderer's textures must keep up.
                let mut px = [[0u8; 4]];
                self.renderer.render(&mut BufferMutRef::new(&mut px, 1, 1), &[], &out.textures_delta, ppp);
            }
        } else {
            let (ctx, renderer) = (&self.ctx, &mut self.renderer);
            let (delta, none) = (&out.textures_delta, egui::TexturesDelta::default());
            // Wayland: lets the compositor's frame callbacks pace us.
            win.window.pre_present_notify();
            // The presenter may draw more than the damage (present.rs).
            regions = win.presenter.draw(w, h, damage, |pixels, regions| {
                let full = regions.first() == Some(&Region::full(w, h));
                let shapes = if full { shapes } else { scene.touching(shapes, regions) };
                let mut primitives = ctx.tessellate(shapes, ppp);
                if full {
                    damage::seal(&mut primitives);
                }
                t_tess = started.elapsed();
                for (i, r) in regions.iter().enumerate() {
                    let delta = if i == 0 { delta } else { &none };
                    clear(pixels, w, r);
                    let target = &mut BufferMutRef::new(pixels, w as usize, h as usize);
                    if full {
                        renderer.render(target, &primitives, delta, ppp);
                    } else {
                        let mut part = damage::clip(&primitives, *r, ppp);
                        damage::seal(&mut part);
                        renderer.render(target, &part, delta, ppp);
                    }
                }
            });
        }
        self.scene = scene;
        self.drawn = ([w, h], ppp, fonts);

        if self.first_frame {
            self.first_frame = false;
            crate::trace!("shell: first frame on screen ({w}x{h}, drawn in {:?}, ui {t_ui:?})", started.elapsed());
        }
        if let Some(s) = &mut self.stats {
            s.frames += 1;
            s.phases[0] += t_ui;
            s.phases[1] += t_scene - t_ui;
            s.phases[2] += t_tess - t_scene;
            s.phases[3] += started.elapsed() - t_tess;
            s.drawn += regions.iter().map(|r| u64::from(r.width()) * u64::from(r.height())).sum::<u64>();
            s.total += u64::from(w) * u64::from(h);
            if s.frames == 30 {
                let ms = s.phases.map(|p| p.as_secs_f64() * 1000.0 / 30.0);
                let share = s.drawn as f64 * 100.0 / s.total as f64;
                win.window.set_title(&format!(
                    "YouTube Music - {:.2} ms/frame: ui {:.2}, diff {:.2}, tessellate {:.2}, raster {:.2}; {share:.0}% drawn",
                    ms.iter().sum::<f64>(),
                    ms[0],
                    ms[1],
                    ms[2],
                    ms[3]
                ));
                *s = Stats::default();
            }
        }

        // The next frame may start one refresh after this one started.
        self.ready_at = started + self.frame_interval;
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |vp| vp.repaint_delay);
        if delay.is_zero() {
            // Animating: wait for the compositor's next vblank, so frames are
            // evenly spaced and in step with the display instead of judder.
            #[cfg(windows)]
            // SAFETY: no arguments; blocks until the next DWM composition.
            if unsafe { windows_sys::Win32::Graphics::Dwm::DwmFlush() } >= 0 {
                self.ready_at = Instant::now();
            }
        }
        // Between animation frames the buffer is kept; otherwise it goes
        // back now (redrawing a small region later touches only its pages).
        if delay > KEEP_BUFFER {
            win.presenter.release();
        }
        if delay < Duration::from_secs(3600) {
            self.schedule(Instant::now() + delay);
        }
    }
}

/// Ctrl+V (Cmd+V on macOS), Shift+Insert or the Paste key, as egui-winit
/// recognizes them.
fn is_paste(key: &winit::event::KeyEvent, modifiers: winit::keyboard::ModifiersState) -> bool {
    use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
    let command = if cfg!(target_os = "macos") { modifiers.super_key() } else { modifiers.control_key() };
    let v = matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("v"))
        || key.physical_key == PhysicalKey::Code(KeyCode::KeyV);
    let insert = key.logical_key == Key::Named(NamedKey::Insert) || key.physical_key == PhysicalKey::Code(KeyCode::Insert);
    key.logical_key == Key::Named(NamedKey::Paste) || (command && v) || (modifiers.shift_key() && insert)
}

/// Clears a region before drawing it, as a full frame starts from black.
fn clear(pixels: &mut [Pixels], w: u32, r: &Region) {
    for y in r.y0..r.y1 {
        let row = (y * w) as usize;
        pixels[row + r.x0 as usize..row + r.x1 as usize].fill([0; 4]);
    }
}

impl ApplicationHandler<Wake> for Shell {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        let window = match egui_winit::create_window(&self.ctx, el, &self.viewport) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("ytmcross: could not create window: {e}");
                el.exit();
                return;
            }
        };
        // Title bar and taskbar icon: the exe's own (build.rs), loaded by the OS.
        #[cfg(windows)]
        {
            use winit::platform::windows::{IconExtWindows, WindowExtWindows};
            let icon = winit::window::Icon::from_resource(1, None).ok();
            window.set_window_icon(icon.clone());
            window.set_taskbar_icon(icon);
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
                let proxy = self.proxy.clone();
                super::win32::install(h.hwnd.get() as _, move |c| drop(proxy.send_event(Wake::Control(c))));
            }
        }
        if let Some(mhz) = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).filter(|&m| m > 0) {
            self.frame_interval = Duration::from_secs_f64(1000.0 / mhz as f64);
        }
        let presenter = match Presenter::new(el, &window) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ytmcross: could not create surface: {e}");
                el.exit();
                return;
            }
        };
        let egui_winit = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        self.win = Some(Surface { clipboard: Clipboard::default(), window, presenter, egui_winit });
        self.schedule(Instant::now());
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, wake: Wake) {
        let delay = match wake {
            Wake::After(delay) => delay,
            #[cfg(windows)]
            Wake::Control(c) => {
                self.app.control(c);
                Duration::ZERO
            }
        };
        self.app.logic(&self.ctx);
        self.schedule(Instant::now() + delay);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(w) = &self.win {
                    w.window.set_visible(false);
                }
                el.exit();
            }
            WindowEvent::RedrawRequested => self.paint(),
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if occluded {
                    if let Some(w) = &mut self.win {
                        w.presenter.release();
                    }
                } else {
                    self.redraw_all = true;
                    self.schedule(Instant::now());
                }
            }
            event => {
                let Some(win) = self.win.as_mut() else { return };
                match &event {
                    WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
                    // egui-winit pastes from its own clipboard: fill it first.
                    WindowEvent::KeyboardInput { event: key, .. } if key.state.is_pressed() && is_paste(key, self.modifiers) => {
                        win.egui_winit.set_clipboard_text(win.clipboard.get(&win.window).unwrap_or_default());
                    }
                    _ => {}
                }
                if win.egui_winit.on_window_event(&win.window, &event).repaint {
                    self.schedule(Instant::now());
                }
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        #[cfg(windows)]
        {
            let loaded = self.app.now_playing().is_some();
            super::win32::update(loaded, loaded && !self.app.playback.paused);
            // Let go while one is being set in Preferences, so its keys reach us.
            let keys = if self.app.recording.is_some() { [None; 3] } else { self.app.config.keys.0 };
            let taken = super::win32::set_hotkeys(keys);
            if !taken.is_empty() {
                let names: Vec<String> = taken.iter().map(|b| b.to_string()).collect();
                self.app.notify(format!("{} can't be used system-wide (another app has it): only in this window", names.join(", ")));
                self.schedule(Instant::now());
            }
        }
        if let Some(when) = self.next_frame {
            let when = when.max(self.ready_at);
            if Instant::now() >= when {
                self.next_frame = None;
                // Hidden: drop the frame. Showing the window again triggers one.
                if self.visible()
                    && let Some(w) = &self.win
                {
                    self.requested = true;
                    w.window.request_redraw();
                }
                el.set_control_flow(ControlFlow::Wait);
            } else {
                el.set_control_flow(ControlFlow::WaitUntil(when));
            }
        } else {
            el.set_control_flow(ControlFlow::Wait);
        }
    }
}

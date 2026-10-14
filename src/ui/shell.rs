//! The window and event loop. egui is rasterized on the CPU into a
//! softbuffer surface: no GPU driver is ever loaded, which is most of the
//! memory a GPU-backed egui app uses. The UI redraws rarely, so the CPU
//! cost of drawing it ourselves is negligible.
//!
//! Frame policy, all in one place:
//! - Something woke us (input, a worker message, an animation tick):
//!   run `App::logic` and schedule a frame.
//! - Frames are paced to the monitor's refresh rate, never faster; while
//!   animating, on Windows, in step with the compositor's vblank.
//! - Minimized or fully covered: logic still runs (the queue keeps
//!   advancing), but nothing is laid out or drawn.
//! - Nothing scheduled: the thread sleeps in the OS until the next event.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::window::{Window, WindowId};

use super::app::App;

/// Sent from any thread to wake the event loop. Carries the requested delay.
struct Wake(Duration);

struct Surface {
    window: Rc<Window>,
    surface: softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>,
    egui_winit: egui_winit::State,
}

struct Shell {
    ctx: egui::Context,
    app: App,
    viewport: egui::ViewportBuilder,
    softbuffer: softbuffer::Context<OwnedDisplayHandle>,
    renderer: EguiSoftwareRender,
    win: Option<Surface>,
    occluded: bool,
    next_frame: Option<Instant>,
    /// Earliest start of the next frame (refresh-rate pacing).
    ready_at: Instant,
    frame_interval: Duration,
    /// `YTMCROSS_STATS=1`: average frame time shown in the window title.
    stats: Option<(Duration, u32)>,
    first_frame: bool,
}

pub fn run(
    viewport: egui::ViewportBuilder,
    make_app: impl FnOnce(&egui::Context) -> App,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let ctx = egui::Context::default();
    let proxy = event_loop.create_proxy();
    ctx.set_request_repaint_callback(move |info| {
        let _ = proxy.send_event(Wake(info.delay));
    });
    crate::trace!("shell: event loop ready");
    let app = make_app(&ctx);
    crate::trace!("shell: app created");
    let mut shell = Shell {
        softbuffer: softbuffer::Context::new(event_loop.owned_display_handle())?,
        // Measured: the per-primitive cache doubles our RAM (22 vs 12 MB) and is
        // slower while scrolling (5.7 vs 2.6 ms/frame), since scrolling
        // invalidates it every frame anyway.
        renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra).with_caching(false),
        stats: std::env::var_os("YTMCROSS_STATS").map(|_| (Duration::ZERO, 0)),
        first_frame: true,
        ctx,
        app,
        viewport,
        win: None,
        occluded: false,
        next_frame: Some(Instant::now()),
        ready_at: Instant::now(),
        frame_interval: Duration::from_micros(16_667),
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

    fn paint(&mut self) -> Result<(), softbuffer::SoftBufferError> {
        let Some(win) = self.win.as_mut() else { return Ok(()) };
        let started = Instant::now();
        let size = win.window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        win.surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())?;

        let input = win.egui_winit.take_egui_input(&win.window);
        let app = &mut self.app;
        let out = self.ctx.run_ui(input, |ui| app.ui(ui));
        win.egui_winit.handle_platform_output(&win.window, out.platform_output);
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);

        let mut buffer = win.surface.buffer_mut()?;
        buffer.fill(0);
        let target = &mut BufferMutRef::new(bytemuck::cast_slice_mut(&mut buffer), w as usize, h as usize);
        self.renderer.render(target, &primitives, &out.textures_delta, out.pixels_per_point);
        buffer.present()?;
        if self.first_frame {
            self.first_frame = false;
            crate::trace!("shell: first frame on screen ({w}x{h}, drawn in {:?})", started.elapsed());
        }

        if let Some((total, frames)) = &mut self.stats {
            *total += started.elapsed();
            *frames += 1;
            if *frames == 30 {
                let ms = total.as_secs_f64() * 1000.0 / 30.0;
                win.window.set_title(&format!("ytmcross - {ms:.2} ms/frame"));
                (*total, *frames) = (Duration::ZERO, 0);
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
        if delay < Duration::from_secs(3600) {
            self.schedule(Instant::now() + delay);
        }
        Ok(())
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
        if let Some(mhz) = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).filter(|&m| m > 0) {
            self.frame_interval = Duration::from_secs_f64(1000.0 / mhz as f64);
        }
        let surface = match softbuffer::Surface::new(&self.softbuffer, window.clone()) {
            Ok(s) => s,
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
        self.win = Some(Surface { window, surface, egui_winit });
        self.schedule(Instant::now());
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, Wake(delay): Wake) {
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
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.paint() {
                    eprintln!("ytmcross: paint failed: {e}");
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if !occluded {
                    self.schedule(Instant::now());
                }
            }
            event => {
                let Some(win) = self.win.as_mut() else { return };
                if win.egui_winit.on_window_event(&win.window, &event).repaint {
                    self.schedule(Instant::now());
                }
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(when) = self.next_frame else {
            el.set_control_flow(ControlFlow::Wait);
            return;
        };
        let when = when.max(self.ready_at);
        if Instant::now() >= when {
            self.next_frame = None;
            // Hidden: drop the frame. Showing the window again triggers one.
            if self.visible()
                && let Some(w) = &self.win
            {
                w.window.request_redraw();
            }
            el.set_control_flow(ControlFlow::Wait);
        } else {
            el.set_control_flow(ControlFlow::WaitUntil(when));
        }
    }
}

//! Getting rendered pixels onto the window: our own frame buffer, shared
//! with the compositor over wl_shm. Only the regions that changed are redrawn
//! and declared damaged, and the buffer's untouched pages never become
//! resident. Some time after the last draw it is given back: idle, no frame
//! buffer exists at all, and the compositor keeps showing what it was given.
//!
//! A Wayland buffer must always be complete, so unlike a window that keeps
//! its own pixels, drawing again after a release means drawing everything
//! (`needs_full`).

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::Window;

use super::damage::Region;

pub type Pixels = [u8; 4];

/// Receives what the compositor sends us: only buffer releases matter.
struct Events;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Events {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for Events {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, busy: &Arc<AtomicBool>, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            busy.store(false, Ordering::Relaxed);
        }
    }
}

delegate_noop!(Events: ignore wl_shm::WlShm);
delegate_noop!(Events: ignore wl_shm_pool::WlShmPool);

/// Mapped but untouched: pages turn resident only once drawn into.
struct Frame {
    pixels: *mut Pixels,
    width: u32,
    height: u32,
    buffer: wl_buffer::WlBuffer,
    /// The compositor may be reading it: it must not change.
    busy: Arc<AtomicBool>,
}

impl Frame {
    fn new(shm: &wl_shm::WlShm, queue: &QueueHandle<Events>, width: u32, height: u32) -> Option<Frame> {
        let bytes = width as usize * height as usize * 4;
        // SAFETY: plain calls; every result is checked, and the fd is owned
        // from the moment it exists. The zero-filled file backs the mapping.
        unsafe {
            let fd = libc::memfd_create(c"ytmcross".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let fd = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(std::os::fd::AsRawFd::as_raw_fd(&fd), bytes as libc::off_t) != 0 {
                return None;
            }
            let flags = libc::PROT_READ | libc::PROT_WRITE;
            let pixels = libc::mmap(ptr::null_mut(), bytes, flags, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&fd), 0);
            if pixels == libc::MAP_FAILED {
                return None;
            }
            let busy = Arc::new(AtomicBool::new(false));
            let pool = shm.create_pool(fd.as_fd(), bytes as i32, queue, ());
            // Little-endian XRGB8888 is bytes B, G, R, X: the renderer's order.
            let stride = width as i32 * 4;
            let format = wl_shm::Format::Xrgb8888;
            let buffer = pool.create_buffer(0, width as i32, height as i32, stride, format, queue, busy.clone());
            // The buffer keeps what it needs of the pool, and we the mapping.
            pool.destroy();
            Some(Frame { pixels: pixels.cast(), width, height, buffer, busy })
        }
    }

    fn pixels(&mut self) -> &mut [Pixels] {
        // SAFETY: the mapping holds exactly width * height pixels, zeroed by the OS.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.width as usize * self.height as usize) }
    }

    fn busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.buffer.destroy();
        // SAFETY: mapped in `new` with this size and not referenced any more.
        unsafe { libc::munmap(self.pixels.cast(), self.width as usize * self.height as usize * 4) };
    }
}

pub struct Presenter {
    connection: Connection,
    queue: EventQueue<Events>,
    surface: wl_surface::WlSurface,
    shm: wl_shm::WlShm,
    frame: Option<Frame>,
    /// A second buffer, only if the compositor held on to the first.
    spare: Option<Frame>,
}

impl Presenter {
    pub fn new(window: &Window) -> Result<Presenter, String> {
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) = (
            window.display_handle().map_err(|e| e.to_string())?.as_raw(),
            window.window_handle().map_err(|e| e.to_string())?.as_raw(),
        ) else {
            return Err("not a Wayland window".into());
        };
        // SAFETY: winit's display and surface, valid as long as the window,
        // which the shell keeps for as long as this.
        let (connection, id) = unsafe {
            let backend = Backend::from_foreign_display(display.display.as_ptr().cast());
            let id = ObjectId::from_ptr(wl_surface::WlSurface::interface(), surface.surface.as_ptr().cast());
            (Connection::from_backend(backend), id.map_err(|e| e.to_string())?)
        };
        let surface = wl_surface::WlSurface::from_id(&connection, id).map_err(|e| e.to_string())?;
        let (globals, queue) = registry_queue_init::<Events>(&connection).map_err(|e| e.to_string())?;
        let shm = globals.bind(&queue.handle(), 1..=1, ()).map_err(|e| e.to_string())?;
        Ok(Presenter { connection, queue, surface, shm, frame: None, spare: None })
    }

    /// No complete frame of this size exists: redraw all of it.
    pub fn needs_full(&self, w: u32, h: u32) -> bool {
        self.frame.as_ref().is_none_or(|f| f.width != w || f.height != h)
    }

    /// Has `render` fill the regions it is given in the `w` x `h` frame,
    /// and puts them on screen. Returns the regions drawn.
    pub fn draw(
        &mut self,
        window: &Window,
        w: u32,
        h: u32,
        damage: Vec<Region>,
        render: impl FnOnce(&mut [Pixels], &[Region]),
    ) -> Vec<Region> {
        let _ = self.queue.dispatch_pending(&mut Events);
        if self.needs_full(w, h) {
            self.frame = None;
            self.spare = None;
            self.frame = Frame::new(&self.shm, &self.queue.handle(), w, h);
        }
        self.avoid_busy(w, h);
        let Some(frame) = &mut self.frame else { return Vec::new() };
        let regions = damage;
        render(frame.pixels(), &regions);
        frame.busy.store(true, Ordering::Relaxed);
        self.surface.attach(Some(&frame.buffer), 0, 0);
        for r in &regions {
            self.surface.damage_buffer(r.x0 as i32, r.y0 as i32, r.width() as i32, r.height() as i32);
        }
        // Asks for the frame callback that paces the next redraw. It has to
        // go with this commit, and it never fires for a window out of view.
        window.pre_present_notify();
        self.surface.commit();
        let _ = self.connection.flush();
        regions
    }

    /// The compositor still holds the frame: carry on in the spare one.
    fn avoid_busy(&mut self, w: u32, h: u32) {
        if !self.frame.as_ref().is_some_and(Frame::busy) {
            return;
        }
        let spare = self.spare.take().filter(|s| !s.busy() && s.width == w && s.height == h);
        let Some(mut next) = spare.or_else(|| Frame::new(&self.shm, &self.queue.handle(), w, h)) else { return };
        if let Some(frame) = &mut self.frame {
            next.pixels().copy_from_slice(frame.pixels());
        }
        self.spare = self.frame.replace(next);
    }

    /// Gives the frame buffers back to the OS, once the compositor is done
    /// with them. Returns false if it isn't yet.
    pub fn release(&mut self) -> bool {
        let _ = self.queue.dispatch_pending(&mut Events);
        if self.frame.as_ref().is_some_and(Frame::busy) {
            return false;
        }
        self.frame = None;
        self.spare = None;
        let _ = self.connection.flush();
        true
    }
}

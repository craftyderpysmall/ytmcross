//! Getting rendered pixels onto the window.
//!
//! Windows: our own frame buffer, copied to the window with GDI. Only the
//! regions that changed are copied (the window keeps showing the rest), the
//! buffer's untouched pages never become resident, and between bursts of
//! drawing it is given back to the OS: idle, no frame buffer exists at all.
//!
//! Elsewhere: a softbuffer surface. Its buffers may be swapped or
//! recreated, so each says how many frames old its content is: a buffer
//! one frame old gets just the damage, two frames old the damage of both
//! frames, anything else a full frame. Only the damage is presented.
//! Backends whose buffers always come back blank (macOS) draw into a copy
//! of the frame kept while drawing goes on, given back when idle.

use std::rc::Rc;

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use super::damage::Region;

pub type Pixels = [u8; 4];

/// What to draw into a buffer whose content is `age` frames old (0:
/// unknown): this frame's damage, plus the previous frame's (`last`) if the
/// buffer missed it; everything when that's most of the window anyway.
#[cfg_attr(windows, allow(dead_code))] // softbuffer platforms; tested everywhere
fn for_buffer(age: u8, damage: Vec<Region>, last: Vec<Region>, w: u32, h: u32) -> Vec<Region> {
    let full = Region::full(w, h);
    let regions = match age {
        1 => damage,
        2 => [damage, last].concat(),
        _ => return vec![full],
    };
    let area: u64 = regions.iter().map(|r| u64::from(r.width()) * u64::from(r.height())).sum();
    if regions.contains(&full) || area * 2 > u64::from(w) * u64::from(h) { vec![full] } else { regions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_get_what_they_missed() {
        let a = Region { x0: 0, y0: 0, x1: 10, y1: 10 };
        let b = Region { x0: 50, y0: 50, x1: 60, y1: 60 };
        let full = Region::full(100, 100);
        assert_eq!(for_buffer(1, vec![a], vec![b], 100, 100), vec![a]);
        assert_eq!(for_buffer(2, vec![a], vec![b], 100, 100), vec![a, b]);
        assert_eq!(for_buffer(0, vec![a], vec![b], 100, 100), vec![full]);
        assert_eq!(for_buffer(3, vec![a], vec![b], 100, 100), vec![full]);
        assert_eq!(for_buffer(2, vec![a], vec![full], 100, 100), vec![full]);
        let big = Region { x0: 0, y0: 0, x1: 100, y1: 60 };
        assert_eq!(for_buffer(1, vec![big], vec![], 100, 100), vec![full]);
    }
}

#[cfg(windows)]
pub use win32::Presenter;

#[cfg(not(windows))]
pub use soft::Presenter;

#[cfg(windows)]
mod win32 {
    use std::ptr;

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetUpdateRect, RGBQUAD, ReleaseDC, SRCCOPY,
        StretchDIBits,
    };
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::*;

    /// Committed but untouched: pages turn resident only once drawn into.
    struct Frame {
        pixels: *mut Pixels,
        width: u32,
        height: u32,
    }

    impl Frame {
        fn new(width: u32, height: u32) -> Option<Frame> {
            let bytes = width as usize * height as usize * 4;
            // SAFETY: a plain allocation; null means failure.
            let pixels = unsafe { VirtualAlloc(ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
            (!pixels.is_null()).then(|| Frame { pixels: pixels.cast(), width, height })
        }

        fn pixels(&mut self) -> &mut [Pixels] {
            // SAFETY: the allocation holds exactly width * height pixels, zeroed by the OS.
            unsafe { std::slice::from_raw_parts_mut(self.pixels, self.width as usize * self.height as usize) }
        }
    }

    impl Drop for Frame {
        fn drop(&mut self) {
            // SAFETY: allocated by VirtualAlloc above and not referenced any more.
            unsafe { VirtualFree(self.pixels.cast(), 0, MEM_RELEASE) };
        }
    }

    pub struct Presenter {
        hwnd: HWND,
        frame: Option<Frame>,
    }

    impl Presenter {
        pub fn new(_el: &ActiveEventLoop, window: &Rc<Window>) -> Result<Presenter, String> {
            match window.window_handle().map_err(|e| e.to_string())?.as_raw() {
                RawWindowHandle::Win32(h) => Ok(Presenter { hwnd: h.hwnd.get() as HWND, frame: None }),
                _ => Err("not a Win32 window".into()),
            }
        }

        /// Has `render` fill the regions it is given in the `w` x `h` frame,
        /// and puts them on screen. The window keeps what was copied to it,
        /// so the damage is all that's drawn. Returns the regions drawn.
        pub fn draw(&mut self, w: u32, h: u32, damage: Vec<Region>, render: impl FnOnce(&mut [Pixels], &[Region])) -> Vec<Region> {
            if self.frame.as_ref().is_none_or(|f| f.width != w || f.height != h) {
                self.frame = None;
                self.frame = Frame::new(w, h);
            }
            let Some(frame) = &mut self.frame else { return Vec::new() };
            let regions = damage;
            render(frame.pixels(), &regions);
            let info = |rows: u32| BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(rows as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                bmiColors: [RGBQUAD::default()],
            };
            // SAFETY: GDI calls on our live window; every row handed over lies
            // inside the frame buffer.
            unsafe {
                let dc = GetDC(self.hwnd);
                for r in &regions {
                    // The source starts at the region's first row and spans
                    // all its rows, so the DIB's origin convention can't matter.
                    let rows = frame.pixels.add(r.y0 as usize * w as usize);
                    let (x, y, rw, rh) = (r.x0 as i32, r.y0 as i32, r.width() as i32, r.height() as i32);
                    StretchDIBits(dc, x, y, rw, rh, x, 0, rw, rh, rows.cast(), &info(r.height()), DIB_RGB_COLORS, SRCCOPY);
                }
                ReleaseDC(self.hwnd, dc);
            }
            regions
        }

        /// Gives the frame buffer back to the OS.
        pub fn release(&mut self) {
            self.frame = None;
        }

        /// The OS dropped part of the window's content (it's waiting to be
        /// repainted). Asked directly, whoever requested the redraw.
        pub fn invalidated(&self, _unrequested: bool) -> bool {
            // SAFETY: a query on our live window.
            unsafe { GetUpdateRect(self.hwnd, ptr::null_mut(), 0) != 0 }
        }
    }
}

#[cfg(not(windows))]
mod soft {
    use std::num::NonZeroU32;

    use winit::event_loop::OwnedDisplayHandle;

    use super::*;

    pub struct Presenter {
        surface: softbuffer::Surface<OwnedDisplayHandle, Rc<Window>>,
        _context: softbuffer::Context<OwnedDisplayHandle>,
        /// Size of the buffers, and the regions the last frame drew.
        size: (u32, u32),
        last: Vec<Region>,
        /// A buffer was presented: from then on, a blank one (age 0) means
        /// the backend never keeps them (macOS).
        presented: bool,
        /// For such backends, the frame, kept while drawing goes on: the
        /// damage is drawn into it and the whole copied over, instead of
        /// rasterizing every frame whole. Given back when idle (`release`).
        kept: Vec<Pixels>,
    }

    impl Presenter {
        pub fn new(el: &ActiveEventLoop, window: &Rc<Window>) -> Result<Presenter, String> {
            let context = softbuffer::Context::new(el.owned_display_handle()).map_err(|e| e.to_string())?;
            let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
            Ok(Presenter { surface, _context: context, size: (0, 0), last: Vec::new(), presented: false, kept: Vec::new() })
        }

        /// Has `render` fill the regions it is given in the `w` x `h` frame,
        /// and presents them: the damage, widened to what the buffer missed
        /// (see the module notes). Returns the regions drawn.
        pub fn draw(&mut self, w: u32, h: u32, damage: Vec<Region>, render: impl FnOnce(&mut [Pixels], &[Region])) -> Vec<Region> {
            let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else { return Vec::new() };
            if self.size != (w, h) {
                if self.surface.resize(nw, nh).is_err() {
                    return Vec::new();
                }
                self.size = (w, h);
                self.kept = Vec::new();
            }
            let Ok(mut buffer) = self.surface.buffer_mut() else { return Vec::new() };
            let last = std::mem::take(&mut self.last);
            let regions = if buffer.age() == 0 && self.presented {
                let pixels = w as usize * h as usize;
                let fresh = self.kept.len() != pixels;
                if fresh {
                    self.kept = vec![[0; 4]; pixels]; // zeroed pages: committed as drawn
                }
                let regions = for_buffer(if fresh { 0 } else { 1 }, damage, last, w, h);
                render(&mut self.kept, &regions);
                let target: &mut [Pixels] = bytemuck::cast_slice_mut(&mut buffer);
                target.copy_from_slice(&self.kept);
                regions
            } else {
                let regions = for_buffer(buffer.age(), damage, last, w, h);
                render(bytemuck::cast_slice_mut(&mut buffer), &regions);
                regions
            };
            let rects: Vec<softbuffer::Rect> = regions
                .iter()
                .filter_map(|r| {
                    Some(softbuffer::Rect { x: r.x0, y: r.y0, width: NonZeroU32::new(r.width())?, height: NonZeroU32::new(r.height())? })
                })
                .collect();
            if let Err(e) = buffer.present_with_damage(&rects) {
                eprintln!("ytmcross: present failed: {e}");
            }
            self.presented = true;
            self.last = regions.clone();
            regions
        }

        /// Gives the kept frame back (only backends that need one have it).
        pub fn release(&mut self) {
            self.kept = Vec::new();
        }

        /// The OS dropped the window's content: a redraw we didn't ask for
        /// is the system asking for it back (X11 expose, restoring...).
        pub fn invalidated(&self, unrequested: bool) -> bool {
            unrequested
        }
    }
}

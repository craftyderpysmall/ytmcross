//! Getting rendered pixels onto the window: our own frame buffer, copied to
//! the window with GDI. Only the regions that changed are copied (the window
//! keeps showing the rest), the buffer's untouched pages never become
//! resident, and between bursts of drawing it is given back to the OS: idle,
//! no frame buffer exists at all.

use std::ptr;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetUpdateRect, RGBQUAD, ReleaseDC, SRCCOPY,
    StretchDIBits,
};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::damage::Region;

pub type Pixels = [u8; 4];

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
    pub fn new(window: &Window) -> Result<Presenter, String> {
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
    /// repainted).
    pub fn invalidated(&self) -> bool {
        // SAFETY: a query on our live window.
        unsafe { GetUpdateRect(self.hwnd, ptr::null_mut(), 0) != 0 }
    }
}

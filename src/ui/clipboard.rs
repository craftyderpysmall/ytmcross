//! The Wayland clipboard, for text. egui-winit's clipboard support would
//! connect at startup; this connects on the first copy or paste, so its
//! thread only exists once it's needed.
//!
//! egui-winit keeps its own in-app clipboard instead; the shell fills it
//! from here right before a paste, and copies what egui copies back here.

use std::cell::OnceCell;
use std::ffi::c_void;
use std::time::Duration;

use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

pub struct Clipboard {
    display: *mut c_void,
    inner: OnceCell<smithay_clipboard::Clipboard>,
}

impl Clipboard {
    pub fn new(window: &Window) -> Result<Clipboard, String> {
        match window.display_handle().map_err(|e| e.to_string())?.as_raw() {
            RawDisplayHandle::Wayland(h) => Ok(Clipboard { display: h.display.as_ptr(), inner: OnceCell::new() }),
            _ => Err("not a Wayland window".into()),
        }
    }

    pub fn get(&self) -> Option<String> {
        let fresh = self.inner.get().is_none();
        let clipboard = self.connected();
        // A new connection hears about the seat and focus a moment later.
        let tries = if fresh { 10 } else { 1 };
        (0..tries).find_map(|i| {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            clipboard.load().ok()
        })
    }

    pub fn set(&self, text: &str) {
        self.connected().store(text);
    }

    fn connected(&self) -> &smithay_clipboard::Clipboard {
        // SAFETY: the shell keeps the window, and so the display, alive
        // for longer than this struct.
        self.inner.get_or_init(|| unsafe { smithay_clipboard::Clipboard::new(self.display) })
    }
}

//! The system clipboard, for text, connected on first use (a copy or a
//! paste): most sessions never touch it, and on Linux connecting costs a
//! thread and a display connection. egui-winit's clipboard support would
//! connect at startup, and bring in an image codec stack for copying
//! images, which this app never does.
//!
//! Windows: straight through Win32. Wayland: smithay-clipboard. X11 and
//! macOS: arboard (text only).
//!
//! egui-winit keeps its own in-app clipboard instead; the shell fills it
//! from here right before a paste, and copies what egui copies back here.

use winit::window::Window;

#[derive(Default)]
pub struct Clipboard {
    /// Connected on first use, then kept: on Linux the copied text is
    /// served from this process for as long as it's on the clipboard.
    #[cfg(not(windows))]
    os: Option<os::Os>,
}

impl Clipboard {
    #[cfg(windows)]
    pub fn get(&mut self, _window: &Window) -> Option<String> {
        win32::get()
    }

    #[cfg(windows)]
    pub fn set(&mut self, _window: &Window, text: &str) {
        win32::set(text);
    }

    #[cfg(not(windows))]
    pub fn get(&mut self, window: &Window) -> Option<String> {
        self.connect(window)?.get()
    }

    #[cfg(not(windows))]
    pub fn set(&mut self, window: &Window, text: &str) {
        if let Some(os) = self.connect(window) {
            os.set(text);
        }
    }

    #[cfg(not(windows))]
    fn connect(&mut self, window: &Window) -> Option<&mut os::Os> {
        if self.os.is_none() {
            self.os = os::Os::connect(window);
        }
        self.os.as_mut()
    }
}

#[cfg(not(windows))]
mod os {
    use winit::window::Window;

    pub enum Os {
        #[cfg(all(unix, not(target_os = "macos")))]
        Wayland(smithay_clipboard::Clipboard),
        Arboard(arboard::Clipboard),
    }

    impl Os {
        pub fn connect(window: &Window) -> Option<Os> {
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
                if let Ok(handle) = window.display_handle()
                    && let RawDisplayHandle::Wayland(display) = handle.as_raw()
                {
                    // SAFETY: the display is the window's; the clipboard is
                    // dropped with the shell's surface, before the event loop
                    // closes the display.
                    return Some(Os::Wayland(unsafe { smithay_clipboard::Clipboard::new(display.display.as_ptr()) }));
                }
            }
            let _ = window;
            arboard::Clipboard::new().ok().map(Os::Arboard)
        }

        pub fn get(&mut self) -> Option<String> {
            match self {
                #[cfg(all(unix, not(target_os = "macos")))]
                Os::Wayland(c) => c.load().ok(),
                Os::Arboard(c) => c.get_text().ok(),
            }
        }

        pub fn set(&mut self, text: &str) {
            match self {
                #[cfg(all(unix, not(target_os = "macos")))]
                Os::Wayland(c) => c.store(text),
                Os::Arboard(c) => {
                    let _ = c.set_text(text);
                }
            }
        }
    }
}

#[cfg(windows)]
mod win32 {
    use std::ptr;

    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    /// Standard clipboard format: UTF-16 text.
    const CF_UNICODETEXT: u32 = 13;

    pub fn get() -> Option<String> {
        // SAFETY: the clipboard is opened and closed around the read, and the
        // locked block is only read up to its terminating NUL.
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return None;
            }
            let data = GetClipboardData(CF_UNICODETEXT);
            let text = (!data.is_null()).then(|| GlobalLock(data) as *const u16).filter(|p| !p.is_null()).map(|p| {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                GlobalUnlock(data);
                text
            });
            CloseClipboard();
            text
        }
    }

    pub fn set(text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        // SAFETY: the block is sized for `wide`; once SetClipboardData succeeds
        // the system owns it, otherwise it is freed here.
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return;
            }
            EmptyClipboard();
            let block = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            let p = if block.is_null() { ptr::null_mut() } else { GlobalLock(block) as *mut u16 };
            if !p.is_null() {
                ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(block);
                if !SetClipboardData(CF_UNICODETEXT, block).is_null() {
                    CloseClipboard();
                    return;
                }
            }
            if !block.is_null() {
                GlobalFree(block);
            }
            CloseClipboard();
        }
    }
}

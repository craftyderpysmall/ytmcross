//! The system clipboard on Windows, for text, straight through Win32.
//! egui-winit's clipboard support (arboard) would also bring in an image
//! codec stack, for copying images, which this app never does.
//!
//! egui-winit keeps its own in-app clipboard instead; the shell fills it
//! from here right before a paste, and copies what egui copies back here.

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

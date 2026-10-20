//! Windows player controls outside the window, both arriving through its
//! wrapped window procedure:
//! - The taskbar thumbnail toolbar: previous, play/pause and next under the
//!   taskbar preview. The taskbar announces the window's button with a
//!   registered message; buttons can only be added after it (and again
//!   after Explorer restarts). Clicks arrive as WM_COMMAND.
//! - The shortcuts from `[keys]`, registered system-wide (WM_HOTKEY).

use std::cell::{Cell, OnceCell};
use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::Shell::{
    THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateIcon, GWLP_WNDPROC, GetSystemMetrics, HICON, RegisterWindowMessageW, SM_CXSMICON,
    SetWindowLongPtrW, WM_COMMAND, WM_HOTKEY, WNDPROC,
};
use windows_sys::core::GUID;

use crate::config::{Bind, CONTROLS, Control};

const CLSID_TASKBAR_LIST: GUID = GUID::from_u128(0x56fdf344_fd6d_11d0_958a_006097c9a090);
const IID_ITASKBAR_LIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

/// The start of ITaskbarList3's vtable, up to the calls we make.
#[repr(C)]
struct Vtbl {
    _unknown: [usize; 3],
    hr_init: unsafe extern "system" fn(*mut Taskbar) -> i32,
    /// AddTab ... SetTabActive.
    _list: [usize; 11],
    thumb_bar_add_buttons: unsafe extern "system" fn(*mut Taskbar, HWND, u32, *const THUMBBUTTON) -> i32,
    thumb_bar_update_buttons: unsafe extern "system" fn(*mut Taskbar, HWND, u32, *const THUMBBUTTON) -> i32,
}

#[repr(C)]
struct Taskbar {
    vtbl: *const Vtbl,
}

#[derive(Clone, Copy)]
struct State {
    hwnd: HWND,
    prev_proc: WNDPROC,
    /// The "TaskbarButtonCreated" message.
    created: u32,
    /// Created when the taskbar button first appears.
    taskbar: *mut Taskbar,
    /// Previous, play, pause, next.
    icons: [HICON; 4],
    added: bool,
    /// Enabled (something is loaded), playing.
    shown: (bool, bool),
    /// Shortcuts asked for (registered unless another program holds them).
    hotkeys: [Option<Bind>; 3],
}

thread_local! {
    static STATE: Cell<Option<State>> = const { Cell::new(None) };
    static ON_CLICK: OnceCell<Box<dyn Fn(Control)>> = const { OnceCell::new() };
}

/// Wraps the window's procedure. `on_click` runs on this thread, inside it.
pub fn install(hwnd: HWND, on_click: impl Fn(Control) + 'static) {
    let name: Vec<u16> = "TaskbarButtonCreated\0".encode_utf16().collect();
    // SAFETY: our live window, on its own thread; the previous procedure is
    // kept and every message we don't handle goes on to it.
    let (created, prev_proc) = unsafe {
        let created = RegisterWindowMessageW(name.as_ptr());
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wndproc as *const () as isize);
        (created, std::mem::transmute::<isize, WNDPROC>(prev))
    };
    ON_CLICK.with(|c| c.set(Box::new(on_click)).ok());
    STATE.set(Some(State {
        hwnd,
        prev_proc,
        created,
        taskbar: ptr::null_mut(),
        icons: [ptr::null_mut(); 4],
        added: false,
        shown: (false, false),
        hotkeys: [None; 3],
    }));
}

/// Registers the shortcuts system-wide (no-op if unchanged). Returns those
/// that can't be: held by another program, or a key Windows can't register.
pub fn set_hotkeys(keys: [Option<Bind>; 3]) -> Vec<Bind> {
    let Some(mut s) = STATE.get() else { return Vec::new() };
    if s.hotkeys == keys {
        return Vec::new();
    }
    s.hotkeys = keys;
    STATE.set(Some(s));
    let mut failed = Vec::new();
    for (id, bind) in (1..).zip(keys) {
        let mods = |b: Bind| {
            [(b.ctrl, MOD_CONTROL), (b.alt, MOD_ALT), (b.shift, MOD_SHIFT), (b.logo, MOD_WIN)]
                .iter()
                .fold(MOD_NOREPEAT, |m, &(on, flag)| if on { m | flag } else { m })
        };
        // SAFETY: hotkeys of our live window, on its thread.
        unsafe {
            UnregisterHotKey(s.hwnd, id);
            if let Some(b) = bind
                && vk(b.key).is_none_or(|vk| RegisterHotKey(s.hwnd, id, mods(b), vk) == 0)
            {
                failed.push(b);
            }
        }
    }
    failed
}

/// The virtual-key code of a key (those a shortcut can reasonably use).
fn vk(key: egui::Key) -> Option<u32> {
    use egui::Key::*;
    let name = key.name();
    // Letters and digits are their own codes; F1 is 0x70.
    if let [c] = name.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some(u32::from(*c));
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        return Some(0x6f + n);
    }
    Some(match key {
        Backspace => 0x08,
        Tab => 0x09,
        Enter => 0x0d,
        Escape => 0x1b,
        Space => 0x20,
        PageUp => 0x21,
        PageDown => 0x22,
        End => 0x23,
        Home => 0x24,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        Insert => 0x2d,
        Delete => 0x2e,
        _ => return None,
    })
}

/// Shows the playback state on the buttons (no-op if unchanged).
pub fn update(enabled: bool, playing: bool) {
    let Some(mut s) = STATE.get() else { return };
    if s.shown == (enabled, playing) {
        return;
    }
    s.shown = (enabled, playing);
    STATE.set(Some(s));
    if s.added {
        let buttons = buttons(&s);
        // SAFETY: a live ITaskbarList3; the buttons outlive the call.
        unsafe { ((*(*s.taskbar).vtbl).thumb_bar_update_buttons)(s.taskbar, s.hwnd, 3, buttons.as_ptr()) };
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(s) = STATE.get() else { return 0 };
    if msg == s.created {
        add(s);
    } else if msg == WM_COMMAND && (wparam >> 16) as u32 & 0xffff == THBN_CLICKED {
        if let Some(&b) = CONTROLS.get(wparam & 0xffff) {
            ON_CLICK.with(|c| c.get().map(|f| f(b)));
        }
        return 0;
    } else if msg == WM_HOTKEY {
        if let Some(&b) = CONTROLS.get(wparam.wrapping_sub(1)) {
            ON_CLICK.with(|c| c.get().map(|f| f(b)));
        }
        return 0;
    }
    // SAFETY: the window's own procedure, called as Windows would.
    unsafe { CallWindowProcW(s.prev_proc, hwnd, msg, wparam, lparam) }
}

/// The taskbar button exists (again): add our buttons to it.
fn add(mut s: State) {
    // SAFETY: COM calls on this (UI) thread; the interface is kept for good.
    unsafe {
        if s.taskbar.is_null() {
            CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
            let mut p: *mut c_void = ptr::null_mut();
            if CoCreateInstance(&CLSID_TASKBAR_LIST, ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_ITASKBAR_LIST3, &mut p) < 0
                || ((*(*p.cast::<Taskbar>()).vtbl).hr_init)(p.cast()) < 0
            {
                return;
            }
            s.taskbar = p.cast();
            let size = GetSystemMetrics(SM_CXSMICON);
            s.icons = [&PREV[..], &PLAY, &PAUSE, &NEXT].map(|shapes| icon(size, shapes));
        }
        let buttons = buttons(&s);
        s.added = ((*(*s.taskbar).vtbl).thumb_bar_add_buttons)(s.taskbar, s.hwnd, 3, buttons.as_ptr()) >= 0;
    }
    STATE.set(Some(s));
}

fn buttons(s: &State) -> [THUMBBUTTON; 3] {
    let (enabled, playing) = s.shown;
    CONTROLS.map(|b| {
        let (icon, tip) = match b {
            Control::Previous => (s.icons[0], "Previous"),
            Control::PlayPause if playing => (s.icons[2], "Pause"),
            Control::PlayPause => (s.icons[1], "Play"),
            Control::Next => (s.icons[3], "Next"),
        };
        let mut sz_tip = [0u16; 260];
        for (d, c) in sz_tip.iter_mut().zip(tip.encode_utf16()) {
            *d = c;
        }
        THUMBBUTTON {
            dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
            iId: b as u32,
            iBitmap: 0,
            hIcon: icon,
            szTip: sz_tip,
            dwFlags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
        }
    })
}

// The bar's glyphs (widgets::paint_icon) as convex polygons, in units of
// the icon's scale, y down.
const PLAY: [&[(f32, f32)]; 1] = [&[(-0.65, -1.0), (1.15, 0.0), (-0.65, 1.0)]];
const PAUSE: [&[(f32, f32)]; 2] = [
    &[(-0.825, -1.0), (-0.275, -1.0), (-0.275, 1.0), (-0.825, 1.0)],
    &[(0.275, -1.0), (0.825, -1.0), (0.825, 1.0), (0.275, 1.0)],
];
const NEXT: [&[(f32, f32)]; 2] =
    [&[(-0.85, -0.85), (0.55, 0.0), (-0.85, 0.85)], &[(0.6, -0.85), (0.9, -0.85), (0.9, 0.85), (0.6, 0.85)]];
const PREV: [&[(f32, f32)]; 2] =
    [&[(0.85, -0.85), (-0.55, 0.0), (0.85, 0.85)], &[(-0.6, -0.85), (-0.9, -0.85), (-0.9, 0.85), (-0.6, 0.85)]];

/// A white glyph on transparent, 4x4 supersampled.
fn icon(size: i32, shapes: &[&[(f32, f32)]]) -> HICON {
    let n = size.max(1) as usize;
    let scale = n as f32 * 0.35;
    let mid = n as f32 / 2.0;
    let inside = |poly: &[(f32, f32)], (x, y): (f32, f32)| {
        let side = |i: usize| {
            let ((ax, ay), (bx, by)) = (poly[i], poly[(i + 1) % poly.len()]);
            (bx - ax) * (y - ay) - (by - ay) * (x - ax)
        };
        let s: Vec<f32> = (0..poly.len()).map(side).collect();
        s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)
    };
    let mut bgra = vec![0u32; n * n];
    for (i, px) in bgra.iter_mut().enumerate() {
        let (x, y) = ((i % n) as f32, (i / n) as f32);
        let mut hits = 0;
        for sub in 0..16 {
            let p = ((x + (sub % 4) as f32 / 4.0 + 0.125 - mid) / scale, (y + (sub / 4) as f32 / 4.0 + 0.125 - mid) / scale);
            hits += shapes.iter().any(|poly| inside(poly, p)) as u32;
        }
        *px = (hits * 255 / 16) << 24 | 0xff_ffff;
    }
    // A zero AND mask: the alpha channel decides.
    let mask = vec![0u8; n * n];
    // SAFETY: both buffers cover an n x n icon (the mask more than its 1 bpp).
    unsafe { CreateIcon(ptr::null_mut(), n as i32, n as i32, 1, 32, mask.as_ptr(), bgra.as_ptr().cast()) }
}

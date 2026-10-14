pub mod login;
pub mod session;

use std::process::Command;
use std::time::Duration;

use crate::paths;

/// Runs the sign-in window as a child process and waits for it.
/// Returns true if a usable session was saved.
pub fn sign_in_blocking() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let ok = Command::new(exe).arg("--login").status().map(|s| s.success()).unwrap_or(false);
    // The webview's helper processes may hold the profile for a moment after exit.
    let dir = paths::login_webview_dir();
    for _ in 0..20 {
        if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let signed_in = ok && session::Session::load(&paths::cookies_file()).is_signed_in();
    if signed_in {
        let _ = std::fs::remove_file(paths::signed_out_marker());
    }
    signed_in
}

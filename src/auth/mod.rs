mod cookiedb;
pub mod login;
pub mod session;

use std::time::Duration;

use crate::paths;

/// Runs the sign-in helper (`ytmcross --login`) and waits for it. Returns
/// true if a usable session was saved.
pub fn sign_in_blocking() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let ok = crate::proc::helper(exe).arg("--login").status().map(|s| s.success()).unwrap_or(false);
    // Its browser's processes may hold the profile for a moment after exit.
    let dir = paths::login_profile_dir();
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

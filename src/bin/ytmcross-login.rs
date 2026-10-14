//! The sign-in window, as its own small executable. Keeping the webview out
//! of the main app means the app never loads WebView2Loader.dll (or any
//! webview code) at all; this runs once, writes cookies.txt and exits.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[path = "../auth/login.rs"]
mod login;
#[allow(dead_code)] // the app's paths; only a few are needed here
#[path = "../paths.rs"]
mod paths;

fn main() {
    login::run()
}

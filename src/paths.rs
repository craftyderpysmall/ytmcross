//! Where ytmcross keeps its files. Everything follows OS conventions so
//! package managers never need to know about custom locations.
//!
//! config: %APPDATA%\ytmcross | ~/.config/ytmcross | ~/Library/Application Support/ytmcross
//! data:   %LOCALAPPDATA%\ytmcross | ~/.local/share/ytmcross | ~/Library/Application Support/ytmcross

use std::path::PathBuf;

const APP: &str = "ytmcross";

pub fn config_dir() -> PathBuf {
    known("APPDATA", dirs::config_dir).join(APP)
}

pub fn data_dir() -> PathBuf {
    known("LOCALAPPDATA", dirs::data_local_dir).join(APP)
}

/// On Windows the environment names these folders: asking the shell
/// instead (SHGetKnownFolderPath) loads windows.storage.dll and friends.
fn known(var: &str, ask: fn() -> Option<PathBuf>) -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(not(windows))]
    let _ = var;
    ask().unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

pub fn theme_file(name: &str) -> PathBuf {
    themes_dir().join(format!("{name}.toml"))
}

/// Netscape-format cookie jar: the Google session.
pub fn cookies_file() -> PathBuf {
    data_dir().join("cookies.txt")
}

/// Present after the user signed out on purpose: start signed out instead
/// of opening the sign-in window.
pub fn signed_out_marker() -> PathBuf {
    data_dir().join("signed-out")
}

/// The track playing at exit and its position, restored (paused) at startup.
pub fn last_played_file() -> PathBuf {
    data_dir().join("last-played")
}

/// Where downloads go: the folder set in Preferences ("~" is the home
/// folder), or "downloads" next to config.toml.
pub fn downloads_dir(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        return config_dir().join("downloads");
    }
    match configured.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\'])) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest.trim_start_matches(['/', '\\'])),
        None => PathBuf::from(configured),
    }
}

/// Throwaway webview profile used only during `--login`; deleted afterwards.
pub fn login_webview_dir() -> PathBuf {
    data_dir().join("login-webview")
}

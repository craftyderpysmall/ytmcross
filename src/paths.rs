//! Where ytmcross keeps its files, by the XDG base directory spec.
//!
//! config: $XDG_CONFIG_HOME/ytmcross (~/.config)
//! data:   $XDG_DATA_HOME/ytmcross   (~/.local/share)
//! cache:  $XDG_CACHE_HOME/ytmcross  (~/.cache)

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

const APP: &str = "ytmcross";

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// The spec says relative values are invalid and must be ignored.
fn xdg(var: &str, default: &str) -> PathBuf {
    let base = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute());
    base.unwrap_or_else(|| home().join(default)).join(APP)
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

/// Creates the data folder, private: it holds the Google session.
pub fn create_data_dir() -> std::io::Result<()> {
    let dir = data_dir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
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
/// folder), or "downloads" in the data folder.
pub fn downloads_dir(configured: &str) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        return data_dir().join("downloads");
    }
    match configured.strip_prefix('~').filter(|rest| rest.is_empty() || rest.starts_with('/')) {
        Some(rest) => home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(configured),
    }
}

/// Throwaway browser profile used only during `--login`; deleted afterwards.
pub fn login_profile_dir() -> PathBuf {
    cache_dir().join("login-profile")
}

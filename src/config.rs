//! `config.toml`. Every field has a default, so a partial or empty file works.

use serde::Deserialize;

use crate::paths;

pub const DEFAULT_CONFIG: &str = include_str!("../assets/default-config.toml");
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("default", include_str!("../assets/themes/default.toml")),
    ("rose-pine", include_str!("../assets/themes/rose-pine.toml")),
];

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub window: WindowConfig,
    pub layout: LayoutConfig,
    pub player: PlayerConfig,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct WindowConfig {
    pub width: f32,
    pub height: f32,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BarPosition {
    Top,
    Bottom,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct LayoutConfig {
    pub bar_position: BarPosition,
    pub sidebar: bool,
    pub sidebar_items: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct PlayerConfig {
    pub ytdlp: String,
    pub volume: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".into(),
            window: WindowConfig::default(),
            layout: LayoutConfig::default(),
            player: PlayerConfig::default(),
        }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig { width: 1100.0, height: 720.0 }
    }
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            bar_position: BarPosition::Bottom,
            sidebar: true,
            sidebar_items: vec!["search".into(), "library".into(), "queue".into()],
        }
    }
}

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig { ytdlp: "yt-dlp".into(), volume: 80 }
    }
}

impl Config {
    /// Loads config.toml. On a parse error, returns defaults plus the error text.
    pub fn load() -> (Config, Option<String>) {
        match std::fs::read_to_string(paths::config_file()) {
            Ok(text) => match toml::from_str(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Config::default(), Some(format!("config.toml: {e}"))),
            },
            Err(_) => (Config::default(), None),
        }
    }
}

/// First run: write the default config and bundled themes so they can be edited.
pub fn write_defaults() {
    let _ = std::fs::create_dir_all(paths::themes_dir());
    let _ = std::fs::create_dir_all(paths::data_dir());
    let config = paths::config_file();
    if !config.exists() {
        let _ = std::fs::write(config, DEFAULT_CONFIG);
    }
    for (name, text) in BUILTIN_THEMES {
        let path = paths::theme_file(name);
        if !path.exists() {
            let _ = std::fs::write(path, text);
        }
    }
}

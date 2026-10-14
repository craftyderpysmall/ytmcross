//! `config.toml`. Every field has a default, so a partial or empty file works.

use serde::Deserialize;

use crate::paths;

pub const DEFAULT_CONFIG: &str = include_str!("../assets/default-config.toml");
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("default", include_str!("../assets/themes/default.toml")),
    ("scarf-dark", include_str!("../assets/themes/scarf-dark.toml")),
];

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub window: WindowConfig,
    pub layout: LayoutConfig,
    pub player: PlayerConfig,
    pub equalizer: EqualizerConfig,
    pub pitch: PitchConfig,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct PitchConfig {
    /// -12..=12; 0 leaves the audio untouched.
    pub semitones: i32,
}

/// Number of equalizer bands (see `player::eq::BANDS`).
pub const EQ_BANDS: usize = 5;

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct EqualizerConfig {
    pub enabled: bool,
    /// Gain per band in dB, -12..=12.
    pub gains: [f32; EQ_BANDS],
}

impl Default for EqualizerConfig {
    fn default() -> Self {
        EqualizerConfig { enabled: true, gains: [0.0; EQ_BANDS] }
    }
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
    pub volume: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".into(),
            window: WindowConfig::default(),
            layout: LayoutConfig::default(),
            player: PlayerConfig::default(),
            equalizer: EqualizerConfig::default(),
            pitch: PitchConfig::default(),
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
        PlayerConfig { volume: 80 }
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

/// Theme names available in the themes folder (plus built-ins), sorted.
pub fn list_themes() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_THEMES.iter().map(|(n, _)| n.to_string()).collect();
    if let Ok(entries) = std::fs::read_dir(paths::themes_dir()) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "toml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Sets `theme = "<name>"` in config.toml, keeping everything else as written.
pub fn save_theme(name: &str) -> std::io::Result<()> {
    edit_config(|text| set_top_level_key(text, "theme", &format!("{name:?}")))
}

/// Rewrites the `[equalizer]` section of config.toml (the app owns it).
pub fn save_equalizer(eq: &EqualizerConfig) -> std::io::Result<()> {
    let gains: Vec<String> = eq.gains.iter().map(|g| format!("{g:.1}")).collect();
    let body = format!(
        "# Managed by the Equalizer menu. Gains in dB for 60 Hz, 250 Hz, 1 kHz, 4 kHz, 12 kHz.\nenabled = {}\ngains = [{}]\n",
        eq.enabled,
        gains.join(", ")
    );
    edit_config(|text| set_section(text, "equalizer", &body))
}

/// Rewrites the `[pitch]` section of config.toml (the app owns it).
pub fn save_pitch(semitones: i32) -> std::io::Result<()> {
    let body = format!("# Managed by the Pitch menu. Semitones, -12 to 12.\nsemitones = {semitones}\n");
    edit_config(|text| set_section(text, "pitch", &body))
}

fn edit_config(edit: impl FnOnce(&str) -> String) -> std::io::Result<()> {
    let path = paths::config_file();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    std::fs::write(path, edit(&text))
}

/// Replaces (or adds) a top-level `key = value` line, before any [section].
fn set_top_level_key(text: &str, key: &str, value: &str) -> String {
    let mut out = Vec::new();
    let mut done = false;
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if !done {
                out.push(format!("{key} = {value}"));
                done = true;
            }
            in_section = true;
        }
        let is_key = !in_section && trimmed.split('=').next().is_some_and(|k| k.trim() == key) && trimmed.contains('=');
        if is_key && !done {
            out.push(format!("{key} = {value}"));
            done = true;
        } else if !is_key {
            out.push(line.to_string());
        }
    }
    if !done {
        out.push(format!("{key} = {value}"));
    }
    out.join("\n") + "\n"
}

/// Replaces the body of `[section]` (or appends the section).
fn set_section(text: &str, section: &str, body: &str) -> String {
    let header = format!("[{section}]");
    let mut out = Vec::new();
    let mut skipping = false;
    let mut found = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skipping = trimmed == header;
            if skipping {
                found = true;
                out.push(header.clone());
                out.push(body.trim_end().to_string());
                out.push(String::new());
                continue;
            }
        }
        if !skipping {
            out.push(line.to_string());
        }
    }
    if !found {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(header);
        out.push(body.trim_end().to_string());
    }
    out.join("\n") + "\n"
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
        // Missing, or an unedited copy of an older bundled version: (re)write.
        // Edited files are never touched.
        let stale = match std::fs::read_to_string(&path) {
            Ok(old) => OLD_BUNDLED_THEMES.contains(&fnv1a(&old.replace('\r', ""))),
            Err(_) => true,
        };
        if stale {
            let _ = std::fs::write(path, text);
        }
    }
}

/// Hashes of bundled theme files from earlier releases (see `fnv1a`).
const OLD_BUNDLED_THEMES: &[u64] = &[
    0xc02b1319b10c9ac1, // default, lavender accent
    0x2085fc8a4e8be069, // default, YouTube Music colors
];

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# my config\ntheme = \"default\" # mine\n\n[window]\nwidth = 900\n";

    #[test]
    fn replaces_top_level_key_and_keeps_the_rest() {
        let out = set_top_level_key(SAMPLE, "theme", "\"scarf-dark\"");
        assert!(out.contains("theme = \"scarf-dark\""));
        assert!(out.contains("# my config") && out.contains("width = 900"));
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.theme, "scarf-dark");
    }

    #[test]
    fn adds_missing_key_before_sections() {
        let out = set_top_level_key("[window]\nwidth = 900\n", "theme", "\"x\"");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.theme, "x");
        assert_eq!(cfg.window.width, 900.0);
    }

    #[test]
    fn section_is_added_then_replaced() {
        let eq = EqualizerConfig { enabled: false, gains: [3.0, 0.0, -2.5, 0.0, 6.0] };
        let body = "enabled = false\ngains = [3.0, 0.0, -2.5, 0.0, 6.0]\n";
        let once = set_section(SAMPLE, "equalizer", body);
        let twice = set_section(&once, "equalizer", "enabled = true\ngains = [0.0, 0.0, 0.0, 0.0, 0.0]\n");
        assert_eq!(toml::from_str::<Config>(&once).unwrap().equalizer, eq);
        let cfg: Config = toml::from_str(&twice).unwrap();
        assert!(cfg.equalizer.enabled);
        assert_eq!(twice.matches("[equalizer]").count(), 1);
        assert_eq!(cfg.window.width, 900.0);
    }
}

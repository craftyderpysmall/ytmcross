//! Themes: a readable list of design tokens in `themes/<name>.toml`.
//!
//! The user's file is merged key by key over the built-in default theme, so
//! any token left out falls back to the default. The resolved tokens are
//! then mapped onto egui's visuals and style in one place: `apply`.

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Vec2};
use serde::Deserialize;
use std::sync::Arc;

use crate::config::BUILTIN_THEMES;
use crate::paths;

// Inter, subset to every character it has but without the tables egui
// never reads (GSUB/GPOS/GDEF, glyph names): renders pixel-identical, 40%
// smaller. Made with:
//   pyftsubset Inter-X.ttf --unicodes='*' --drop-tables+=GSUB,GPOS,GDEF --layout-features=''
const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

/// Font family for titles and headings.
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

#[derive(Deserialize, Clone, Debug)]
struct Spec {
    colors: ColorSpec,
    font: FontSpec,
    shape: ShapeSpec,
    art: ArtSpec,
}

#[derive(Deserialize, Clone, Debug)]
struct ColorSpec {
    background: String,
    panel: String,
    text: String,
    text_dim: String,
    accent: String,
    hover: String,
    selection: String,
}

#[derive(Deserialize, Clone, Debug)]
struct FontSpec {
    file: String,
    size: f32,
}

#[derive(Deserialize, Clone, Debug)]
struct ShapeSpec {
    rounding: f32,
    spacing: f32,
    padding: f32,
}

#[derive(Deserialize, Clone, Debug)]
struct ArtSpec {
    show: bool,
    size: f32,
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub hover: Color32,
    pub selection: Color32,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub colors: Palette,
    pub font_size: f32,
    pub rounding: f32,
    pub spacing: f32,
    pub padding: f32,
    pub show_art: bool,
    pub art_size: f32,
    pub font_file: String,
}

/// A loaded theme plus anything that went wrong while loading it.
pub struct Loaded {
    pub theme: Theme,
    /// Custom font bytes, if the theme names a font file.
    pub font: Option<Vec<u8>>,
    pub error: Option<String>,
}

impl Theme {
    pub fn builtin() -> Theme {
        resolve(&default_table()).expect("built-in theme is valid")
    }

    /// Reads `themes/<name>.toml`, merged over the default. Does file I/O,
    /// so call it at startup or from a worker thread.
    pub fn load(name: &str) -> Loaded {
        let mut table = default_table();
        let mut error = None;
        let user = std::fs::read_to_string(paths::theme_file(name))
            .ok()
            .or_else(|| BUILTIN_THEMES.iter().find(|(n, _)| *n == name).map(|(_, t)| t.to_string()));
        match user.map(|t| t.parse::<toml::Table>()) {
            Some(Ok(user)) => merge(&mut table, user),
            Some(Err(e)) => error = Some(format!("theme {name}: {e}")),
            None => error = Some(format!("theme {name}: not found in {}", paths::themes_dir().display())),
        }
        let theme = match resolve(&table) {
            Ok(t) => t,
            Err(e) => {
                error = Some(format!("theme {name}: {e}"));
                Theme::builtin()
            }
        };
        let font = (!theme.font_file.is_empty())
            .then(|| match std::fs::read(&theme.font_file) {
                Ok(bytes) => Some(bytes),
                Err(e) => {
                    error = Some(format!("font {}: {e}", theme.font_file));
                    None
                }
            })
            .flatten();
        Loaded { theme, font, error }
    }

    /// Maps the tokens onto egui. Cheap: only called at startup and on reload.
    pub fn apply(&self, ctx: &egui::Context) {
        let c = &self.colors;
        let r = CornerRadius::same(self.rounding.clamp(0.0, 255.0) as u8);
        let mut v = egui::Visuals::dark();
        v.panel_fill = c.background;
        v.window_fill = c.panel;
        v.window_stroke = Stroke::NONE;
        v.window_corner_radius = r;
        v.menu_corner_radius = r;
        v.extreme_bg_color = c.panel;
        v.text_edit_bg_color = Some(c.panel);
        v.faint_bg_color = c.panel;
        v.override_text_color = Some(c.text);
        v.hyperlink_color = c.accent;
        v.selection.bg_fill = c.accent.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0_f32, c.accent);
        v.text_cursor.stroke = Stroke::new(2.0_f32, c.accent);
        for (w, fill) in [
            (&mut v.widgets.noninteractive, c.background),
            (&mut v.widgets.inactive, c.panel),
            (&mut v.widgets.hovered, c.hover),
            (&mut v.widgets.active, c.selection),
            (&mut v.widgets.open, c.selection),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = Stroke::NONE;
            w.fg_stroke = Stroke::new(1.0_f32, c.text);
            w.corner_radius = r;
            w.expansion = 0.0;
        }
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, c.accent.gamma_multiply(0.4));
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, c.accent);
        ctx.set_visuals_of(egui::Theme::Dark, v.clone());
        ctx.set_visuals_of(egui::Theme::Light, v);
        ctx.set_theme(egui::Theme::Dark);

        let size = self.font_size;
        ctx.all_styles_mut(|s| {
            s.text_styles = [
                (TextStyle::Small, FontId::proportional(size * 0.85)),
                (TextStyle::Body, FontId::proportional(size)),
                (TextStyle::Button, FontId::proportional(size)),
                (TextStyle::Monospace, FontId::monospace(size)),
                (TextStyle::Heading, FontId::new(size * 1.7, bold())),
            ]
            .into();
            s.spacing.item_spacing = Vec2::splat(self.spacing);
            s.spacing.button_padding = Vec2::new(self.padding * 0.75, self.padding * 0.4);
            s.spacing.interact_size.y = size * 2.0;
            s.animation_time = 0.12;
        });
    }
}

/// Installs Inter (or the theme's font) as the UI font, plus the system's
/// fallback fonts when `fallback` is set. Rebuilds the font atlas, so only
/// call at startup or when something changed.
pub fn install_fonts(ctx: &egui::Context, custom: Option<Arc<FontData>>, fallback: bool) {
    let mut defs = FontDefinitions::default();
    defs.font_data.insert("ui".into(), custom.unwrap_or_else(|| Arc::new(FontData::from_static(INTER_REGULAR))));
    defs.font_data.insert("ui-bold".into(), Arc::new(FontData::from_static(INTER_SEMIBOLD)));
    let mut tail = Vec::new();
    if fallback {
        for (i, &(bytes, index)) in system_fallbacks().iter().enumerate() {
            let name = format!("fallback-{i}");
            defs.font_data.insert(name.clone(), Arc::new(FontData { index, ..FontData::from_static(bytes) }));
            tail.push(name);
        }
    }
    // egui's bundled fonts are compiled out; Inter covers Latin, Greek and
    // Cyrillic, the fallbacks (only when needed) everything else.
    let family = |first: &[&str]| first.iter().map(|s| s.to_string()).chain(tail.iter().cloned()).collect::<Vec<_>>();
    defs.families.insert(FontFamily::Proportional, family(&["ui"]));
    defs.families.insert(FontFamily::Monospace, family(&["ui"]));
    defs.families.insert(bold(), family(&["ui-bold", "ui"]));
    ctx.set_fonts(defs);
}

/// Whether `text` has characters Inter can't draw (CJK, Hangul, Thai,
/// Arabic, Hebrew, Indic, ...): then the system fallback fonts are needed.
pub fn needs_fallback(text: &str) -> bool {
    text.chars().any(|c| {
        let c = c as u32;
        // Inter: Latin, IPA, Greek, Cyrillic (< U+0530), phonetic and Latin/Greek
        // extended, punctuation and symbols (U+1D00..U+2E7F); variation selectors.
        !(c < 0x0530 || (0x1D00..0x2E80).contains(&c) || (0xFE00..0xFE10).contains(&c) || c == 0xFEFF)
    })
}

/// System fonts covering other scripts, memory-mapped once: the OS pages in
/// only the glyphs drawn, and that memory is file-backed, not ours.
fn system_fallbacks() -> &'static [(&'static [u8], u32)] {
    static FONTS: std::sync::OnceLock<Vec<(&'static [u8], u32)>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        #[cfg(windows)]
        let dir = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
        #[cfg(windows)]
        let files: &[&str] =
            &["msyh.ttc", "malgun.ttf", "Nirmala.ttc", "Nirmala.ttf", "LeelawUI.ttf", "segoeui.ttf", "seguisym.ttf"];
        #[cfg(target_os = "macos")]
        let dir = std::path::PathBuf::from("/System/Library/Fonts");
        #[cfg(target_os = "macos")]
        let files: &[&str] = &["Hiragino Sans GB.ttc", "AppleSDGothicNeo.ttc", "Supplemental/Arial Unicode.ttf"];
        #[cfg(not(any(windows, target_os = "macos")))]
        let dir = std::path::PathBuf::from("/usr/share/fonts");
        #[cfg(not(any(windows, target_os = "macos")))]
        let files: &[&str] = &[
            "opentype/noto/NotoSansCJK-Regular.ttc",
            "noto-cjk/NotoSansCJK-Regular.ttc",
            "google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
            "truetype/dejavu/DejaVuSans.ttf",
            "TTF/DejaVuSans.ttf",
            "dejavu-sans-fonts/DejaVuSans.ttf",
        ];
        files.iter().filter_map(|f| map_file(&dir.join(f))).map(|bytes| (bytes, 0)).collect()
    })
}

/// Maps a file read-only for the rest of the process's life.
#[cfg(windows)]
fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY};
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    // SAFETY: a read-only view of a file we opened; it's never unmapped, so
    // the slice stays valid. Font files aren't modified while in use.
    unsafe {
        let mapping = CreateFileMappingW(file.as_raw_handle(), std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null());
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        CloseHandle(mapping); // the view keeps the mapping alive
        (!view.Value.is_null()).then(|| std::slice::from_raw_parts(view.Value as *const u8, len))
    }
}

#[cfg(unix)]
fn map_file(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    // SAFETY: as on Windows: read-only, private, never unmapped.
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0);
        (p != libc::MAP_FAILED).then(|| std::slice::from_raw_parts(p as *const u8, len))
    }
}

fn default_table() -> toml::Table {
    BUILTIN_THEMES[0].1.parse().expect("built-in theme is valid TOML")
}

/// Recursively overlays `over` onto `base`.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn resolve(table: &toml::Table) -> Result<Theme, String> {
    let spec: Spec = table.clone().try_into().map_err(|e: toml::de::Error| e.to_string())?;
    let c = &spec.colors;
    Ok(Theme {
        colors: Palette {
            background: hex(&c.background)?,
            panel: hex(&c.panel)?,
            text: hex(&c.text)?,
            text_dim: hex(&c.text_dim)?,
            accent: hex(&c.accent)?,
            hover: hex(&c.hover)?,
            selection: hex(&c.selection)?,
        },
        font_size: spec.font.size.clamp(8.0, 40.0),
        rounding: spec.shape.rounding.max(0.0),
        spacing: spec.shape.spacing.max(0.0),
        padding: spec.shape.padding.max(0.0),
        show_art: spec.art.show,
        art_size: spec.art.size.clamp(16.0, 160.0),
        font_file: spec.font.file,
    })
}

/// "#rrggbb" or "#rrggbbaa".
fn hex(s: &str) -> Result<Color32, String> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("zz"), 16);
    let bad = || format!("invalid color {s:?} (expected #rrggbb or #rrggbbaa)");
    match h.len() {
        6 => Ok(Color32::from_rgb(byte(0).map_err(|_| bad())?, byte(2).map_err(|_| bad())?, byte(4).map_err(|_| bad())?)),
        8 => Ok(Color32::from_rgba_unmultiplied(
            byte(0).map_err(|_| bad())?,
            byte(2).map_err(|_| bad())?,
            byte(4).map_err(|_| bad())?,
            byte(6).map_err(|_| bad())?,
        )),
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_theme_falls_back_to_default() {
        let mut table = default_table();
        merge(&mut table, "[colors]\naccent = \"#ff0000\"".parse().unwrap());
        let t = resolve(&table).unwrap();
        assert_eq!(t.colors.accent, Color32::from_rgb(255, 0, 0));
        assert_eq!(t.colors.background, Theme::builtin().colors.background);
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(hex("#12345").is_err());
        assert!(hex("#gggggg").is_err());
        assert_eq!(hex("#ffffff80").unwrap().a(), 0x80);
    }
}

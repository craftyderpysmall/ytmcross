//! Menus: Account / Equalizer / Pitch / Theme / Preferences popups, right-click menus
//! for songs and playlists, and the delete confirmation. All of them are
//! drawn only while open, so they cost nothing otherwise.
//!
//! Like the views, they draw with shared borrows and return what the user
//! chose; the caller applies it.

use egui::containers::menu::SubMenuButton;
use egui::{Align, Button, Frame, Id, Modal, Popup, PopupCloseBehavior, RectAlign, Response, RichText, Slider, TextEdit, Ui};

use super::app::{App, is_special_playlist};
use super::widgets::{self, nav_item, pill, text_button};
use crate::api::models::Playlist;
use crate::config::{EQ_BANDS, EqualizerConfig};
use crate::{paths, proc};
use crate::player::eq::{BANDS, MAX_GAIN_DB};

/// Buttons `menus` draws (the sidebar reserves room for them).
pub const MENU_COUNT: usize = 6;

pub enum MenuAction {
    SignIn,
    SignOut,
    ListThemes,
    SetTheme(String),
    OpenThemes,
    OpenDownloads,
    /// A new download folder (empty: the default).
    DownloadFolder(String),
    /// Program that opens folders (empty: the system default).
    FileManager(String),
    /// New settings, and whether to save them (on release, not every step).
    Equalizer(EqualizerConfig, bool),
    /// Semitones, and whether to save.
    Pitch(i32, bool),
}

const EQ_PRESETS: &[(&str, [f32; EQ_BANDS])] = &[
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Bass", [6.0, 3.0, 0.0, 0.0, 0.0]),
    ("Vocal", [-2.0, 0.0, 3.0, 2.0, 0.0]),
    ("Treble", [0.0, 0.0, 0.0, 3.0, 6.0]),
];

/// The Account, Equalizer, Pitch, Theme, Downloads and Preferences buttons
/// with their popups.
/// `compact` lays them out as small text buttons (layout without a sidebar).
pub fn menus(app: &App, ui: &mut Ui, compact: bool) -> Option<MenuAction> {
    let theme = &app.theme;
    let trigger = |ui: &mut Ui, label: &str| if compact { text_button(ui, theme, label) } else { nav_item(ui, theme, label, false) };
    let popup = |resp: &Response| {
        let align = if compact { RectAlign::BOTTOM_START } else { RectAlign::RIGHT_END };
        Popup::menu(resp)
            .align(align)
            .gap(6.0)
            .frame(Frame::menu(&resp.ctx.global_style()).inner_margin(12))
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
    };
    let mut action = None;

    let account = trigger(ui, "Account");
    popup(&account).show(|ui| {
        ui.set_min_width(200.0);
        let status = if app.signed_out { "Not signed in" } else { "Signed in" };
        ui.label(RichText::new(status).color(theme.colors.text_dim));
        ui.add_space(6.0);
        if app.signing_in {
            ui.label("Waiting for the sign-in window…");
        } else if app.signed_out {
            if pill(ui, theme, "Sign in").clicked() {
                action = Some(MenuAction::SignIn);
                ui.close();
            }
        } else if ui.button("Sign out").clicked() {
            action = Some(MenuAction::SignOut);
            ui.close();
        }
    });

    let eq_button = trigger(ui, "Equalizer");
    popup(&eq_button).show(|ui| action = equalizer(app, ui).or(action.take()));

    let pitch_button = trigger(ui, "Pitch");
    popup(&pitch_button).show(|ui| action = pitch(app, ui).or(action.take()));

    let theme_button = trigger(ui, "Theme");
    if theme_button.clicked() {
        action = Some(MenuAction::ListThemes); // pick up theme files added since
    }
    popup(&theme_button).show(|ui| {
        ui.set_min_width(180.0);
        ui.label(RichText::new("Theme").color(theme.colors.text_dim));
        ui.add_space(4.0);
        for name in &app.themes {
            if ui.selectable_label(*name == app.config.theme, name).clicked() {
                action = Some(MenuAction::SetTheme(name.clone()));
                ui.close();
            }
        }
        ui.add_space(6.0);
        if ui.button("Open themes folder").clicked() {
            action = Some(MenuAction::OpenThemes);
            ui.close();
        }
    });

    if trigger(ui, "Downloads").on_hover_text("Open the downloads folder").clicked() {
        action = Some(MenuAction::OpenDownloads);
    }

    let prefs = trigger(ui, "Preferences");
    if prefs.clicked() {
        ui.data_mut(|d| d.remove::<String>(Id::new(FOLDER_FIELD))); // start from the saved value
        if CHOOSE_FILE_MANAGER {
            let found = proc::installed_file_managers();
            ui.data_mut(|d| d.insert_temp(Id::new(FILE_MANAGERS), found));
        }
    }
    popup(&prefs).show(|ui| action = preferences(app, ui).or(action.take()));
    action
}

/// Text being typed in Preferences, kept in egui's memory while it is open.
const FOLDER_FIELD: &str = "download-folder";
/// Installed file managers, looked up when Preferences opens.
const FILE_MANAGERS: &str = "file-managers";
/// Where folders open with xdg-open, which may pick a terminal one.
const CHOOSE_FILE_MANAGER: bool = cfg!(not(any(windows, target_os = "macos")));

fn preferences(app: &App, ui: &mut Ui) -> Option<MenuAction> {
    let theme = &app.theme;
    ui.set_width(340.0);
    ui.label(RichText::new("Download folder").color(theme.colors.text_dim));
    ui.add_space(4.0);
    let field = Id::new(FOLDER_FIELD);
    let mut folder = ui.data(|d| d.get_temp::<String>(field)).unwrap_or_else(|| app.config.downloads.folder.clone());
    let default = paths::downloads_dir("").display().to_string();
    let edit = ui.add(TextEdit::singleline(&mut folder).hint_text(default).desired_width(f32::INFINITY));
    let mut chosen = (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))).then(|| folder.clone());
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if pill(ui, theme, "Save").clicked() {
            chosen = Some(folder.clone());
        }
        if ui.button("Default").on_hover_text("Downloads next to config.toml").clicked() {
            chosen = Some(String::new());
        }
    });
    ui.data_mut(|d| d.insert_temp(field, folder));
    ui.add_space(6.0);
    let small = |text: String| RichText::new(text).font(widgets::small(theme)).color(theme.colors.text_dim);
    ui.label(small("Downloads go to:".into()));
    ui.add(egui::Label::new(small(app.downloads.dir().display().to_string())).truncate());
    let mut file_manager = None;
    if CHOOSE_FILE_MANAGER {
        ui.add_space(10.0);
        ui.label(RichText::new("File manager").color(theme.colors.text_dim));
        ui.add_space(4.0);
        let current = app.config.downloads.file_manager.trim();
        let mut found: Vec<&str> = ui.data(|d| d.get_temp::<Vec<&'static str>>(Id::new(FILE_MANAGERS))).unwrap_or_default();
        if !current.is_empty() && !found.contains(&current) {
            found.push(current); // set in config.toml
        }
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(current.is_empty(), "System default").clicked() {
                file_manager = Some(String::new());
            }
            for name in found {
                if ui.selectable_label(name == current, name).clicked() {
                    file_manager = Some(name.to_string());
                }
            }
        });
        ui.add_space(4.0);
        ui.label(small("Opens the downloads and themes folders.".into()));
    }
    if chosen.is_some() {
        ui.close();
    }
    chosen.map(MenuAction::DownloadFolder).or(file_manager.map(MenuAction::FileManager))
}

/// Widgets default to the panel colour, which is also the popup's: lift
/// them so checkboxes and slider rails show.
fn lift(app: &App, ui: &mut Ui) {
    let v = ui.visuals_mut();
    v.widgets.inactive.bg_fill = app.theme.colors.hover;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, app.theme.colors.text_dim.gamma_multiply(0.5));
    v.slider_trailing_fill = true;
}

fn equalizer(app: &App, ui: &mut Ui) -> Option<MenuAction> {
    lift(app, ui);
    ui.spacing_mut().slider_width = 120.0;
    let mut eq = app.eq.clone();
    let mut changed = false;
    let mut save = false;
    ui.horizontal(|ui| {
        let r = ui.checkbox(&mut eq.enabled, "Equalizer");
        changed |= r.changed();
        save |= r.changed();
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for (gain, freq) in eq.gains.iter_mut().zip(BANDS) {
            ui.vertical(|ui| {
                ui.set_width(40.0);
                ui.label(RichText::new(format!("{gain:+.0}")).font(widgets::small(&app.theme)));
                let r = ui.add_enabled(
                    eq.enabled,
                    Slider::new(gain, -MAX_GAIN_DB..=MAX_GAIN_DB).vertical().step_by(0.5).show_value(false),
                );
                changed |= r.changed();
                save |= r.drag_stopped() || (r.changed() && !r.dragged());
                let label = if freq >= 1000.0 { format!("{}k", freq / 1000.0) } else { format!("{freq}") };
                ui.label(RichText::new(label).font(widgets::small(&app.theme)).color(app.theme.colors.text_dim));
            });
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        for (name, gains) in EQ_PRESETS {
            let active = eq.gains == *gains;
            if ui.add_enabled(eq.enabled, egui::Button::selectable(active, *name)).clicked() {
                eq.gains = *gains;
                changed = true;
                save = true;
            }
        }
    });
    (changed || save).then_some(MenuAction::Equalizer(eq, save))
}

fn pitch(app: &App, ui: &mut Ui) -> Option<MenuAction> {
    lift(app, ui);
    ui.set_width(260.0);
    ui.spacing_mut().slider_width = 220.0;
    let theme = &app.theme;
    let mut semitones = app.pitch;
    let mut save = false;
    ui.horizontal(|ui| {
        ui.label("Pitch");
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let text = match semitones {
                0 => "Original".to_string(),
                1 | -1 => format!("{semitones:+} semitone"),
                s => format!("{s:+} semitones"),
            };
            ui.label(RichText::new(text).color(theme.colors.text_dim));
        });
    });
    ui.add_space(6.0);
    let r = ui.add(Slider::new(&mut semitones, -12..=12).show_value(false));
    save |= r.drag_stopped() || (r.changed() && !r.dragged());
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        for (label, step) in [("−1", -1), ("+1", 1)] {
            if ui.button(label).clicked() {
                semitones = (semitones + step).clamp(-12, 12);
                save = true;
            }
        }
        if ui.add_enabled(semitones != 0, egui::Button::new("Reset")).clicked() {
            semitones = 0;
            save = true;
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Changes pitch, not speed.").font(widgets::small(theme)).color(theme.colors.text_dim));
    (semitones != app.pitch || save).then_some(MenuAction::Pitch(semitones, save))
}

/// Where a song, playlist or album stands with downloads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Download {
    No,
    Running,
    Done,
}

pub enum TrackChoice {
    PlayNext,
    AddToQueue,
    /// Index into the library list.
    AddToPlaylist(usize),
    Download,
}

/// Right-click menu on a song row. `playlists` is None when signed out.
pub fn track_menu(resp: &Response, playlists: Option<&[Playlist]>, download: Download) -> Option<TrackChoice> {
    let mut choice = None;
    resp.context_menu(|ui| {
        ui.set_min_width(170.0);
        if ui.button("Play next").clicked() {
            choice = Some(TrackChoice::PlayNext);
        }
        if ui.button("Add to queue").clicked() {
            choice = Some(TrackChoice::AddToQueue);
        }
        // egui's default arrow glyph isn't in Inter (its own fonts are compiled out).
        let button = egui::Button::new("Add to playlist").right_text("\u{203a}");
        SubMenuButton::from_button(button).ui(ui, |ui| {
            ui.set_min_width(170.0);
            match playlists {
                None => {
                    ui.label("Sign in to use playlists");
                }
                Some(list) => {
                    // Only the user's own playlists can be added to.
                    let mut own = list.iter().enumerate().filter(|(_, p)| p.owned && !is_special_playlist(&p.id)).peekable();
                    if own.peek().is_none() {
                        ui.label("No playlists yet");
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (i, p) in own {
                            if ui.button(&p.title).clicked() {
                                choice = Some(TrackChoice::AddToPlaylist(i));
                            }
                        }
                    });
                }
            }
        });
        match download {
            Download::No => {
                if ui.button("Download").clicked() {
                    choice = Some(TrackChoice::Download);
                }
            }
            Download::Running => {
                ui.add_enabled(false, Button::new("Downloading\u{2026}"));
            }
            Download::Done => {
                ui.add_enabled(false, Button::new("Downloaded"));
            }
        }
    });
    choice
}

pub enum PlaylistChoice {
    Play,
    Download,
    Delete,
    Remove,
}

/// Right-click menu on a playlist or album: download it (or remove the
/// download), own playlists can be deleted, saved ones removed from the library.
pub fn playlist_menu(resp: &Response, playlist: &Playlist, signed_out: bool, download: Download) -> Option<PlaylistChoice> {
    let mut choice = None;
    resp.context_menu(|ui| {
        ui.set_min_width(170.0);
        if ui.button("Play").clicked() {
            choice = Some(PlaylistChoice::Play);
        }
        let label = match download {
            Download::No => "Download",
            Download::Running => "Downloading\u{2026}",
            Download::Done => "Remove download",
        };
        if ui.add_enabled(download != Download::Running, Button::new(label)).clicked() {
            choice = Some(PlaylistChoice::Download);
        }
        if signed_out || is_special_playlist(&playlist.id) {
            return;
        }
        if playlist.owned {
            if ui.button("Delete playlist…").clicked() {
                choice = Some(PlaylistChoice::Delete);
            }
        } else if playlist.save_id.is_some() && ui.button("Remove from library").clicked() {
            choice = Some(PlaylistChoice::Remove);
        }
    });
    choice
}

/// "Delete this playlist?" dialog. `Some(true)`: confirmed, `Some(false)`: cancelled.
pub fn delete_dialog(app: &App, ctx: &egui::Context) -> Option<bool> {
    let playlist = app.confirm_delete.as_ref()?;
    let theme = &app.theme;
    let mut answer = None;
    let modal = Modal::new(Id::new("delete-playlist")).show(ctx, |ui| {
        ui.set_width(320.0);
        ui.label(RichText::new(format!("Delete \u{201c}{}\u{201d}?", playlist.title)).font(widgets::title_font(theme)));
        ui.add_space(4.0);
        ui.label(RichText::new("This removes it from your YouTube Music account.").color(theme.colors.text_dim));
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if pill(ui, theme, "Delete").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}

pub fn apply(app: &mut App, action: MenuAction) {
    match action {
        MenuAction::SignIn => app.sign_in(),
        MenuAction::SignOut => app.sign_out(),
        MenuAction::ListThemes => app.list_themes(),
        MenuAction::SetTheme(name) => app.set_theme(&name),
        MenuAction::OpenThemes => app.open_folder(&paths::themes_dir()),
        MenuAction::OpenDownloads => {
            let dir = app.downloads.dir();
            app.open_folder(&dir);
        }
        MenuAction::DownloadFolder(folder) => app.set_download_folder(folder),
        MenuAction::FileManager(program) => app.set_file_manager(program),
        MenuAction::Equalizer(eq, save) => app.set_equalizer(eq, save),
        MenuAction::Pitch(semitones, save) => app.set_pitch(semitones, save),
    }
}

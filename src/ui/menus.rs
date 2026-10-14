//! Menus: Account / Equalizer / Theme popups, right-click menus for songs
//! and playlists, and the delete confirmation. All of them are drawn only
//! while open, so they cost nothing otherwise.
//!
//! Like the views, they draw with shared borrows and return what the user
//! chose; the caller applies it.

use egui::containers::menu::SubMenuButton;
use egui::{Align, Frame, Id, Modal, Popup, PopupCloseBehavior, RectAlign, Response, RichText, Slider, Ui};

use super::app::{App, is_special_playlist};
use super::widgets::{self, nav_item, pill, text_button};
use crate::api::models::Playlist;
use crate::config::{EQ_BANDS, EqualizerConfig};
use crate::player::eq::{BANDS, MAX_GAIN_DB};

pub enum MenuAction {
    SignIn,
    SignOut,
    ListThemes,
    SetTheme(String),
    /// New settings, and whether to save them (on release, not every step).
    Equalizer(EqualizerConfig, bool),
}

const EQ_PRESETS: &[(&str, [f32; EQ_BANDS])] = &[
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Bass", [6.0, 3.0, 0.0, 0.0, 0.0]),
    ("Vocal", [-2.0, 0.0, 3.0, 2.0, 0.0]),
    ("Treble", [0.0, 0.0, 0.0, 3.0, 6.0]),
];

/// The Account, Equalizer and Theme buttons with their popups. `compact`
/// lays them out as small text buttons (for the layout without a sidebar).
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
        ui.add_space(4.0);
        ui.label(RichText::new("Theme files live in the themes folder next to config.toml.").font(widgets::small(theme)).color(theme.colors.text_dim));
    });
    action
}

fn equalizer(app: &App, ui: &mut Ui) -> Option<MenuAction> {
    // Widgets default to the panel colour, which is also the popup's: lift
    // them so the checkbox and slider rails show.
    let v = ui.visuals_mut();
    v.widgets.inactive.bg_fill = app.theme.colors.hover;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, app.theme.colors.text_dim.gamma_multiply(0.5));
    v.slider_trailing_fill = true;
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

pub enum TrackChoice {
    PlayNext,
    AddToQueue,
    /// Index into the library list.
    AddToPlaylist(usize),
}

/// Right-click menu on a song row.
pub fn track_menu(resp: &Response, playlists: Option<&[Playlist]>) -> Option<TrackChoice> {
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
                    let mut any = false;
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (i, p) in list.iter().enumerate().filter(|(_, p)| !is_special_playlist(&p.id)) {
                            any = true;
                            if ui.button(&p.title).clicked() {
                                choice = Some(TrackChoice::AddToPlaylist(i));
                            }
                        }
                    });
                    if !any {
                        ui.label("No playlists yet");
                    }
                }
            }
        });
    });
    choice
}

pub enum PlaylistChoice {
    Play,
    Delete,
}

/// Right-click menu on a playlist row.
pub fn playlist_menu(resp: &Response, playlist: &Playlist) -> Option<PlaylistChoice> {
    let mut choice = None;
    resp.context_menu(|ui| {
        ui.set_min_width(170.0);
        if ui.button("Play").clicked() {
            choice = Some(PlaylistChoice::Play);
        }
        if !is_special_playlist(&playlist.id) && ui.button("Delete playlist…").clicked() {
            choice = Some(PlaylistChoice::Delete);
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
        MenuAction::Equalizer(eq, save) => app.set_equalizer(eq, save),
    }
}

// No console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod auth;
mod config;
mod paths;
mod player;
mod proc;
mod stream;
mod theme;
mod trace;
mod ui;
mod worker;

use auth::session::Session;
use config::Config;
use theme::Theme;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--login") {
        auth::login::run();
    }
    if let [_, flag, version] = args.as_slice()
        && flag == "--build-solver"
    {
        std::process::exit(stream::build_solver(version));
    }

    config::write_defaults();
    if !Session::load(&paths::cookies_file()).is_signed_in() && !auth::sign_in_blocking() {
        eprintln!("ytmcross: sign-in was cancelled");
        return Ok(());
    }

    let (config, config_error) = Config::load();
    let theme = Theme::load(&config.theme);
    let viewport = egui::ViewportBuilder::default()
        .with_title("ytmcross")
        .with_app_id("ytmcross")
        .with_inner_size([config.window.width, config.window.height])
        .with_min_inner_size([520.0, 360.0]);
    ui::run(viewport, move |ctx| ui::App::new(ctx, config, theme, config_error))
}

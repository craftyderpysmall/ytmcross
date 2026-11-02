mod api;
mod auth;
mod config;
mod download;
mod mpris;
mod paths;
mod player;
mod pool;
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
    proc::tune_heap();
    trace!("main: start");
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, version] = args.as_slice()
        && flag == "--build-solver"
    {
        std::process::exit(stream::build_solver(version));
    }
    if args.get(1).is_some_and(|flag| flag == "--login") {
        auth::login::run();
    }

    config::write_defaults();
    // First run: offer sign-in right away. After signing out on purpose (or
    // cancelling), the app opens signed out; the Account menu signs in.
    let mut signed_in = Session::load(&paths::cookies_file()).is_signed_in();
    if !signed_in && !paths::signed_out_marker().exists() {
        signed_in = auth::sign_in_blocking();
    }

    let (config, config_error) = Config::load();
    let theme = Theme::load(&config.theme);
    let viewport = egui::ViewportBuilder::default()
        .with_title("YouTube Music")
        .with_app_id("ytmcross")
        .with_inner_size([config.window.width, config.window.height])
        .with_min_inner_size([520.0, 360.0]);
    ui::run(viewport, move |ctx, wake| ui::App::new(ctx, wake, config, theme, config_error, signed_in))
}

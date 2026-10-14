//! One-shot sign-in window (the `ytmcross-login` executable).
//!
//! Runs in its own process so the webview's event loop never meets egui's,
//! and so every byte the browser engine allocated is returned to the OS the
//! moment sign-in finishes. Flow: show Google's sign-in page, wait until the
//! browser lands on music.youtube.com with a SAPISID cookie, dump the
//! cookies to `cookies.txt`, exit.

use std::fs;
use std::io::Write;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebContext, WebViewBuilder};

use crate::paths;

const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&passive=true&continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26next%3Dhttps%253A%252F%252Fmusic.youtube.com%252F";

/// Google refuses sign-in from browsers it can't identify. WebView2 already
/// reports a genuine Edge UA; WebKit-based webviews need a Safari one.
#[cfg(not(windows))]
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

enum UserEvent {
    PageLoaded(String),
}

/// Exit code 0 on success, 1 if the window was closed before signing in.
pub fn run() -> ! {
    let profile = paths::login_webview_dir();
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::create_dir_all(paths::data_dir());

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("ytmcross - sign in")
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 700.0))
        .build(&event_loop)
        .expect("failed to create sign-in window");

    let proxy = event_loop.create_proxy();
    let mut context = WebContext::new(Some(profile));
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(LOGIN_URL)
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = proxy.send_event(UserEvent::PageLoaded(url));
            }
        });
    #[cfg(not(windows))]
    let builder = builder.with_user_agent(USER_AGENT);

    #[cfg(any(windows, target_os = "macos"))]
    let webview = builder.build(&window).expect("failed to create webview");
    #[cfg(not(any(windows, target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().unwrap()).expect("failed to create webview")
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::PageLoaded(url)) if url.starts_with("https://music.youtube.com") => {
                let cookies = webview.cookies_for_url("https://music.youtube.com").unwrap_or_default();
                if cookies.iter().any(|c| c.name() == "SAPISID") {
                    match write_cookies(&cookies) {
                        Ok(()) => std::process::exit(0),
                        Err(e) => {
                            eprintln!("ytmcross: could not save cookies: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => std::process::exit(1),
            _ => {}
        }
    })
}

/// Netscape cookie-jar format, read back by `session.rs`.
fn write_cookies(cookies: &[wry::cookie::Cookie<'static>]) -> std::io::Result<()> {
    let path = paths::cookies_file();
    let tmp = path.with_extension("tmp");
    let mut out = String::from("# Netscape HTTP Cookie File\n# Written by ytmcross. This is your Google session: keep it private.\n\n");
    // Session cookies get a far-future expiry so they're kept like the rest.
    let far_future = 4_102_444_800i64; // 2100-01-01
    for c in cookies {
        let domain = match c.domain() {
            Some(d) if d.starts_with('.') => d.to_string(),
            Some(d) => format!(".{d}"),
            None => ".youtube.com".to_string(),
        };
        let prefix = if c.http_only().unwrap_or(false) { "#HttpOnly_" } else { "" };
        let expires = c.expires_datetime().map(|t| t.unix_timestamp()).unwrap_or(far_future);
        out.push_str(&format!(
            "{prefix}{domain}\tTRUE\t{}\t{}\t{expires}\t{}\t{}\n",
            c.path().unwrap_or("/"),
            if c.secure().unwrap_or(false) { "TRUE" } else { "FALSE" },
            c.name(),
            c.value(),
        ));
    }
    let mut file = fs::File::create(&tmp)?;
    file.write_all(out.as_bytes())?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, &path)
}

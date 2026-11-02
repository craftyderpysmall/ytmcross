//! One-shot sign-in (`ytmcross --login`, a process of its own).
//!
//! Google refuses sign-in in webviews and in browsers with a debugger
//! attached, so this runs the user's own Chromium-family browser, untouched,
//! on a throwaway profile. Flow: show Google's sign-in page, wait until the
//! profile's cookie database holds a SAPISID for YouTube, close the browser,
//! decrypt the YouTube cookies to `cookies.txt`, delete the profile, exit.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};

use super::cookiedb::{self, Cookie};
use crate::{paths, proc};

const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?ltmpl=music&service=youtube&passive=true&continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26next%3Dhttps%253A%252F%252Fmusic.youtube.com%252F";

/// Tried in order, after `$YTMCROSS_BROWSER`.
const BROWSERS: [&str; 6] = ["google-chrome-stable", "google-chrome", "chromium", "brave", "vivaldi", "microsoft-edge-stable"];

/// What Chromium encrypts cookies with under `--password-store=basic`:
/// PBKDF2-SHA1 of "peanuts", salt "saltysalt", one iteration. Its own fixed
/// key, not a secret: that flag keeps this profile off the desktop keyring.
const KEY: [u8; 16] = [0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27, 0x27, 0x78];

const POLL: Duration = Duration::from_secs(1);
/// How long a browser gets to show up, and to shut down.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Polls without the lock file before the browser counts as closed: it may
/// briefly restart itself on a new profile.
const GONE_POLLS: u32 = 3;

/// Exit code 0 on success, 1 if the window was closed before signing in,
/// 2 if no browser could be found.
pub fn run() -> ! {
    let profile = paths::login_profile_dir();
    let _ = fs::remove_dir_all(&profile);
    let _ = paths::create_data_dir();
    if fs::DirBuilder::new().recursive(true).mode(0o700).create(&profile).is_err() {
        std::process::exit(1);
    }
    let Some(browser) = launch(&profile) else {
        eprintln!("ytmcross: sign-in needs Chrome, Chromium, Brave, Vivaldi or Edge (or set YTMCROSS_BROWSER)");
        std::process::exit(2);
    };
    let cookies = wait_for_session(&profile, &browser);
    let _ = fs::remove_dir_all(&profile);
    let saved = cookies.is_some_and(|c| match write_cookies(&c) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("ytmcross: could not save cookies: {e}");
            false
        }
    });
    std::process::exit(if saved { 0 } else { 1 })
}

/// Starts the first browser that exists. In a process group of its own, so
/// that all of it can be closed, even after it replaces the process we ran.
fn launch(profile: &Path) -> Option<Child> {
    let custom = std::env::var("YTMCROSS_BROWSER").ok().filter(|b| !b.is_empty());
    custom.iter().map(String::as_str).chain(BROWSERS).find_map(|browser| {
        proc::helper(browser)
            .arg(format!("--app={LOGIN_URL}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(["--password-store=basic", "--no-first-run", "--no-default-browser-check", "--window-size=480,720"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()
    })
}

/// Waits for a session or for the window to close, leaving the browser shut.
fn wait_for_session(profile: &Path, browser: &Child) -> Option<Vec<Cookie>> {
    let lock = profile.join("SingletonLock");
    let running = || fs::symlink_metadata(&lock).is_ok();
    let start = Instant::now();
    let (mut seen, mut gone) = (false, 0);
    let mut latest = None;
    loop {
        std::thread::sleep(POLL);
        if let Some(cookies) = read_session(profile) {
            latest = Some(cookies);
            break;
        }
        gone = if running() { 0 } else { gone + 1 };
        seen |= gone == 0;
        if gone >= GONE_POLLS && (seen || start.elapsed() > START_TIMEOUT) {
            break;
        }
    }
    // A clean shutdown writes out the cookies still in memory.
    // SAFETY: a plain signal to the process group we created.
    unsafe { libc::killpg(browser.id() as libc::pid_t, libc::SIGTERM) };
    let closing = Instant::now();
    while running() && closing.elapsed() < CLOSE_TIMEOUT {
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: as above; anything still alive holds the session in memory.
    unsafe { libc::killpg(browser.id() as libc::pid_t, libc::SIGKILL) };
    read_session(profile).or(latest)
}

/// The YouTube cookies, once they make up a session.
fn read_session(profile: &Path) -> Option<Vec<Cookie>> {
    let file = cookie_file(profile)?;
    // Being written to: a torn read fails or lacks the cookie; try again later.
    let cookies = cookiedb::read(&fs::read(file).ok()?)?;
    let cookies: Vec<Cookie> = cookies.into_iter().filter(|c| is_youtube(&c.host)).collect();
    cookies.iter().any(|c| c.name == "SAPISID" && decrypt(&c.encrypted).is_some()).then_some(cookies)
}

fn cookie_file(profile: &Path) -> Option<PathBuf> {
    let default = profile.join("Default");
    [default.join("Network/Cookies"), default.join("Cookies")].into_iter().find(|p| p.exists())
}

/// Only what the app uses: the profile also holds the whole Google account's.
fn is_youtube(host: &str) -> bool {
    matches!(host.trim_start_matches('.'), "youtube.com" | "music.youtube.com" | "www.youtube.com")
}

/// The plaintext of a `v10` value: AES-128-CBC, IV of 16 spaces, PKCS#7.
fn decrypt(encrypted: &[u8]) -> Option<Vec<u8>> {
    let data = encrypted.strip_prefix(b"v10")?;
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }
    let cipher = Aes128::new(GenericArray::from_slice(&KEY));
    let mut previous = [b' '; 16];
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<16>().0 {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        out.extend(block.iter().zip(previous).map(|(b, p)| b ^ p));
        previous = *chunk;
    }
    let padding = usize::from(*out.last()?);
    if !(1..=16).contains(&padding) {
        return None;
    }
    out.truncate(out.len() - padding);
    Some(out)
}

/// A cookie's value. From database version 24 a hash of the host comes first.
fn value(cookie: &Cookie) -> Option<String> {
    if cookie.encrypted.is_empty() {
        return Some(cookie.value.clone());
    }
    let plain = decrypt(&cookie.encrypted)?;
    let skip = if cookie.db_version >= 24 { 32 } else { 0 };
    String::from_utf8(plain.get(skip..)?.to_vec()).ok()
}

/// Netscape cookie-jar format, read back by `session.rs`.
fn write_cookies(cookies: &[Cookie]) -> std::io::Result<()> {
    let path = paths::cookies_file();
    let tmp = path.with_extension("tmp");
    let mut out = String::from("# Netscape HTTP Cookie File\n# Written by ytmcross. This is your Google session: keep it private.\n\n");
    for c in cookies {
        let Some(value) = value(c) else { continue };
        let domain = if c.host.starts_with('.') { c.host.clone() } else { format!(".{}", c.host) };
        let prefix = if c.http_only { "#HttpOnly_" } else { "" };
        let secure = if c.secure { "TRUE" } else { "FALSE" };
        out.push_str(&format!("{prefix}{domain}\tTRUE\t{}\t{secure}\t{}\t{}\t{value}\n", c.path, c.expires_unix(), c.name));
    }
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    file.write_all(out.as_bytes())?;
    drop(file);
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(encrypted: Vec<u8>, db_version: u32) -> Cookie {
        Cookie { host: ".youtube.com".into(), name: "A".into(), value: String::new(), path: "/".into(), encrypted, expires_utc: 0, secure: true, http_only: false, db_version }
    }

    #[test]
    fn decrypts_chromium_values() {
        // openssl enc -aes-128-cbc -K fd621f...7872 -iv 2020...20, of "hello cookie"
        let mut blob = b"v10".to_vec();
        blob.extend([
            0x2a, 0xc8, 0x0d, 0x58, 0x60, 0x6f, 0xdf, 0x92, 0x24, 0x69, 0x40, 0xc1, 0x7b, 0xad, 0xd0, 0x61,
        ]);
        assert_eq!(decrypt(&blob).unwrap(), b"hello cookie");
        assert_eq!(value(&cookie(blob, 23)).unwrap(), "hello cookie");
        assert!(decrypt(b"v11abc").is_none() && decrypt(b"v10").is_none() && decrypt(b"v10short").is_none());
    }

    #[test]
    fn keeps_only_youtube_cookies() {
        assert!(is_youtube(".youtube.com") && is_youtube("music.youtube.com"));
        assert!(!is_youtube(".google.com") && !is_youtube("accounts.google.com") && !is_youtube("notyoutube.com"));
    }
}

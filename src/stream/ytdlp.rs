//! Fallback resolver: yt-dlp. Slow (~4 s per lookup, mostly process and
//! JS-runtime startup) but maintained against every YouTube change, so it
//! covers whatever the native path can't (a brand-new player the bundled
//! solver doesn't understand yet, accounts without Premium, ...).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;

use crate::{paths, proc};

/// Runs one yt-dlp process for `ids`, calling `found` as each URL is printed.
pub fn resolve(ytdlp: &str, jar: &Path, ids: &[String], mut found: impl FnMut(&str, String)) -> Result<(), String> {
    // yt-dlp rewrites its cookie file on exit; give it a private copy so
    // concurrent runs can never corrupt the real session.
    std::fs::copy(paths::cookies_file(), jar).map_err(|e| format!("cookies: {e}"))?;
    let mut cmd = proc::command(ytdlp);
    cmd.args([
        "--no-warnings",
        "--no-playlist",
        "--ignore-errors",
        "--no-progress",
        // AAC in MP4 (itag 141 at 256 kbps with Premium, else 140): what the
        // built-in decoder plays.
        "-f",
        "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]",
        "--extractor-args",
        "youtube:player_client=web_music;player_skip=webpage,configs",
        "--print",
        "%(id)s %(url)s",
        "--cookies",
    ])
    .arg(jar)
    .args(ids.iter().map(|id| format!("https://music.youtube.com/watch?v={id}")))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| format!("couldn't run {ytdlp} ({e}). Is yt-dlp installed?"))?;
    proc::bind_to_parent(&child);
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some((id, url)) = line.trim().split_once(' ')
                && url.starts_with("http")
            {
                found(id, url.to_string());
            }
        }
    }
    let _ = child.wait();
    let _ = std::fs::remove_file(jar);
    Ok(())
}

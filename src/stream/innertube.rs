//! Asking YouTube Music for a track's streams (the `player` endpoint) and
//! turning the chosen one into a playable URL.

use serde_json::{Value, json};

use super::solver::Solver;
use crate::auth::session::{ORIGIN, Session};

/// Audio formats the built-in decoder plays, best first:
/// AAC 256 kbps (YouTube Premium), then AAC 128 kbps.
const PREFERRED_ITAGS: &[i64] = &[141, 140];

/// A player response's chosen format, still scrambled.
pub struct Scrambled {
    pub itag: i64,
    /// Either a ready URL with an `n` to unscramble, or a signatureCipher.
    url: Option<String>,
    cipher: Option<String>,
}

pub fn player(agent: &ureq::Agent, session: &Session, video_id: &str, sts: u32) -> Result<Scrambled, String> {
    let version = crate::api::client::client_version();
    let body = json!({
        "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": version, "hl": "en", "timeZone": "UTC", "utcOffsetMinutes": 0}},
        "videoId": video_id,
        "playbackContext": {"contentPlaybackContext": {"html5Preference": "HTML5_PREF_WANTS", "signatureTimestamp": sts}},
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let mut req = agent
        .post(format!("{ORIGIN}/youtubei/v1/player?prettyPrint=false"))
        .header("X-YouTube-Client-Name", "67")
        .header("X-YouTube-Client-Version", version)
        .header("Origin", ORIGIN)
        .header("X-Origin", ORIGIN)
        .header("Cookie", session.cookie_header());
    if let Some(auth) = session.auth_header() {
        req = req.header("Authorization", auth);
    }
    let resp: Value = req
        .send_json(&body)
        .and_then(|mut r| r.body_mut().with_config().limit(16 << 20).read_json())
        .map_err(|e| format!("player request: {e}"))?;

    let status = resp["playabilityStatus"]["status"].as_str().unwrap_or("?");
    if status != "OK" {
        let reason = resp["playabilityStatus"]["reason"].as_str().unwrap_or("");
        return Err(format!("not playable ({status}) {reason}"));
    }
    let formats = resp["streamingData"]["adaptiveFormats"].as_array().ok_or("no streams")?;
    let chosen = PREFERRED_ITAGS
        .iter()
        .find_map(|want| formats.iter().find(|f| f["itag"].as_i64() == Some(*want)))
        .ok_or("no AAC stream")?;
    Ok(Scrambled {
        itag: chosen["itag"].as_i64().unwrap_or(0),
        url: chosen["url"].as_str().map(str::to_string),
        cipher: chosen["signatureCipher"].as_str().map(str::to_string),
    })
}

/// Unscrambles the signature (if any) and `n` parameter.
pub fn unlock(s: &Scrambled, solver: &Solver) -> Result<String, String> {
    let (base, sig) = match (&s.url, &s.cipher) {
        (Some(url), _) => (url.clone(), None),
        (None, Some(cipher)) => {
            let scrambled = pct_decode(param(cipher, "s").ok_or("cipher without s")?);
            let key = param(cipher, "sp").unwrap_or("signature").to_string();
            let url = pct_decode(param(cipher, "url").ok_or("cipher without url")?);
            (url, Some((key, solver.sig(&scrambled)?)))
        }
        (None, None) => return Err("stream has no URL".into()),
    };
    let (head, query) = base.split_once('?').ok_or("malformed stream URL")?;
    let mut parts: Vec<String> = Vec::new();
    for kv in query.split('&') {
        match kv.strip_prefix("n=") {
            Some(n) => parts.push(format!("n={}", pct_encode(&solver.n(&pct_decode(n))?))),
            None => parts.push(kv.to_string()),
        }
    }
    if let Some((key, sig)) = sig {
        parts.push(format!("{key}={}", pct_encode(&sig)));
    }
    Ok(format!("{head}?{}", parts.join("&")))
}

fn param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(b[i + 1]) << 4 | hex(b[i + 2]));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trip() {
        let raw = "AE0s2JYw/+=&x";
        assert_eq!(pct_decode(&pct_encode(raw)), raw);
        assert_eq!(pct_decode("a%3Db%26c"), "a=b&c");
        assert_eq!(pct_decode("100%"), "100%");
    }

    #[test]
    fn reads_params() {
        let c = "s=abc%3D&sp=sig&url=https%3A%2F%2Fx%2Fv%3Fn%3D1";
        assert_eq!(param(c, "sp"), Some("sig"));
        assert_eq!(pct_decode(param(c, "url").unwrap()), "https://x/v?n=1");
        assert_eq!(param(c, "missing"), None);
    }
}

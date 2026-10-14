//! The signed-in session: cookies from `cookies.txt` plus the SAPISID
//! hashes YouTube expects on authenticated requests.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

pub const ORIGIN: &str = "https://music.youtube.com";

#[derive(Clone, Default)]
pub struct Session {
    cookie_header: String,
    /// (scheme, cookie value) for SAPISIDHASH, SAPISID1PHASH, SAPISID3PHASH.
    sids: Vec<(&'static str, String)>,
}

impl Session {
    pub fn load(path: &Path) -> Session {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        Session::parse(&text)
    }

    fn parse(text: &str) -> Session {
        let mut pairs = Vec::new();
        let (mut sapisid, mut p1, mut p3) = (None, None, None);
        for line in text.lines() {
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 7 {
                continue;
            }
            let domain = fields[0].trim_start_matches('.');
            if domain != "youtube.com" && domain != "music.youtube.com" {
                continue;
            }
            let (name, value) = (fields[5], fields[6]);
            match name {
                "SAPISID" => sapisid = Some(value.to_string()),
                "__Secure-1PAPISID" => p1 = Some(value.to_string()),
                "__Secure-3PAPISID" => p3 = Some(value.to_string()),
                _ => {}
            }
            pairs.push(format!("{name}={value}"));
        }
        // YouTube falls back to __Secure-3PAPISID when SAPISID is missing.
        let main = sapisid.or_else(|| p3.clone());
        let sids = [("SAPISIDHASH", main), ("SAPISID1PHASH", p1), ("SAPISID3PHASH", p3)]
            .into_iter()
            .filter_map(|(scheme, v)| v.map(|v| (scheme, v)))
            .collect();
        Session { cookie_header: pairs.join("; "), sids }
    }

    pub fn is_signed_in(&self) -> bool {
        self.sids.iter().any(|(s, _)| *s == "SAPISIDHASH")
    }

    pub fn cookie_header(&self) -> &str {
        &self.cookie_header
    }

    /// `SAPISIDHASH <ts>_<sha1("<ts> <SID> <origin>")> SAPISID1PHASH ... SAPISID3PHASH ...`
    pub fn auth_header(&self) -> Option<String> {
        if self.sids.is_empty() {
            return None;
        }
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let parts: Vec<String> = self
            .sids
            .iter()
            .map(|(scheme, sid)| {
                let digest = Sha1::digest(format!("{ts} {sid} {ORIGIN}").as_bytes());
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                format!("{scheme} {ts}_{hex}")
            })
            .collect();
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netscape_jar() {
        let jar = "# comment\n#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tabc\n.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\txyz\n.youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-3PAPISID\tq\n.google.com\tTRUE\t/\tTRUE\t0\tNID\tnope\n";
        let s = Session::parse(jar);
        assert!(s.is_signed_in());
        assert_eq!(s.cookie_header(), "SID=abc; SAPISID=xyz; __Secure-3PAPISID=q");
        let auth = s.auth_header().unwrap();
        assert!(auth.starts_with("SAPISIDHASH ") && auth.contains(" SAPISID3PHASH "));
    }
}

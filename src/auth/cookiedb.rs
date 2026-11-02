//! Reads the `cookies` table of a Chromium cookie database: a walk of the
//! SQLite file format, which is all sign-in needs of a SQLite library (about
//! 1 MB of binary). Any surprise reads as `None`: the file may be mid-write.

/// A row of the table, with what `login.rs` needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub path: String,
    pub encrypted: Vec<u8>,
    /// Microseconds since 1601; 0 for a session cookie.
    pub expires_utc: i64,
    pub secure: bool,
    pub http_only: bool,
    /// The database's schema version.
    pub db_version: u32,
}

impl Cookie {
    /// Unix seconds. Session cookies get a far-future expiry, to be kept
    /// like the rest.
    pub fn expires_unix(&self) -> i64 {
        const FAR_FUTURE: i64 = 4_102_444_800; // 2100-01-01
        const WINDOWS_EPOCH: i64 = 11_644_473_600;
        if self.expires_utc == 0 { FAR_FUTURE } else { self.expires_utc / 1_000_000 - WINDOWS_EPOCH }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Null,
    Int(i64),
    Bytes(Vec<u8>),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        }
    }

    fn int(&self) -> i64 {
        if let Value::Int(i) = self { *i } else { 0 }
    }
}

/// B-tree depth limit: a torn file could otherwise point in circles.
const MAX_DEPTH: u32 = 16;

struct Db<'a> {
    file: &'a [u8],
    page_size: usize,
    usable: usize,
}

pub fn read(file: &[u8]) -> Option<Vec<Cookie>> {
    if !file.starts_with(b"SQLite format 3\0") {
        return None;
    }
    let page_size = match u16::from_be_bytes([*file.get(16)?, *file.get(17)?]) {
        1 => 65536,
        n => usize::from(n),
    };
    let db = Db { file, page_size, usable: page_size.checked_sub(usize::from(*file.get(20)?))? };

    // The schema table: type, name, tbl_name, rootpage, sql.
    let mut schema = Vec::new();
    db.walk(1, 0, &mut schema)?;
    let table = |name: &str| schema.iter().find(|r| r.get(1).is_some_and(|n| n.text() == name) && r.first().is_some_and(|t| t.text() == "table"));
    let cookies = table("cookies")?;
    let columns = columns(&cookies.get(4)?.text());
    let mut rows = Vec::new();
    db.walk(usize::try_from(cookies.get(3)?.int()).ok()?, 0, &mut rows)?;

    let mut meta = Vec::new();
    db.walk(usize::try_from(table("meta")?.get(3)?.int()).ok()?, 0, &mut meta)?;
    let version = meta.iter().find(|r| r.first().is_some_and(|k| k.text() == "version"))?;
    let db_version = version.get(1)?.text().parse().ok()?;

    let index = |name: &str| columns.iter().position(|c| c == name);
    let (host, name, value) = (index("host_key")?, index("name")?, index("value")?);
    let (path, encrypted, expires) = (index("path")?, index("encrypted_value")?, index("expires_utc")?);
    let (secure, http_only) = (index("is_secure")?, index("is_httponly")?);
    rows.iter()
        .map(|r| {
            let encrypted = match r.get(encrypted)? {
                Value::Bytes(b) => b.clone(),
                _ => Vec::new(),
            };
            Some(Cookie {
                host: r.get(host)?.text(),
                name: r.get(name)?.text(),
                value: r.get(value)?.text(),
                path: r.get(path)?.text(),
                encrypted,
                expires_utc: r.get(expires)?.int(),
                secure: r.get(secure)?.int() != 0,
                http_only: r.get(http_only)?.int() != 0,
                db_version,
            })
        })
        .collect()
}

/// Column names, from the `CREATE TABLE` the schema keeps.
fn columns(sql: &str) -> Vec<String> {
    let body = sql.split_once('(').map_or("", |(_, rest)| rest);
    let mut names = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in body.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                names.push(&body[start..i]);
                break;
            }
            ')' => depth -= 1,
            ',' if depth == 0 => {
                names.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    names.iter().filter_map(|d| d.split_whitespace().next()).map(|n| n.trim_matches(['"', '`', '[', ']']).to_string()).collect()
}

impl Db<'_> {
    fn page(&self, number: usize) -> Option<&[u8]> {
        let start = number.checked_sub(1)?.checked_mul(self.page_size)?;
        self.file.get(start..start.checked_add(self.page_size)?)
    }

    /// Collects the rows of the table b-tree rooted at page `number`.
    fn walk(&self, number: usize, depth: u32, rows: &mut Vec<Vec<Value>>) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let page = self.page(number)?;
        // Page 1 starts with the file header.
        let start = if number == 1 { 100 } else { 0 };
        let be16 = |at: usize| Some(usize::from(u16::from_be_bytes([*page.get(at)?, *page.get(at + 1)?])));
        let cells = be16(start + 3)?;
        let interior = match page.get(start)? {
            0x05 => true,
            0x0d => false,
            _ => return None,
        };
        let pointers = start + if interior { 12 } else { 8 };
        for i in 0..cells {
            let cell = be16(pointers + i * 2)?;
            if interior {
                self.walk(be32(page, cell)?, depth + 1, rows)?;
            } else {
                rows.push(self.record(page, cell)?);
            }
        }
        if interior {
            self.walk(be32(page, start + 8)?, depth + 1, rows)?;
        }
        Some(())
    }

    /// Decodes a table leaf cell.
    fn record(&self, page: &[u8], cell: usize) -> Option<Vec<Value>> {
        let (size, n) = varint(page.get(cell..)?)?;
        let (_rowid, m) = varint(page.get(cell + n..)?)?;
        let payload = self.payload(page, cell + n + m, usize::try_from(size).ok()?)?;
        let (header_size, n) = varint(&payload)?;
        let header_size = usize::try_from(header_size).ok()?;
        let mut types = payload.get(n..header_size)?;
        let mut body = payload.get(header_size..)?;
        let mut values = Vec::new();
        while !types.is_empty() {
            let (serial, n) = varint(types)?;
            types = types.get(n..)?;
            let len = match serial {
                0 | 8 | 9 => 0,
                1..=4 => serial as usize,
                5 => 6,
                6 | 7 => 8,
                n if n >= 12 => ((n - 12) / 2) as usize,
                _ => return None,
            };
            let bytes = body.get(..len)?;
            body = body.get(len..)?;
            values.push(match serial {
                0 => Value::Null,
                8 => Value::Int(0),
                9 => Value::Int(1),
                1..=6 => Value::Int(bytes.iter().fold(if bytes[0] & 0x80 != 0 { -1 } else { 0 }, |v, &b| (v << 8) | i64::from(b))),
                7 => Value::Null, // no float columns are read
                _ => Value::Bytes(bytes.to_vec()),
            });
        }
        Some(values)
    }

    /// A cell's payload, following its overflow pages.
    fn payload(&self, page: &[u8], at: usize, size: usize) -> Option<Vec<u8>> {
        let usable = self.usable;
        let (max_local, min_local) = (usable.checked_sub(35)?, ((usable.checked_sub(12)?) * 32 / 255).checked_sub(23)?);
        if size <= max_local {
            return Some(page.get(at..at + size)?.to_vec());
        }
        let overflow = min_local + (size - min_local) % (usable - 4);
        let local = if overflow <= max_local { overflow } else { min_local };
        let mut out = page.get(at..at + local)?.to_vec();
        let mut next = be32(page, at + local)?;
        while out.len() < size {
            let page = self.page(next)?;
            let take = (size - out.len()).min(usable - 4);
            out.extend_from_slice(page.get(4..4 + take)?);
            next = be32(page, 0)?;
        }
        Some(out)
    }
}

fn be32(bytes: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// SQLite's big-endian varint: the value and its length.
fn varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..9 {
        let b = *bytes.get(i)?;
        if i == 8 {
            return Some(((value << 8) | u64::from(b), 9));
        }
        value = (value << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made with sqlite3 (dev/cookiedb-fixture.py): fake cookies in Chromium's
    /// schema, including one spilling onto overflow pages and enough rows
    /// for an interior page.
    const FIXTURE: &[u8] = include_bytes!("cookiedb-fixture.sqlite");

    #[test]
    fn reads_rows_interior_pages_and_overflow() {
        let cookies = read(FIXTURE).unwrap();
        assert_eq!(cookies.len(), 301);
        assert!(cookies.iter().all(|c| c.db_version == 24));
        let sapisid = cookies.iter().find(|c| c.name == "SAPISID").unwrap();
        assert_eq!((sapisid.host.as_str(), sapisid.path.as_str()), (".youtube.com", "/"));
        assert_eq!(sapisid.encrypted, b"v10fake");
        assert_eq!(sapisid.expires_utc, 13_500_000_000_000_000);
        assert!(sapisid.secure && !sapisid.http_only);
        let long = cookies.iter().find(|c| c.name == "LONG").unwrap();
        assert_eq!(long.encrypted.len(), 10_000);
        assert!(long.encrypted.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
        assert!(long.http_only && !long.secure);
    }

    #[test]
    fn expiry_is_unix_seconds() {
        let mut c = read(FIXTURE).unwrap().remove(0);
        c.expires_utc = 13_500_000_000_000_000;
        assert_eq!(c.expires_unix(), 1_855_526_400);
        c.expires_utc = 0;
        assert_eq!(c.expires_unix(), 4_102_444_800);
    }

    #[test]
    fn damaged_files_read_as_none() {
        assert_eq!(read(b"not a database"), None);
        assert_eq!(read(&FIXTURE[..4096]), None);
        assert_eq!(read(&[]), None);
    }

    #[test]
    fn column_names_ignore_types_and_constraints() {
        let sql = "CREATE TABLE cookies(creation_utc INTEGER NOT NULL,host_key TEXT NOT NULL,x DECIMAL(10,2),UNIQUE (a,b))";
        assert_eq!(columns(sql), ["creation_utc", "host_key", "x", "UNIQUE"]);
    }

    #[test]
    fn varints() {
        assert_eq!(varint(&[0x05]), Some((5, 1)));
        assert_eq!(varint(&[0x81, 0x00]), Some((128, 2)));
        assert_eq!(varint(&[0xff; 9]), Some((u64::MAX, 9)));
        assert_eq!(varint(&[0x80]), None);
    }
}

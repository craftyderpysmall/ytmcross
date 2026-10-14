//! Where each fragment of a fragmented MP4 starts, read from its segment
//! index (`sidx`). YouTube's audio streams are fragmented in ~10 s pieces;
//! knowing their offsets lets a seek jump straight to the right one instead
//! of fetching every fragment header on the way (one request, not twenty).

pub struct FragmentIndex {
    /// The file's bytes before its first fragment: ftyp, moov, sidx.
    pub head: Vec<u8>,
    /// Byte offset and start time (seconds) of each fragment.
    pub fragments: Vec<(u64, f64)>,
}

impl FragmentIndex {
    /// Parses the index from the start of the file (which must reach past the
    /// first fragment's header). None if the file has no usable index.
    pub fn parse(data: &[u8]) -> Option<FragmentIndex> {
        let mut pos = 0;
        while let Some((kind, size, body)) = next_box(data, pos) {
            if kind == *b"sidx" {
                return Self::from_sidx(data, pos + size, body);
            }
            if kind == *b"moof" || kind == *b"mdat" {
                return None;
            }
            pos += size;
        }
        None
    }

    fn from_sidx(data: &[u8], anchor: usize, sidx: &[u8]) -> Option<FragmentIndex> {
        let version = *sidx.first()?;
        let timescale = f64::from(be32(sidx, 8)?);
        let (first_offset, rest) = match version {
            0 => (u64::from(be32(sidx, 16)?), 20),
            1 => (be64(sidx, 20)?, 28),
            _ => return None,
        };
        let count = usize::from(u16::from_be_bytes(sidx.get(rest + 2..rest + 4)?.try_into().ok()?));
        let first = anchor as u64 + first_offset;
        let (mut offset, mut time) = (first, 0u64);
        let mut fragments = Vec::with_capacity(count);
        for i in 0..count {
            let entry = rest + 4 + i * 12;
            let reference = be32(sidx, entry)?;
            if reference & 0x8000_0000 != 0 || timescale == 0.0 {
                return None; // nested indexes aren't used by YouTube
            }
            fragments.push((offset, time as f64 / timescale));
            offset += u64::from(reference & 0x7fff_ffff);
            time += u64::from(be32(sidx, entry + 4)?);
        }
        let first = usize::try_from(first).ok()?;
        let head = data.get(..first)?.to_vec();
        (!fragments.is_empty() && relative_offsets(data, first)?).then_some(FragmentIndex { head, fragments })
    }

    /// The fragment containing `seconds`: its offset and start time.
    pub fn fragment_at(&self, seconds: f64) -> Option<(u64, f64)> {
        self.fragments.iter().rev().find(|f| f.1 <= seconds).copied()
    }
}

/// Whether the first fragment addresses its data relative to itself (no
/// absolute `base_data_offset` in any tfhd), so it reads the same wherever
/// it sits in the stream.
fn relative_offsets(data: &[u8], moof_at: usize) -> Option<bool> {
    let (kind, _, moof) = next_box(data, moof_at)?;
    if kind != *b"moof" {
        return Some(false);
    }
    let mut pos = 0;
    while let Some((kind, size, body)) = next_box(moof, pos) {
        if kind == *b"traf" {
            let mut p = 0;
            while let Some((kind, size, tfhd)) = next_box(body, p) {
                if kind == *b"tfhd" && be32(tfhd, 0)? & 1 != 0 {
                    return Some(false);
                }
                p += size;
            }
        }
        pos += size;
    }
    Some(true)
}

/// The box at `pos`: its type, total size and body.
fn next_box(data: &[u8], pos: usize) -> Option<([u8; 4], usize, &[u8])> {
    let size = be32(data, pos)? as usize;
    let kind: [u8; 4] = data.get(pos + 4..pos + 8)?.try_into().ok()?;
    let (size, header) = match size {
        1 => (usize::try_from(be64(data, pos + 8)?).ok()?, 16),
        0 => (data.len() - pos, 8),
        s => (s, 8),
    };
    if size < header {
        return None;
    }
    let body = data.get(pos + header..(pos + size).min(data.len()))?;
    Some((kind, size, body))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    fn file(tfhd_flags: u32) -> Vec<u8> {
        let mut sidx = vec![0, 0, 0, 0]; // version 0, flags
        sidx.extend(1u32.to_be_bytes()); // reference id
        sidx.extend(1000u32.to_be_bytes()); // timescale
        sidx.extend(0u32.to_be_bytes()); // earliest pts
        sidx.extend(0u32.to_be_bytes()); // first offset
        sidx.extend([0, 0, 0, 3]); // reserved, 3 references
        for (size, dur) in [(100u32, 10_000u32), (200, 10_000), (300, 5_000)] {
            sidx.extend(size.to_be_bytes());
            sidx.extend(dur.to_be_bytes());
            sidx.extend(0u32.to_be_bytes());
        }
        let tfhd = boxed(b"tfhd", &[tfhd_flags.to_be_bytes().as_slice(), &1u32.to_be_bytes()].concat());
        let moof = boxed(b"moof", &boxed(b"traf", &tfhd));
        [boxed(b"ftyp", b"dash"), boxed(b"moov", &[0; 16]), boxed(b"sidx", &sidx), moof].concat()
    }

    #[test]
    fn reads_fragment_offsets_and_times() {
        let data = file(0x2_0000);
        let ix = FragmentIndex::parse(&data).unwrap();
        let first = 12 + 24 + (8 + 24 + 36) as u64;
        assert_eq!(ix.head.len() as u64, first);
        assert_eq!(ix.fragments, vec![(first, 0.0), (first + 100, 10.0), (first + 300, 20.0)]);
        assert_eq!(ix.fragment_at(15.0), Some((first + 100, 10.0)));
        assert_eq!(ix.fragment_at(0.0), Some((first, 0.0)));
    }

    #[test]
    fn refuses_absolute_data_offsets() {
        assert!(FragmentIndex::parse(&file(0x1)).is_none());
    }
}

//! Binary property lists, as much of them as Finder's tags need: an array of
//! strings, read and written.
//!
//! Finder keeps a file's tags in the `com.apple.metadata:_kMDItemUserTags`
//! attribute as a `bplist00` — Apple's binary plist — holding one array of
//! strings. A plist library would read every type the format has; this reads
//! and writes the one shape that attribute takes, and refuses anything else
//! rather than guessing at it.
//!
//! The format, as far as it goes here: the eight bytes `bplist00`; the
//! objects, each a marker byte whose high nibble is its type and low nibble
//! its length (15 meaning "the length follows, as an integer object"); a table
//! of each object's offset; and a 32-byte trailer saying how wide an offset and
//! an object reference are, how many objects there are, which one is the top,
//! and where the offset table starts. Integers are big-endian. A string is
//! ASCII (`0x5_`) or UTF-16BE (`0x6_`, its length in code units); an array
//! (`0xA_`) is a list of object references.

/// The magic and version every binary plist starts with.
const MAGIC: &[u8] = b"bplist00";

/// The trailer's size.
const TRAILER: usize = 32;

/// How many strings an array may hold before it is taken for nonsense: far
/// past any file's tags, short of an allocation a corrupt length could ask for.
const MAX_ITEMS: usize = 4096;

/// A binary plist of one array holding `items`, each as ASCII where it is and
/// UTF-16 where it is not, as Apple's own writer does.
pub fn encode_strings(items: &[String]) -> Vec<u8> {
    let objects = items.len() + 1;
    let ref_size = width(objects as u64);
    let mut out = MAGIC.to_vec();
    let mut offsets = Vec::with_capacity(objects);

    offsets.push(out.len());
    marker(&mut out, 0xA0, items.len());
    for index in 1..objects {
        push_be(&mut out, index as u64, ref_size);
    }
    for item in items {
        offsets.push(out.len());
        if item.is_ascii() {
            marker(&mut out, 0x50, item.len());
            out.extend_from_slice(item.as_bytes());
        } else {
            let units: Vec<u16> = item.encode_utf16().collect();
            marker(&mut out, 0x60, units.len());
            for unit in units {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
    }

    let table = out.len();
    let offset_size = width(table as u64);
    for offset in offsets {
        push_be(&mut out, offset as u64, offset_size);
    }
    out.extend_from_slice(&[0u8; 6]);
    out.push(offset_size as u8);
    out.push(ref_size as u8);
    out.extend_from_slice(&(objects as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&(table as u64).to_be_bytes());
    out
}

/// The strings of a binary plist whose top object is an array of strings;
/// `None` for anything else, or anything malformed.
pub fn decode_strings(data: &[u8]) -> Option<Vec<String>> {
    if data.len() < MAGIC.len() + TRAILER || !data.starts_with(MAGIC) {
        return None;
    }
    let trailer = &data[data.len() - TRAILER..];
    let offset_size = usize::from(trailer[6]);
    let ref_size = usize::from(trailer[7]);
    let objects = usize::try_from(be(&trailer[8..16])?).ok()?;
    let top = usize::try_from(be(&trailer[16..24])?).ok()?;
    let table = usize::try_from(be(&trailer[24..32])?).ok()?;
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) || top >= objects {
        return None;
    }
    let body = &data[..data.len() - TRAILER];
    let table_end = objects.checked_mul(offset_size)?.checked_add(table)?;
    if table_end > body.len() {
        return None;
    }
    let offset_of = |index: usize| -> Option<usize> {
        if index >= objects {
            return None;
        }
        let at = table + index * offset_size;
        usize::try_from(be(&body[at..at + offset_size])?).ok()
    };

    let at = offset_of(top)?;
    let (kind, count, mut cursor) = object(body, at)?;
    if kind != 0xA || count > MAX_ITEMS {
        return None;
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let reference = usize::try_from(be(body.get(cursor..cursor + ref_size)?)?).ok()?;
        cursor += ref_size;
        items.push(string(body, offset_of(reference)?)?);
    }
    Some(items)
}

/// The string object at `at`.
fn string(body: &[u8], at: usize) -> Option<String> {
    let (kind, len, start) = object(body, at)?;
    match kind {
        0x5 => {
            let bytes = body.get(start..start.checked_add(len)?)?;
            bytes
                .is_ascii()
                .then(|| String::from_utf8_lossy(bytes).into_owned())
        }
        0x6 => {
            let bytes = body.get(start..start.checked_add(len.checked_mul(2)?)?)?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

/// The object at `at`: its type nibble, its length, and where its content
/// starts (past a length integer when the marker holds none).
fn object(body: &[u8], at: usize) -> Option<(u8, usize, usize)> {
    let marker = *body.get(at)?;
    let (kind, short) = (marker >> 4, usize::from(marker & 0x0F));
    if short < 0x0F {
        return Some((kind, short, at + 1));
    }
    let int = *body.get(at + 1)?;
    if int >> 4 != 0x1 {
        return None;
    }
    let size = 1usize << (int & 0x0F);
    if size > 8 {
        return None;
    }
    let len = usize::try_from(be(body.get(at + 2..at + 2 + size)?)?).ok()?;
    Some((kind, len, at + 2 + size))
}

/// A big-endian unsigned integer of one to eight bytes.
fn be(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte)),
    )
}

/// How many bytes (1, 2, 4 or 8) hold `n`.
fn width(n: u64) -> usize {
    if n <= 0xFF {
        1
    } else if n <= 0xFFFF {
        2
    } else if n <= 0xFFFF_FFFF {
        4
    } else {
        8
    }
}

fn push_be(out: &mut Vec<u8>, n: u64, size: usize) {
    out.extend_from_slice(&n.to_be_bytes()[8 - size..]);
}

/// A marker byte for an object of type `kind` and length `len`, with the
/// length as an integer object after it when it does not fit in the nibble.
fn marker(out: &mut Vec<u8>, kind: u8, len: usize) {
    if len < 0x0F {
        out.push(kind | len as u8);
        return;
    }
    out.push(kind | 0x0F);
    let size = width(len as u64);
    out.push(0x10 | size.trailing_zeros() as u8);
    push_be(out, len as u64, size);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A file tagged Red and "Work", laid out byte by byte from the format's
    /// description rather than by the writer, so the reader is held to the
    /// format and not only to its own writer's habits.
    #[test]
    fn a_plist_laid_out_by_hand_is_read() {
        let by_hand: &[u8] = &[
            0x62, 0x70, 0x6C, 0x69, 0x73, 0x74, 0x30, 0x30, // bplist00
            0xA2, 0x01, 0x02, // array of 2: objects 1 and 2
            0x55, 0x52, 0x65, 0x64, 0x0A, 0x36, // "Red\n6"
            0x54, 0x57, 0x6F, 0x72, 0x6B, // "Work"
            0x08, 0x0B, 0x11, // offsets 8, 11, 17
            0, 0, 0, 0, 0, 0, 1, 1, // trailer: widths 1 and 1
            0, 0, 0, 0, 0, 0, 0, 3, // three objects
            0, 0, 0, 0, 0, 0, 0, 0, // the top is object 0
            0, 0, 0, 0, 0, 0, 0, 0x16, // the table is at 22
        ];
        assert_eq!(decode_strings(by_hand), Some(strings(&["Red\n6", "Work"])));
        assert_eq!(encode_strings(&strings(&["Red\n6", "Work"])), by_hand);
    }

    /// Anything the writer writes, the reader reads back: ASCII, UTF-16 past
    /// the BMP, lengths that need an integer after the marker, none at all,
    /// and more objects than a one-byte reference holds.
    #[test]
    fn what_is_written_reads_back() {
        let long = "x".repeat(300);
        for items in [
            strings(&[]),
            strings(&["red"]),
            strings(&["ünïcödé — 日本語 🎬\n3", "plain"]),
            strings(&[&long, "fifteen chars!!", "fourteen chars"]),
            (0..300).map(|i| format!("tag {i}")).collect(),
        ] {
            assert_eq!(decode_strings(&encode_strings(&items)), Some(items));
        }
    }

    /// Nonsense is `None`, never a panic: no magic, a truncated file, a top
    /// object that is not an array, a reference out of range.
    #[test]
    fn a_plist_that_is_not_an_array_of_strings_is_refused() {
        assert_eq!(decode_strings(b""), None);
        assert_eq!(decode_strings(b"bplist00"), None);
        let good = encode_strings(&strings(&["a", "b"]));
        assert_eq!(decode_strings(&good[..good.len() - 1]), None);
        let mut not_array = good.clone();
        not_array[8] = 0x52; // the top object is now a string
        assert_eq!(decode_strings(&not_array), None);
        let mut bad_ref = good.clone();
        bad_ref[9] = 0x09; // a reference to object 9 of 3
        assert_eq!(decode_strings(&bad_ref), None);
        let mut not_bplist = good;
        not_bplist[..8].copy_from_slice(b"bplist01");
        assert_eq!(decode_strings(&not_bplist), None);
    }
}

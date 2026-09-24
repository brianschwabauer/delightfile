//! Numbers written for a person to read.
//!
//! A count a person reads — "1,000,000 files", "12,345 triangles" — is read
//! faster with its thousands grouped: the eye sees *a million* in the commas
//! before it has counted the zeros. Log lines, byte offsets, line numbers and
//! the like stay bare, because those are copied and compared rather than read.
//!
//! It lives in the core rather than in the app because both crates print
//! counts. The ops, archive and journal messages ("Trashed 1,234 items",
//! "Restored 12,000 items from the trash") are built here, in df-core, and the
//! crate boundary only runs one way — df-core cannot reach into df-app — so the
//! one copy has to sit on this side of it for the app and the core to agree on
//! how a number looks.

/// A count with thousands separators: `1234567` → `"1,234,567"`.
///
/// Always a comma, never the locale's separator. Nothing else in the program
/// is localized, and a string that switched between `1,000` and `1.000` with
/// the environment would read as a size in one of them. One loop over the
/// digits is the whole of what a formatting crate would do here, so there
/// isn't one.
///
/// Takes a `u64` so every count in the program fits: a `u32` or `usize` goes in
/// with `u64::from` or `as u64`, which is lossless on the 64-bit targets this
/// program is built for.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The edges: no comma until there is a fourth digit, and then one every
    /// three counted from the right, never a leading one.
    #[test]
    fn grouped_puts_a_comma_every_three_digits_from_the_right() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(12_345), "12,345");
        assert_eq!(grouped(65535), "65,535");
        assert_eq!(grouped(100_000), "100,000");
        assert_eq!(grouped(1_000_000), "1,000,000");
        assert_eq!(grouped(u64::MAX), "18,446,744,073,709,551,615");
    }
}

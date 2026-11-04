//! Lowercase hex, the only encoding the manifest and signature files use.
//! Uppercase and mixed case are rejected so each value has one spelling.

/// Encode as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        s.push(char::from(
            DIGITS.get(usize::from(b >> 4)).copied().unwrap_or(b'0'),
        ));
        s.push(char::from(
            DIGITS.get(usize::from(b & 15)).copied().unwrap_or(b'0'),
        ));
    }
    s
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decode exactly `N` bytes of lowercase hex; anything else is `None`.
pub fn decode_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != N.checked_mul(2)? {
        return None;
    }
    let mut out = [0u8; N];
    for (o, pair) in out.iter_mut().zip(b.as_chunks::<2>().0) {
        *o = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_strictness() {
        assert_eq!(encode(&[0, 0xab, 0xff]), "00abff");
        assert_eq!(decode_fixed::<3>("00abff"), Some([0, 0xab, 0xff]));
        assert_eq!(decode_fixed::<3>("00ABFF"), None);
        assert_eq!(decode_fixed::<3>("00abf"), None);
        assert_eq!(decode_fixed::<3>("00abffaa"), None);
        assert_eq!(decode_fixed::<1>("0g"), None);
        assert_eq!(decode_fixed::<1>("+1"), None);
    }
}

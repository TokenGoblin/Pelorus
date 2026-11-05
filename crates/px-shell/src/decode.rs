//! Decoding a fetched page's bytes to text: a byte-order mark, the
//! `Content-Type` charset, then a prescan for `<meta charset>` /
//! `<meta http-equiv=content-type>` in the first 1024 bytes (the HTML
//! encoding-sniffing order, in outline). Supported: UTF-8, UTF-16 (BOM only),
//! and the single-byte Western set (`windows-1252`, `iso-8859-1`, `us-ascii`,
//! which the Encoding Standard folds into `windows-1252`). Any other label is
//! decoded as UTF-8 with replacement characters; no page is refused for it.

/// windows-1252 for 0x80..=0x9F (the rest of the range maps to itself).
const CP1252: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Enc {
    Utf8,
    Utf16Le,
    Utf16Be,
    Latin,
}

fn label(l: &str) -> Option<Enc> {
    match l.trim().trim_matches(['"', '\'']).to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Enc::Utf8),
        "windows-1252" | "iso-8859-1" | "latin1" | "us-ascii" | "ascii" | "iso8859-1"
        | "l1" | "cp1252" | "x-cp1252" => Some(Enc::Latin),
        "utf-16le" | "utf-16" => Some(Enc::Utf16Le),
        "utf-16be" => Some(Enc::Utf16Be),
        _ => None,
    }
}

/// The charset parameter of a `Content-Type` value.
fn header_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then_some(v.trim())
    })
}

/// A `charset=` declared in the first 1024 bytes of markup.
fn prescan(bytes: &[u8]) -> Option<Enc> {
    let head = bytes.get(..bytes.len().min(1024))?;
    let text: String = head.iter().map(|&b| char::from(b)).collect();
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower.get(from..)?.find("charset") {
        let at = from + i + "charset".len();
        let rest = lower.get(at..)?.trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start().trim_start_matches(['"', '\'']);
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
                .unwrap_or(rest.len());
            if let Some(e) = rest.get(..end).and_then(label) {
                return Some(e);
            }
        }
        from = at;
    }
    None
}

/// Decode `bytes` as a page. `content_type` is the response header, if any.
#[must_use]
pub fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let (enc, skip) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (Enc::Utf8, 3)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (Enc::Utf16Le, 2)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (Enc::Utf16Be, 2)
    } else {
        let enc = content_type
            .and_then(header_charset)
            .and_then(label)
            .or_else(|| prescan(bytes))
            .unwrap_or(Enc::Utf8);
        (enc, 0)
    };
    let body = bytes.get(skip..).unwrap_or_default();
    match enc {
        Enc::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Enc::Latin => body
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => CP1252
                    .get(usize::from(b) - 0x80)
                    .copied()
                    .unwrap_or('\u{FFFD}'),
                _ => char::from(b),
            })
            .collect(),
        Enc::Utf16Le | Enc::Utf16Be => {
            let units = body.chunks_exact(2).map(|c| {
                let (a, b) = (
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                );
                if enc == Enc::Utf16Le {
                    u16::from_le_bytes([a, b])
                } else {
                    u16::from_be_bytes([a, b])
                }
            });
            char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_default_and_bom() {
        assert_eq!(decode_html("é".as_bytes(), None), "é");
        assert_eq!(decode_html(b"\xEF\xBB\xBFhi", None), "hi");
    }

    #[test]
    fn header_charset_wins_over_meta() {
        assert_eq!(
            decode_html(b"<meta charset=utf-8>\xE9", Some("text/html; charset=ISO-8859-1")),
            "<meta charset=utf-8>é"
        );
    }

    #[test]
    fn meta_charset_is_prescanned() {
        assert_eq!(decode_html(b"<meta charset=\"windows-1252\">\x93x\x94", None).chars().skip(21).collect::<String>(), "\u{201C}x\u{201D}");
        assert_eq!(
            decode_html(
                b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\">\xE9",
                None
            )
            .chars()
            .last(),
            Some('é')
        );
    }

    #[test]
    fn utf16_with_bom_and_bad_utf8() {
        assert_eq!(decode_html(&[0xFF, 0xFE, b'a', 0, b'b', 0], None), "ab");
        assert_eq!(decode_html(b"a\xFFb", None), "a\u{FFFD}b");
    }
}

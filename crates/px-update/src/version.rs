//! Release versions (hand-parsed semver subset) and data-file dates.

use std::fmt;

/// `MAJOR.MINOR.PATCH`, numeric only. Pre-release and build metadata are not
/// accepted: the update channel ships releases, and "1.2.3-rc1 sorts below
/// 1.2.3" is exactly the kind of rule a downgrade attack lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

fn component(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 9 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b.first() == Some(&b'0') {
        return None;
    }
    s.parse().ok()
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// Strict parse: exactly three dot-separated canonical decimals of at
    /// most nine digits (no sign, no leading zero, no whitespace).
    pub fn parse(s: &str) -> Option<Version> {
        let mut it = s.split('.');
        let v = Version {
            major: component(it.next()?)?,
            minor: component(it.next()?)?,
            patch: component(it.next()?)?,
        };
        it.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Seconds since the Unix epoch at 00:00:00 UTC of a `YYYYMMDD` date, which
/// is how data-file versions are written (the `fetched` date of
/// `data/*.version`). `None` for an impossible date or a year outside
/// 1970..=9999.
pub fn date_to_unix(yyyymmdd: u32) -> Option<u64> {
    let year = i64::from(yyyymmdd / 10_000);
    let month = i64::from(yyyymmdd / 100 % 100);
    let day = i64::from(yyyymmdd % 100);
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > dim {
        return None;
    }
    // Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days).ok()?.checked_mul(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders() {
        assert_eq!(Version::parse("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(Version::parse("0.0.0"), Some(Version::new(0, 0, 0)));
        assert!(Version::new(1, 10, 0) > Version::new(1, 9, 99));
        assert!(Version::new(2, 0, 0) > Version::new(1, 99, 99));
        assert_eq!(Version::new(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn rejects_everything_non_canonical() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "+1.2.3",
            "-1.2.3",
            "1.2.3-rc1",
            "1.2.3+b",
            " 1.2.3",
            "1.2.3 ",
            "1..3",
            "a.b.c",
            "1.2.1000000000",
            "\u{661}.\u{662}.\u{663}",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(date_to_unix(19_700_101), Some(0));
        assert_eq!(date_to_unix(20_000_101), Some(946_684_800));
        assert_eq!(date_to_unix(20_260_909), Some(1_788_912_000));
        assert_eq!(date_to_unix(20_240_229), Some(1_709_164_800));
        assert_eq!(date_to_unix(20_230_229), None);
        assert_eq!(date_to_unix(20_261_301), None);
        assert_eq!(date_to_unix(20_260_100), None);
        assert_eq!(date_to_unix(0), None);
        assert_eq!(date_to_unix(19_691_231), None);
    }
}

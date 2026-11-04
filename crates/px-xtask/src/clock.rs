//! Today's date in UTC, as `date -u +%Y-%m-%d` prints it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Civil date for a count of days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` for a Unix timestamp in seconds.
pub fn format_date(unix_secs: i64) -> String {
    let (y, m, d) = civil_from_days(unix_secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today, UTC.
pub fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_date(secs)
}

#[cfg(test)]
mod tests {
    use super::format_date;

    #[test]
    fn known_dates() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(1_788_912_000), "2026-09-09");
        assert_eq!(format_date(1_798_675_199), "2026-12-30");
        assert_eq!(format_date(1_798_675_200), "2026-12-31");
        assert_eq!(format_date(-86_400), "1969-12-31");
    }
}

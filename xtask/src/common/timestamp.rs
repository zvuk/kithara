//! ISO 8601 UTC timestamp without `chrono` (used in markdown report
//! headers across `quality`, `health`, etc.).

pub(crate) fn utc_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = epoch_days_to_date(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
/// Howard Hinnant's civil-from-days algorithm.
fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_has_expected_format() {
        let ts = utc_timestamp();
        let re = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$").unwrap();
        assert!(re.is_match(&ts), "timestamp should match ISO 8601: {ts}");
    }

    #[test]
    fn epoch_days_to_date_unix_epoch() {
        let (y, m, d) = epoch_days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn epoch_days_to_date_known_date() {
        // 2024-01-01 is day 19723
        let (y, m, d) = epoch_days_to_date(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }
}

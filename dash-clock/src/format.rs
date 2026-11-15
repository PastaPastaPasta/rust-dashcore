//! Core's time formatting (`util/time.cpp`), shared by the node's log stamp and the lines that
//! show a block time.

use std::fmt::Write;

/// Core's `FormatISO8601DateTime`: `YYYY-MM-DDThh:mm:ssZ`, the day floored for times before
/// 1970.
// Core: util/time.cpp:65-72
pub fn push_iso8601(out: &mut String, secs: i64) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let _ = write!(
        out,
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    );
}

/// Core's `FormatISO8601DateTime` as a string.
pub fn format_iso8601_date_time(secs: i64) -> String {
    let mut s = String::new();
    push_iso8601(&mut s, secs);
    s
}

/// The proleptic Gregorian date of a day count from 1970-01-01 (H. Hinnant's `civil_from_days`,
/// the algorithm behind `std::chrono::year_month_day`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 {
        mp + 3
    } else {
        mp - 9
    }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_matches_core_s_util_tests() {
        // Core v24.0.0-rc.3: src/test/util_tests.cpp:296-302 (`util_FormatISO8601DateTime`).
        assert_eq!(format_iso8601_date_time(971890963199), "32767-12-31T23:59:59Z");
        assert_eq!(format_iso8601_date_time(971890876800), "32767-12-31T00:00:00Z");
        assert_eq!(format_iso8601_date_time(1317425777), "2011-09-30T23:36:17Z");
        assert_eq!(format_iso8601_date_time(0), "1970-01-01T00:00:00Z");
        // `std::chrono::floor<days>` before the epoch and a leap day (not in Core's test).
        assert_eq!(format_iso8601_date_time(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_iso8601_date_time(951_782_400), "2000-02-29T00:00:00Z");
    }
}

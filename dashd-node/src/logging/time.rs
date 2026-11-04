//! The line timestamp (`LogTimestampStr`, `FormatISO8601DateTime`).

use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub use dash_clock::format_iso8601_date_time;
use dash_clock::push_iso8601;

/// Core's `LogTimestampStr` stamp: the system time (never the mock time), with microseconds if
/// asked, then ` (mocktime: N)` while a mock time is set, then a space.
// Core: logging.cpp:386-409
pub(super) fn push_timestamp(out: &mut String, micros: bool, mocktime: i64) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_else(|e| -(e.duration().as_micros() as i64));
    push_iso8601(out, now / 1_000_000);
    if micros {
        out.pop();
        let _ = write!(out, ".{:06}Z", now % 1_000_000);
    }
    if mocktime > 0 {
        let _ = write!(out, " (mocktime: {mocktime})");
    }
    out.push(' ');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_has_micros_and_mocktime_when_asked() {
        let mut s = String::new();
        push_timestamp(&mut s, true, 1_600_000_000);
        // 2026-10-12T12:34:56.123456Z (mocktime: 1600000000)
        assert_eq!(s.len(), "YYYY-MM-DDThh:mm:ss.uuuuuuZ".len() + " (mocktime: 1600000000) ".len());
        assert!(s.ends_with("Z (mocktime: 1600000000) "), "{s}");
        let mut s = String::new();
        push_timestamp(&mut s, false, 0);
        assert_eq!(s.len(), "YYYY-MM-DDThh:mm:ssZ ".len());
    }
}

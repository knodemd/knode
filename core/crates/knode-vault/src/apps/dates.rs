//! The dates note apps write, read as [`Timestamp`]s.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use knode_format::Timestamp;

/// Seconds between the Unix epoch and 2001-01-01, where Core Data dates (Apple Notes, Bear)
/// start.
const CORE_DATA_EPOCH: f64 = 978_307_200.0;

/// A Core Data date: seconds since 2001-01-01 UTC.
pub(crate) fn core_data(seconds: f64) -> Option<Timestamp> {
    seconds
        .is_finite()
        .then(|| Timestamp(((seconds + CORE_DATA_EPOCH) * 1000.0).round() as i64))
        .filter(|ts| ts.0 > 0)
}

fn from_utc(dt: NaiveDateTime) -> Timestamp {
    Timestamp(dt.and_utc().timestamp_millis())
}

/// An ISO 8601 date and time, with or without fractions and offset (UTC when it has none), or a
/// date alone.
pub(crate) fn iso(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(Timestamp(dt.with_timezone(&Utc).timestamp_millis()));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%.f %z",
        "%Y-%m-%d %H:%M:%S %z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(text, format) {
            return Some(Timestamp(dt.with_timezone(&Utc).timestamp_millis()));
        }
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(text, format) {
            return Some(from_utc(dt));
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(from_utc)
}

/// An Evernote date: `20201231T235959Z`.
pub(crate) fn enex(text: &str) -> Option<Timestamp> {
    NaiveDateTime::parse_from_str(text.trim(), "%Y%m%dT%H%M%SZ")
        .ok()
        .map(from_utc)
}

/// A date as Notion writes it in an export, `October 20, 2026 9:30 AM` or `October 20, 2026`,
/// read as UTC (the export does not say the time zone), or ISO 8601.
pub(crate) fn notion(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    // A range, `October 20, 2026 → October 22, 2026`: its start.
    let text = text.split(" → ").next().unwrap_or(text).trim();
    for format in ["%B %d, %Y %I:%M %p", "%B %e, %Y %I:%M %p", "%Y/%m/%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(text, format) {
            return Some(from_utc(dt));
        }
    }
    for format in ["%B %d, %Y", "%B %e, %Y", "%Y/%m/%d"] {
        if let Some(dt) = NaiveDate::parse_from_str(text, format)
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
        {
            return Some(from_utc(dt));
        }
    }
    iso(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_app_dates() {
        let ts = Timestamp(1_792_489_800_000); // 2026-10-20T09:50:00Z
        assert_eq!(iso("2026-10-20T09:50:00Z"), Some(ts));
        assert_eq!(iso("2026-10-20T11:50:00+02:00"), Some(ts));
        assert_eq!(iso("2026-10-20T09:50:00.000"), Some(ts));
        assert_eq!(enex("20261020T095000Z"), Some(ts));
        assert_eq!(notion("October 20, 2026 9:50 AM"), Some(ts));
        assert_eq!(
            notion("October 20, 2026"),
            Some(Timestamp(1_792_454_400_000))
        );
        assert_eq!(core_data(1_792_489_800.0 - CORE_DATA_EPOCH), Some(ts));
        assert_eq!(iso("yesterday"), None);
    }
}

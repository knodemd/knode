//! Reading the timestamps the format writes: RFC 3339 in UTC with milliseconds.

use knode_format::Timestamp;

/// Reads `2026-10-20T09:30:00.000Z`, exactly as [`Timestamp`] displays it.
pub(crate) fn parse(text: &str) -> Option<Timestamp> {
    let b = text.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second, ms) = (num(11..13)?, num(14..16)?, num(17..19)?, num(20..23)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let ts = Timestamp(((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000) + ms);
    // Reject dates that do not exist, such as February 30.
    (ts.to_string() == text).then_some(ts)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar. Howard Hinnant's
/// algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_timestamps_display() {
        for ms in [0, 1_792_489_800_123, 951_782_400_000, 4_102_444_799_999] {
            let ts = Timestamp(ms);
            assert_eq!(parse(&ts.to_string()), Some(ts));
        }
        assert_eq!(parse("2026-02-30T00:00:00.000Z"), None);
        assert_eq!(parse("2026-10-20"), None);
        assert_eq!(parse("2026-10-20T09:30:00Z"), None);
    }
}

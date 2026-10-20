//! Dates as reading exports write them, read into ISO 8601.

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// A Kindle date, `Monday, January 5, 2026 9:15:02 PM`, as `2026-01-05T21:15:02` (the time of
/// the device, which the file does not give). `None` for other languages and forms.
pub(crate) fn kindle(text: &str) -> Option<String> {
    let text = text.trim();
    // Drop the day of the week.
    let rest = text.split_once(", ").map(|(_, r)| r).unwrap_or(text);
    let mut parts = rest.split_whitespace();
    let month = parts.next()?.to_lowercase();
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let day: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
    let year: u32 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let meridiem = parts.next().map(str::to_ascii_uppercase);
    let mut hms = time.split(':').map(|p| p.parse::<u32>().ok());
    let mut hour = hms.next()??;
    let minute = hms.next()??;
    let second = hms.next().flatten().unwrap_or(0);
    match meridiem.as_deref() {
        Some("PM") if hour < 12 => hour += 12,
        Some("AM") if hour == 12 => hour = 0,
        _ => {}
    }
    valid(year, month as u32, day, hour, minute, second)
        .then(|| format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"))
}

/// A PDF date, `D:20260102103000+01'00'`, as `2026-01-02T10:30:00+01:00`; a date without a
/// time as `2026-01-02`.
pub(crate) fn pdf(text: &str) -> Option<String> {
    let text = text.trim().trim_start_matches("D:");
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| digits.get(r).and_then(|d| d.parse::<u32>().ok());
    let (year, month, day) = (num(0..4)?, num(4..6)?, num(6..8)?);
    if digits.len() < 12 {
        return valid(year, month, day, 0, 0, 0).then(|| format!("{year:04}-{month:02}-{day:02}"));
    }
    let (hour, minute) = (num(8..10)?, num(10..12)?);
    let second = num(12..14).unwrap_or(0);
    if !valid(year, month, day, hour, minute, second) {
        return None;
    }
    let rest = &text[digits.len()..];
    let zone = match rest.chars().next() {
        Some('Z') => "Z".to_string(),
        Some(sign @ ('+' | '-')) => {
            let z: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
            match (z.get(0..2), z.get(2..4)) {
                (Some(h), Some(m)) => format!("{sign}{h}:{m}"),
                (Some(h), None) => format!("{sign}{h}:00"),
                _ => String::new(),
            }
        }
        _ => String::new(),
    };
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{zone}"
    ))
}

/// A Readwise date, `2026-01-02 10:15:30+00:00`, as `2026-01-02T10:15:30+00:00`. Other text is
/// kept as it is.
pub(crate) fn readwise(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let b = text.as_bytes();
    if b.len() >= 19 && b[4] == b'-' && b[7] == b'-' && b[10] == b' ' {
        return Some(format!("{}T{}", &text[..10], &text[11..]));
    }
    Some(text.to_string())
}

fn valid(year: u32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> bool {
    (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour < 24
        && minute < 60
        && second < 61
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_kindle_dates() {
        assert_eq!(
            kindle("Monday, January 5, 2026 9:15:02 PM").as_deref(),
            Some("2026-01-05T21:15:02")
        );
        assert_eq!(
            kindle("Tuesday, February 3, 2026 12:01:00 AM").as_deref(),
            Some("2026-02-03T00:01:00")
        );
        assert_eq!(kindle("lunes, 5 de enero de 2026 21:15:02"), None);
    }

    #[test]
    fn reads_pdf_dates() {
        assert_eq!(
            pdf("D:20260102103000+01'00'").as_deref(),
            Some("2026-01-02T10:30:00+01:00")
        );
        assert_eq!(
            pdf("D:20260102103000Z").as_deref(),
            Some("2026-01-02T10:30:00Z")
        );
        assert_eq!(pdf("D:20260102").as_deref(), Some("2026-01-02"));
        assert_eq!(pdf("yesterday"), None);
    }

    #[test]
    fn reads_readwise_dates() {
        assert_eq!(
            readwise("2026-01-02 10:15:30+00:00").as_deref(),
            Some("2026-01-02T10:15:30+00:00")
        );
        assert_eq!(readwise(""), None);
    }
}

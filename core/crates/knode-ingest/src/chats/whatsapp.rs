//! WhatsApp chat exports: "Export chat" on the phone writes a `.zip` (or, on Android, a `.txt`
//! and the media beside it) with the chat as text and, when asked for, its media.
//!
//! The text file is `_chat.txt` on iOS and `WhatsApp Chat with <name>.txt` on Android (the
//! name in the phone's language). Every message starts with a line that begins with its date
//! and time, in the phone's locale:
//!
//! | Family | Example |
//! | --- | --- |
//! | iOS, bracketed | `[04/03/2026, 09:15:02] Ada: Hello` |
//! | Android, dash | `04/03/2026, 09:15 - Ada: Hello` |
//! | Day, month, year | `04/03/2026`, `04.03.26`, `4-3-2026` |
//! | Month, day, year | `3/4/26` (United States and others) |
//! | Year first | `2026-03-04`, `2026. 03. 04.` |
//! | 12 hours | `9:15 AM`, `9:15 a. m.`, `오후 9:15`, with a narrow no-break space before `AM` |
//!
//! Lines that do not start with a date continue the message before them. Day and month are
//! told apart by a number over 12 when there is one, else by the order that keeps the
//! messages in time, else by the separator and the clock. What WhatsApp writes itself ("Ada
//! added Grace", the encryption notice) has no sender on Android and a left-to-right mark
//! before it on iOS. Files are `<attached: name>` on iOS and `name (file attached)` on Android;
//! an export without media says `<Media omitted>` (Android) or `image omitted` (iOS).
//!
//! A WhatsApp export has no id for the chat or its messages: the chat is found again by its
//! name (from the file names of the export), and a message by its minute, sender and text.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{NaiveDate, NaiveDateTime};
use regex::Regex;

use super::ChatFormat;
use super::files::{Files, file_name, join, parent, stem};
use super::model::{Chat, ChatMessage, Media, stable_hash};
use super::render::escape_text;

/// The line that starts a message. Groups: 1 `[`, 2-4 the date's numbers, 5 an East Asian
/// morning or afternoon before the time, 6-8 hours, minutes and seconds, 9 `a` or `p` of
/// AM or PM, 10 what ends the date (`] ` or ` - `), 11 the rest.
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^[\x{200E}\x{200F}\x{FEFF}\x{202A}-\x{202E}]*(\[)?",
        r"(\d{1,4})[./-] ?(\d{1,2})[./-] ?(\d{1,4})\.?,?\s+",
        r"(?:(오전|오후|午前|午後|上午|下午)\s?)?",
        r"(\d{1,2})[:.](\d{2})(?:[:.](\d{2}))?",
        r"(?:\s*([AaPp])\.?\s?[Mm]\.?)?",
        r"(\]\s|\s[-–]\s)(.*)$"
    ))
    .expect("a valid regex")
});

/// `<attached: name>` on iOS, in any language: `<angehängt: name>`, `<adjunto: name>`.
static ATTACHED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<([^<>:\n]{1,40}):\s*([^<>\n]+?\.[A-Za-z0-9]{1,6})>").expect("a valid regex")
});

/// `name (file attached)` on Android, in any language.
static FILE_ATTACHED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+?\.[A-Za-z0-9]{2,6})\s\(([^()\n]{3,40})\)$").expect("a valid regex")
});

/// `image omitted` on iOS.
static IOS_OMITTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(image|video|audio|sticker|gif|document|contact card) omitted$")
        .expect("a valid regex")
});

/// Words that "omitted" is in the languages of `<Media omitted>`.
const OMITTED_WORDS: &[&str] = &[
    "omit",
    "omis",
    "omess",
    "ausgeschlossen",
    "ocult",
    "weggelaten",
    "utelämn",
    "udeladt",
    "utelat",
    "pominięt",
    "vynech",
    "kihagy",
    "отсутств",
    "пропущ",
    "dahil edilmedi",
];

/// Words that "file attached" is in the languages of `name (file attached)`.
const ATTACHED_WORDS: &[&str] = &[
    "attach",
    "angehängt",
    "adjunt",
    "joint",
    "allegat",
    "anexad",
    "bijgevoegd",
    "bifogad",
    "vedhæft",
    "vedlagt",
    "załącz",
    "прикреп",
    "eklendi",
];

/// Prepositions between "WhatsApp Chat" and the name in the export's file name.
const NAME_WORDS: &[&str] = &[
    "with", "mit", "con", "com", "avec", "met", "med", "z", "ze", "s", "amb", "cu",
];

/// The chat file of a WhatsApp export, if it has one: `_chat.txt`, or a `.txt` named after
/// WhatsApp, in `root` (see [`super::in_root`]) and the shallowest.
pub(crate) fn locate(files: &mut Files, root: Option<&str>) -> Option<String> {
    let candidate = match &files.single {
        Some(single) if single.to_ascii_lowercase().ends_with(".txt") => Some(single.clone()),
        Some(_) => None,
        None => {
            let shallowest = |wanted: &dyn Fn(&str) -> bool| {
                files
                    .names()
                    .iter()
                    .filter(|n| super::in_root(n, root) && wanted(file_name(n)))
                    .min_by_key(|n| (n.matches('/').count(), n.len()))
                    .cloned()
            };
            shallowest(&|n| n == "_chat.txt").or_else(|| {
                shallowest(&|n| {
                    let lower = n.to_lowercase();
                    lower.ends_with(".txt") && lower.contains("whatsapp")
                })
            })
        }
    }?;
    let head = files.peek(&candidate, 16 * 1024).ok()?;
    let head = String::from_utf8_lossy(&head);
    head.lines()
        .filter(|l| !l.trim().is_empty())
        .take(10)
        .any(|l| header(l).is_some())
        .then_some(candidate)
}

/// The first line of a message, read.
#[derive(Debug, Clone)]
struct Header {
    bracketed: bool,
    /// The date's three numbers, as written.
    date: [String; 3],
    /// Which separator the date uses.
    separator: char,
    hour: u32,
    minute: u32,
    second: u32,
    /// `Some(true)` for the afternoon on a 12-hour clock.
    pm: Option<bool>,
    /// Whether that is written `AM` or `PM`, as in the United States, rather than `a. m.`.
    plain_am_pm: bool,
    rest: String,
}

fn header(line: &str) -> Option<Header> {
    let c = HEADER.captures(line)?;
    let bracketed = c.get(1).is_some();
    let end = c.get(10)?.as_str();
    if bracketed != end.starts_with(']') {
        return None;
    }
    let number = |i: usize| c.get(i).map(|m| m.as_str().to_string()).unwrap_or_default();
    let separator = line[c.get(2)?.end()..].chars().next().unwrap_or('/');
    let pm = match (c.get(5).map(|m| m.as_str()), c.get(9).map(|m| m.as_str())) {
        (Some("오후" | "午後" | "下午"), _) => Some(true),
        (Some(_), _) => Some(false),
        (None, Some(letter)) => Some(letter.eq_ignore_ascii_case("p")),
        (None, None) => None,
    };
    let plain_am_pm = c.get(9).is_some_and(|m| {
        line[m.start()..].starts_with("AM") || line[m.start()..].starts_with("PM")
    });
    Some(Header {
        bracketed,
        plain_am_pm,
        date: [number(2), number(3), number(4)],
        separator,
        hour: number(6).parse().ok()?,
        minute: number(7).parse().ok()?,
        second: c.get(8).map_or(Some(0), |m| m.as_str().parse().ok())?,
        pm,
        rest: number(11),
    })
}

/// The order of day and month in dates whose year comes last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    DayMonth,
    MonthDay,
}

impl Order {
    /// How the order is kept in [`keys::DATE_ORDER`](super::keys::DATE_ORDER).
    fn id(self) -> &'static str {
        match self {
            Order::DayMonth => "day-month",
            Order::MonthDay => "month-day",
        }
    }

    fn from_id(id: &str) -> Option<Order> {
        match id {
            "day-month" => Some(Order::DayMonth),
            "month-day" => Some(Order::MonthDay),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Order::DayMonth => "day/month",
            Order::MonthDay => "month/day",
        }
    }
}

/// What told the order of day and month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Basis {
    /// Every date has its year first: nothing to tell.
    YearFirst,
    /// A number over 12.
    Numbers,
    /// Only one order keeps the messages in time.
    Chronology,
    /// Nothing in the dates: the locale's habit.
    Habit,
}

fn date(header: &Header, order: Order) -> Option<NaiveDate> {
    let n = |i: usize| header.date[i].parse::<i32>().ok();
    let year = |s: &str, v: i32| if s.len() <= 2 { 2000 + v } else { v };
    let (y, m, d) = if header.date[0].len() == 4 {
        (n(0)?, n(1)?, n(2)?)
    } else {
        let y = year(&header.date[2], n(2)?);
        match order {
            Order::DayMonth => (y, n(1)?, n(0)?),
            Order::MonthDay => (y, n(0)?, n(1)?),
        }
    };
    NaiveDate::from_ymd_opt(y, u32::try_from(m).ok()?, u32::try_from(d).ok()?)
}

fn time(header: &Header, order: Order) -> Option<NaiveDateTime> {
    let hour = match header.pm {
        Some(true) if header.hour < 12 => header.hour + 12,
        Some(false) if header.hour == 12 => 0,
        _ => header.hour,
    };
    date(header, order)?.and_hms_opt(hour, header.minute, header.second)
}

/// Whether the dates can be read in `order`: none has a month over 12 in it. (A date that
/// is not one either way, such as 31/02, does not count.)
fn possible(headers: &[Header], order: Order) -> bool {
    let month = match order {
        Order::DayMonth => 1,
        Order::MonthDay => 0,
    };
    headers
        .iter()
        .filter(|h| h.date[0].len() != 4)
        .all(|h| h.date[month].parse::<u32>().is_ok_and(|m| m <= 12))
}

/// The order of day and month: a number over 12 tells, else the order that keeps the messages
/// in time, else the locale's habit: `/` with `AM` and `PM` is the United States.
fn order(headers: &[Header]) -> (Order, Basis) {
    let last_year = headers.iter().filter(|h| h.date[0].len() != 4);
    if last_year.clone().next().is_none() {
        return (Order::DayMonth, Basis::YearFirst);
    }
    let mut first_big = false;
    let mut second_big = false;
    for h in last_year.clone() {
        first_big |= h.date[0].parse::<u32>().is_ok_and(|n| n > 12);
        second_big |= h.date[1].parse::<u32>().is_ok_and(|n| n > 12);
    }
    match (first_big, second_big) {
        (true, false) => return (Order::DayMonth, Basis::Numbers),
        (false, true) => return (Order::MonthDay, Basis::Numbers),
        _ => {}
    }
    let backwards = |order: Order| {
        let times: Vec<Option<NaiveDateTime>> = headers.iter().map(|h| time(h, order)).collect();
        let invalid = times.iter().filter(|t| t.is_none()).count();
        let valid: Vec<NaiveDateTime> = times.into_iter().flatten().collect();
        invalid * 1000 + valid.windows(2).filter(|w| w[1] < w[0]).count()
    };
    let (dm, md) = (backwards(Order::DayMonth), backwards(Order::MonthDay));
    if dm != md {
        let order = if dm < md {
            Order::DayMonth
        } else {
            Order::MonthDay
        };
        return (order, Basis::Chronology);
    }
    let twelve_hours = headers.iter().any(|h| h.plain_am_pm);
    let slash = last_year.clone().next().is_some_and(|h| h.separator == '/');
    if slash && twelve_hours {
        (Order::MonthDay, Basis::Habit)
    } else {
        (Order::DayMonth, Basis::Habit)
    }
}

/// Bidirectional marks WhatsApp puts around names, numbers and its own lines.
fn is_bidi(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{FEFF}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn without_bidi(text: &str) -> String {
    text.chars().filter(|c| !is_bidi(*c)).collect()
}

/// A chat read from an export, and what is worth knowing about how.
pub(crate) struct Read {
    pub chat: Chat,
    /// Said when the dates could be read either way.
    pub note: Option<String>,
    /// Lines that start like a message but whose date is not one, with why.
    pub skipped: Vec<(String, String)>,
    /// How many secrets in the messages were replaced.
    pub redacted: usize,
}

/// Why a chat cannot be imported into the node it was imported into before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrderConflict(pub String);

/// Reads the chat in `chat_file`, named `name` (see [`name`]). `stored` is the order of day and
/// month this chat was imported with before, kept on its node: it is used again, so the same
/// chat always reads its dates the same way. When the new export cannot be read in that order,
/// nothing is read and the conflict says why.
pub(crate) fn read(
    files: &mut Files,
    chat_file: &str,
    name: &str,
    stored: Option<&str>,
) -> std::io::Result<Result<Read, OrderConflict>> {
    let text = files.read_to_string(chat_file)?;
    let text = text.trim_start_matches('\u{feff}');

    // Messages: a header line, and the lines after it up to the next one.
    let mut entries: Vec<(Header, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        match header(line) {
            Some(h) => entries.push((h, Vec::new())),
            None => {
                if let Some((_, more)) = entries.last_mut() {
                    more.push(line.to_string());
                }
            }
        }
    }
    let headers: Vec<Header> = entries.iter().map(|(h, _)| h.clone()).collect();
    let (found, basis) = order(&headers);
    let title = format!("WhatsApp - {name}");
    let mut note = None;
    let order = match stored.and_then(Order::from_id) {
        Some(stored) if basis == Basis::YearFirst || possible(&headers, stored) => stored,
        Some(stored) => {
            return Ok(Err(OrderConflict(format!(
                "this export writes its dates {}, but the chat was imported before reading them \
                 {}, when its dates could be read either way: the messages imported then have \
                 the wrong dates. Delete the node \"{title}\" and import this export again",
                found.name(),
                stored.name()
            ))));
        }
        None => {
            if basis == Basis::Habit {
                note = Some(format!(
                    "{title}: every date could be day/month or month/day; they were read as {}, \
                     and later imports of this chat read them the same way.",
                    found.name()
                ));
            }
            found
        }
    };

    let dir = parent(chat_file).to_string();
    let mut chat = Chat::new(ChatFormat::WhatsApp, name.to_lowercase(), title);
    if basis != Basis::YearFirst {
        chat.properties
            .insert(super::keys::DATE_ORDER.into(), order.id().into());
    }
    let mut skipped = Vec::new();
    let mut redacted = 0;
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (h, more) in entries {
        let Some(time) = time(&h, order) else {
            skipped.push((
                format!("{} ({})", chat.title, crate::text::one_line(&h.rest, 40)),
                format!("its date is not a date read {}", order.name()),
            ));
            continue;
        };
        let mut raw = h.rest.clone();
        for line in &more {
            raw.push('\n');
            raw.push_str(line);
        }
        let (sender, body) = split_sender(&raw, h.bracketed);
        let mut message_text = without_bidi(&body);
        let mut media = Vec::new();
        if sender.is_some() {
            media = take_media(&mut message_text, |name| {
                let entry = join(&dir, name);
                files.contains(&entry).then_some(entry)
            });
        }

        let (message_text, n) = crate::redact::redact_secrets(&message_text);
        redacted += n;

        let mark = time.format("%Y-%m-%dT%H:%M").to_string();
        let base = stable_hash(&[&mark, sender.as_deref().unwrap_or(""), &without_bidi(&raw)]);
        let n = seen.entry((mark.clone(), base.clone())).or_insert(0);
        let key = if *n == 0 { base } else { format!("{base}-{n}") };
        *n += 1;
        let mut message =
            ChatMessage::new(key, mark, time, sender, escape_text(message_text.trim()));
        message.media = media;
        chat.messages.push(message);
    }
    chat.collect_participants();

    let group_line = chat.messages.iter().any(|m| {
        m.sender.is_none() && {
            let text = m.text.to_lowercase();
            ["group", "grupo", "gruppe", "groupe", "groep"]
                .iter()
                .any(|w| text.contains(w))
        }
    });
    chat.kind = if chat.participants.len() > 2 || group_line {
        "group"
    } else {
        "personal"
    }
    .to_string();
    Ok(Ok(Read {
        chat,
        note,
        skipped,
        redacted,
    }))
}

/// The sender and the text of a message, or no sender for what WhatsApp wrote itself.
fn split_sender(raw: &str, bracketed: bool) -> (Option<String>, String) {
    let Some((name, text)) = raw.split_once(": ") else {
        return (None, raw.to_string());
    };
    let name = without_bidi(name).trim().to_string();
    if name.is_empty() || name.contains('\n') || name.chars().count() > 60 || name.contains('"') {
        return (None, raw.to_string());
    }
    // On iOS, what WhatsApp wrote starts with a left-to-right mark, after the chat's name.
    if bracketed && text.starts_with('\u{200E}') {
        let plain = without_bidi(text);
        let plain = plain.trim();
        if !ATTACHED.is_match(plain) && !IOS_OMITTED.is_match(plain.lines().next().unwrap_or("")) {
            return (None, plain.to_string());
        }
    }
    (Some(name), text.to_string())
}

/// Takes the files out of a message's text: `<attached: name>`, `name (file attached)`,
/// `<Media omitted>` and `image omitted`. `find` gives the path in the export of a file name,
/// when the export has it.
fn take_media(text: &mut String, find: impl Fn(&str) -> Option<String>) -> Vec<Media> {
    let mut media = Vec::new();
    let file = |name: &str| match find(name) {
        Some(entry) => Media::File {
            name: name.to_string(),
            entry,
        },
        None => Media::Missing {
            name: name.to_string(),
        },
    };

    // iOS.
    let mut kept = String::with_capacity(text.len());
    let mut last = 0;
    for c in ATTACHED.captures_iter(text) {
        let whole = c.get(0).expect("the match");
        let name = c[2].trim();
        kept.push_str(&text[last..whole.start()]);
        last = whole.end();
        media.push(file(name));
    }
    kept.push_str(&text[last..]);
    *text = kept;

    // Android: the first line names the file, the rest is its caption.
    let (first, rest) = match text.split_once('\n') {
        Some((first, rest)) => (first.trim().to_string(), Some(rest.to_string())),
        None => (text.trim().to_string(), None),
    };
    if let Some(c) = FILE_ATTACHED.captures(&first) {
        let name = c[1].trim();
        let said = c[2].to_lowercase();
        let found = find(name);
        if found.is_some() || ATTACHED_WORDS.iter().any(|w| said.contains(w)) {
            media.push(file(name));
            *text = rest.unwrap_or_default();
        }
    }

    // Left out of the export.
    let whole = text.trim();
    let lower = whole.to_lowercase();
    if whole.starts_with('<')
        && whole.ends_with('>')
        && !whole.contains(':')
        && whole.chars().count() <= 50
        && OMITTED_WORDS.iter().any(|w| lower.contains(w))
    {
        media.push(Media::Omitted {
            what: "media".into(),
        });
        text.clear();
    } else if let Some(c) = IOS_OMITTED.captures(whole) {
        media.push(Media::Omitted {
            what: c[1].to_lowercase(),
        });
        text.clear();
    }
    media
}

/// The chat's name, from the export's file names: `WhatsApp Chat with Ada.txt` on Android;
/// for `_chat.txt` (iOS), the folder it is in or the `.zip`: `WhatsApp Chat - Ada.zip`. A
/// loose `_chat.txt` takes its folder's name only when the folder is named as WhatsApp names
/// it. `None` when no name is found: a chat is found again by its name, so none is guessed.
pub(crate) fn name(files: &Files, path: &Path, chat_file: &str) -> Option<String> {
    chat_name(chat_file, path, files.single.is_some())
}

fn chat_name(chat_file: &str, path: &Path, single: bool) -> Option<String> {
    let file = file_name(chat_file);
    let file = file
        .strip_suffix(".txt")
        .or_else(|| file.strip_suffix(".TXT"))
        .unwrap_or(file);
    let usable =
        |name: String| Some(name).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("_chat"));
    if !file.eq_ignore_ascii_case("_chat") {
        return after_whatsapp(file).or_else(|| usable(clean_title(file)));
    }
    let inside = file_name(parent(chat_file)).to_string();
    let outside = if single {
        path.parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        stem(path)
    };
    for candidate in [&inside, &outside] {
        if let Some(name) = after_whatsapp(candidate) {
            return Some(name);
        }
    }
    if single && inside.is_empty() {
        return None;
    }
    let container = if inside.is_empty() { outside } else { inside };
    usable(clean_title(&container))
}

/// The name after "WhatsApp Chat" (and a preposition) in a file name.
fn after_whatsapp(name: &str) -> Option<String> {
    let at = (0..name.len()).find(|&i| {
        name.is_char_boundary(i)
            && name
                .as_bytes()
                .get(i..i + "whatsapp".len())
                .is_some_and(|b| b.eq_ignore_ascii_case(b"whatsapp"))
    })?;
    let separator = |c: char| c == '-' || c.is_whitespace();
    let mut rest = name[at + "whatsapp".len()..].trim_start_matches(separator);
    if rest
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("chat"))
    {
        rest = rest[4..].trim_start_matches(separator);
    }
    if let Some((word, after)) = rest.split_once(' ')
        && NAME_WORDS.contains(&word.to_lowercase().as_str())
    {
        rest = after;
    }
    let name = clean_title(rest);
    (!name.is_empty()).then_some(name)
}

/// A name a title can hold: no `[`, `]` or `|`, spaces collapsed.
fn clean_title(name: &str) -> String {
    without_bidi(name)
        .replace(['[', ']', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(lines: &[&str]) -> Vec<Header> {
        lines.iter().map(|l| header(l).expect(l)).collect()
    }

    fn times(lines: &[&str]) -> Vec<String> {
        let hs = headers(lines);
        let (order, _) = order(&hs);
        hs.iter()
            .map(|h| {
                time(h, order)
                    .unwrap()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn reads_ios_bracketed_lines() {
        assert_eq!(
            times(&[
                "[04/03/2026, 09:15:02] Ada: Hello",
                "\u{200e}[13/03/2026, 21:40:00] Grace: \u{200e}<attached: 00000012-PHOTO.jpg>",
            ]),
            ["2026-03-04 09:15:02", "2026-03-13 21:40:00"]
        );
        // Month first, 12 hours with a narrow no-break space, two-digit year.
        assert_eq!(
            times(&[
                "[3/4/26, 9:15:02\u{202f}AM] Ada: Hello",
                "[3/14/26, 12:05:00\u{202f}PM] Grace: Hi",
                "[3/14/26, 12:30:00 AM] Grace: Late",
            ]),
            [
                "2026-03-04 09:15:02",
                "2026-03-14 12:05:00",
                "2026-03-14 00:30:00"
            ]
        );
        // Dots, and no comma.
        assert_eq!(
            times(&["[04.03.26 09:15:02] Ada: Hallo"]),
            ["2026-03-04 09:15:02"]
        );
    }

    #[test]
    fn reads_android_dash_lines() {
        assert_eq!(
            times(&[
                "04/03/2026, 09:15 - Ada: Hello",
                "13/03/2026, 21:40 - Grace: Hi"
            ]),
            ["2026-03-04 09:15:00", "2026-03-13 21:40:00"]
        );
        assert_eq!(
            times(&[
                "3/4/26, 9:15\u{202f}PM - Ada: Hello",
                "3/4/26, 10:01 PM - Grace: Hi"
            ]),
            ["2026-03-04 21:15:00", "2026-03-04 22:01:00"]
        );
        // Spanish 12 hours, German dots, en dash.
        assert_eq!(
            times(&["4/3/26, 9:15 p. m. - Ada: Hola"]),
            ["2026-03-04 21:15:00"]
        );
        assert_eq!(
            times(&["04.03.26, 09:15 – Ada: Hallo"]),
            ["2026-03-04 09:15:00"]
        );
    }

    #[test]
    fn reads_years_first() {
        assert_eq!(
            times(&[
                "2026-03-04 09:15 - Ada: Hej",
                "[2026-03-04, 09:16:30] Ada: Hej",
                "2026. 03. 04. 9:17 - Ada: Szia",
                "2026. 3. 4. 오후 9:18 - Ada: 안녕",
                "2026/03/04 午前9:19 - Ada: こんにちは",
            ]),
            [
                "2026-03-04 09:15:00",
                "2026-03-04 09:16:30",
                "2026-03-04 09:17:00",
                "2026-03-04 21:18:00",
                "2026-03-04 09:19:00"
            ]
        );
    }

    #[test]
    fn tells_day_and_month_apart() {
        // No number over 12: the order that keeps the messages in time.
        assert_eq!(
            times(&[
                "01/02/2026, 10:00 - Ada: a",
                "02/02/2026, 10:00 - Ada: b",
                "01/03/2026, 10:00 - Ada: c"
            ]),
            [
                "2026-02-01 10:00:00",
                "2026-02-02 10:00:00",
                "2026-03-01 10:00:00"
            ]
        );
        // Nothing tells: `/` with 24 hours is day first, with 12 hours month first.
        assert_eq!(
            times(&["01/02/2026, 10:00 - Ada: a"]),
            ["2026-02-01 10:00:00"]
        );
        assert_eq!(
            times(&["01/02/26, 10:00 AM - Ada: a"]),
            ["2026-01-02 10:00:00"]
        );
    }

    #[test]
    fn says_what_told_the_order() {
        let basis = |lines: &[&str]| order(&headers(lines));
        assert_eq!(
            basis(&["13/03/2026, 10:00 - Ada: a"]),
            (Order::DayMonth, Basis::Numbers)
        );
        assert_eq!(basis(&["2026-03-13 10:00 - Ada: a"]).1, Basis::YearFirst);
        // A United States export on a 24-hour clock with no day over 12 cannot be told.
        assert_eq!(
            basis(&["3/4/26, 10:00 - Ada: a", "3/5/26, 10:00 - Ada: b"]),
            (Order::DayMonth, Basis::Habit)
        );
        let hs = headers(&["3/14/26, 10:00 - Ada: a"]);
        assert!(!possible(&hs, Order::DayMonth));
        assert!(possible(&hs, Order::MonthDay));
    }

    #[test]
    fn rejects_other_lines() {
        assert!(header("Hello there").is_none());
        assert!(header("[04/03/2026, 09:15:02 - Ada: no").is_none());
        assert!(header("04/03/2026, 09:15:02] Ada: no").is_none());
        assert!(header("Meet on 04/03/2026 at 09:15").is_none());
    }

    #[test]
    fn splits_senders_from_system_lines() {
        assert_eq!(
            split_sender("Ada: Hello: there", false),
            (Some("Ada".into()), "Hello: there".into())
        );
        assert_eq!(
            split_sender("Ada added Grace", false),
            (None, "Ada added Grace".into())
        );
        assert_eq!(
            split_sender("Ada changed the subject to \"Plans: May\"", false),
            (None, "Ada changed the subject to \"Plans: May\"".into())
        );
        assert_eq!(
            split_sender("Book Club: \u{200e}Ada added Grace", true),
            (None, "Ada added Grace".into())
        );
        assert_eq!(
            split_sender("\u{202a}+1 555 0100\u{202c}: \u{200e}image omitted", true),
            (Some("+1 555 0100".into()), "\u{200e}image omitted".into())
        );
    }

    #[test]
    fn takes_files_out_of_messages() {
        let find = |name: &str| (name != "gone.jpg").then(|| format!("dir/{name}"));
        let mut text = "<attached: 00000012-PHOTO.jpg>".to_string();
        assert_eq!(
            take_media(&mut text, find),
            [Media::File {
                name: "00000012-PHOTO.jpg".into(),
                entry: "dir/00000012-PHOTO.jpg".into()
            }]
        );
        assert_eq!(text, "");

        let mut text = "IMG-20260304-WA0001.jpg (file attached)\nThe beach".to_string();
        let media = take_media(&mut text, find);
        assert!(
            matches!(&media[..], [Media::File { name, .. }] if name == "IMG-20260304-WA0001.jpg")
        );
        assert_eq!(text, "The beach");

        let mut text = "gone.jpg (Datei angehängt)".to_string();
        assert_eq!(
            take_media(&mut text, find),
            [Media::Missing {
                name: "gone.jpg".into()
            }]
        );

        for omitted in [
            "<Media omitted>",
            "<Medien ausgeschlossen>",
            "image omitted",
        ] {
            let mut text = omitted.to_string();
            let media = take_media(&mut text, find);
            assert!(matches!(&media[..], [Media::Omitted { .. }]), "{omitted}");
            assert_eq!(text, "");
        }
        let mut text = "I love it <3 (really)".to_string();
        assert!(take_media(&mut text, |_| None).is_empty());
        let mut text = "<sigh>".to_string();
        assert!(take_media(&mut text, |_| None).is_empty());
    }

    #[test]
    fn names_chats_after_their_export() {
        let p = Path::new("/x/WhatsApp Chat - Book Club.zip");
        assert_eq!(
            chat_name("_chat.txt", p, false).as_deref(),
            Some("Book Club")
        );
        let p = Path::new("/x/export.zip");
        assert_eq!(
            chat_name("WhatsApp Chat with Ada Lovelace.txt", p, false).as_deref(),
            Some("Ada Lovelace")
        );
        assert_eq!(
            chat_name("WhatsApp-Chat mit Grace.txt", p, false).as_deref(),
            Some("Grace")
        );
        assert_eq!(
            chat_name("Chat de WhatsApp con Linus.txt", p, false).as_deref(),
            Some("Linus")
        );
        assert_eq!(
            chat_name("WhatsApp Chat - Team [old]/_chat.txt", p, false).as_deref(),
            Some("Team old")
        );
        assert_eq!(chat_name("_chat.txt", p, false).as_deref(), Some("export"));

        // A loose `_chat.txt` takes its folder's name only when WhatsApp named the folder.
        let loose = Path::new("/x/WhatsApp Chat - Ada/_chat.txt");
        assert_eq!(chat_name("_chat.txt", loose, true).as_deref(), Some("Ada"));
        let loose = Path::new("/Users/example/Downloads/_chat.txt");
        assert_eq!(chat_name("_chat.txt", loose, true), None);
        let zipped = Path::new("/x/_chat.zip");
        assert_eq!(chat_name("_chat.txt", zipped, false), None);

        // Not ASCII: no byte offsets across strings of different lengths.
        let p = Path::new("/x/İİİ WhatsApp Chat - İzmir Ekibi.zip");
        assert_eq!(
            chat_name("_chat.txt", p, false).as_deref(),
            Some("İzmir Ekibi")
        );
        assert_eq!(after_whatsapp("İ").as_deref(), None);
        assert_eq!(
            after_whatsapp("WHATSAPP CHAT WITH Ada").as_deref(),
            Some("Ada")
        );
    }
}

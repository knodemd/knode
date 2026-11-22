//! Calendar events, stored as nodes.
//!
//! The calendars of a Mac are reached only through EventKit, which only the app can use. So
//! the Knode app reads them and hands the events to the core ([`sync`]), and the core decides
//! what to write: which calendars and which days are wanted, one node per event, found again by
//! the calendar's own id, kept in step when the event changes, and marked, never deleted, when
//! it is cancelled or removed from the calendar.
//!
//! An event the space has that the app did not read is not marked removed at once: it may have
//! moved outside the days read. [`sync`] hands it back as missing ([`MissingEvent`]), the app asks
//! its calendar whether the event is still there, and only those gone are marked ([`remove`]).
//!
//! An event is one node of kind [`EVENT_KIND`], written by Knode itself as the `calendar`
//! source ([`SOURCE_ID`]). Its title is the event's title and the day it starts,
//! `Weekly sync (2026-11-24)`, so every occurrence of a recurring event has its own (and, like a
//! recording's, `Weekly sync (2026-11-24) 2` when another node has that title); its body is
//! a short description (when, where, the call, who, the calendar and the notes, cut when they
//! are long); its properties ([`keys`]) hold the same as data.
//!
//! An event is found again by its key ([`CalendarEvent::key`]): the calendar's external id of
//! the event, the same on every device, followed for an occurrence of a recurring event by `@`
//! and the time that occurrence was planned to start.
//!
//! Attendees are named, never written with their mail address, unless the space asks for
//! addresses ([`CalendarOptions::attendee_emails`]); an attendee whose name is the title of a
//! `person` node of the space is written as a `[[link]]` to it.
//!
//! A meeting recorded while an event happens is linked to it ([`happening`],
//! [`link_recording`]). Nothing here reads a calendar or sends anything anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{DateTime, Local, TimeZone};
use knode_format::{
    Error, NewNode, NodeId, NodeUpdate, Properties, Provenance, Result, Space, Timestamp, kinds,
    link_kinds,
};
use serde::{Deserialize, Serialize};

use crate::recordings::free_title;
use crate::sources::{Container, History};

/// The id of the calendar source: the actor of what it writes.
pub const SOURCE_ID: &str = "calendar";
/// The kind of an event's node.
pub const EVENT_KIND: &str = "event";
/// The kind of the nodes attendees are linked to.
pub const PERSON_KIND: &str = "person";
/// The space setting with the [`CalendarOptions`], as JSON.
pub const OPTIONS_SETTING: &str = "calendar.options";

/// The most characters of an event's notes written into its body.
pub const MAX_NOTES: usize = 1500;
/// The most attendees named in an event's body; the rest are counted.
pub const MAX_ATTENDEES: usize = 30;
/// How long before an event starts a recording still counts as its own, in milliseconds.
pub const EARLY_START_MS: i64 = 10 * 60 * 1000;

const DAY_MS: i64 = 24 * 3600 * 1000;

/// The properties of an event's node. Every value is a string; times are Unix milliseconds.
pub mod keys {
    /// What finds the event again: its external id, and `@<start>` for an occurrence.
    pub const KEY: &str = "event.key";
    /// The calendar's external id of the event, shared by every occurrence.
    pub const EXTERNAL_ID: &str = "event.external_id";
    /// The event's title, as the calendar has it.
    pub const TITLE: &str = "event.title";
    /// The id of its calendar on this device.
    pub const CALENDAR_ID: &str = "event.calendar_id";
    /// The name of its calendar.
    pub const CALENDAR: &str = "event.calendar";
    /// When it starts.
    pub const START: &str = "event.start";
    /// When it ends.
    pub const END: &str = "event.end";
    /// The local day it starts, `YYYY-MM-DD`: what finds the events of a day.
    pub const DAY: &str = "event.day";
    /// `true` for an all-day event, else `false`.
    pub const ALL_DAY: &str = "event.all_day";
    /// Where it happens.
    pub const LOCATION: &str = "event.location";
    /// The link to its call, when it has one.
    pub const CALL_LINK: &str = "event.call_link";
    /// The names of its attendees, as a JSON list.
    pub const ATTENDEES: &str = "event.attendees";
    /// The mail addresses of its attendees, as a JSON list, only when the space asks for them.
    pub const ATTENDEE_EMAILS: &str = "event.attendee_emails";
    /// `confirmed`, `tentative`, `cancelled` or `removed`.
    pub const STATUS: &str = "event.status";
    /// Whether it is an occurrence of a recurring event: `true` or `false`.
    pub const RECURRING: &str = "event.recurring";
}

/// Where an event stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventStatus {
    /// It happens. The default.
    #[default]
    Confirmed,
    /// It may happen.
    Tentative,
    /// The organiser cancelled it.
    Cancelled,
    /// It is no longer in the calendar. Only Knode sets it.
    Removed,
}

impl EventStatus {
    /// Its name, as stored and as the protocol takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            EventStatus::Confirmed => "confirmed",
            EventStatus::Tentative => "tentative",
            EventStatus::Cancelled => "cancelled",
            EventStatus::Removed => "removed",
        }
    }

    fn from_text(text: &str) -> EventStatus {
        match text {
            "tentative" => EventStatus::Tentative,
            "cancelled" => EventStatus::Cancelled,
            "removed" => EventStatus::Removed,
            _ => EventStatus::Confirmed,
        }
    }

    /// Whether it no longer happens: cancelled or removed.
    pub fn is_off(self) -> bool {
        matches!(self, EventStatus::Cancelled | EventStatus::Removed)
    }
}

impl fmt::Display for EventStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Someone invited to an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    /// Their name, as the calendar has it. It may be empty, or be a mail address.
    #[serde(default)]
    pub name: String,
    /// Their mail address. Written only when the space asks for addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// An event as the app read it from a calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// The calendar's external id of the event (EventKit's
    /// `calendarItemExternalIdentifier`), the same on every device.
    pub external_id: String,
    /// For an occurrence of a recurring event, when that occurrence was planned to start (its
    /// original start, which does not move when one occurrence is moved). `None` for an event
    /// that does not repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<i64>,
    /// The id of its calendar on this device.
    pub calendar_id: String,
    /// The name of its calendar.
    #[serde(default)]
    pub calendar: String,
    /// Its title.
    #[serde(default)]
    pub title: String,
    /// When it starts.
    pub start: i64,
    /// When it ends.
    pub end: i64,
    /// Whether it lasts whole days.
    #[serde(default)]
    pub all_day: bool,
    /// Where it happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Its link, such as the call's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Its notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Who is invited, the organiser included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Attendee>,
    /// `confirmed`, `tentative` or `cancelled`.
    #[serde(default)]
    pub status: EventStatus,
}

impl CalendarEvent {
    /// What finds the event again: its external id, followed for an occurrence of a recurring
    /// event by `@` and when that occurrence was planned to start.
    pub fn key(&self) -> String {
        let id = self.external_id.trim();
        match self.occurrence {
            Some(at) => format!("{id}@{at}"),
            None => id.to_string(),
        }
    }

    /// The link to the event's call, if its link, place or notes hold one.
    pub fn call_link(&self) -> Option<String> {
        [&self.url, &self.location, &self.notes]
            .into_iter()
            .flatten()
            .find_map(|text| call_link_in(text))
    }
}

/// The hosts whose links are calls, with the path a call's link starts with.
const CALL_HOSTS: &[(&str, &str)] = &[
    ("zoom.us", "/j/"),
    ("zoom.us", "/my/"),
    ("zoom.us", "/w/"),
    ("meet.google.com", "/"),
    ("teams.microsoft.com", "/l/meetup-join"),
    ("teams.microsoft.com", "/meet/"),
    ("teams.live.com", "/meet"),
    ("webex.com", "/"),
    ("whereby.com", "/"),
    ("meet.jit.si", "/"),
    ("facetime.apple.com", "/join"),
    ("app.slack.com", "/huddle"),
    ("chime.aws", "/"),
    ("gotomeeting.com", "/join"),
    ("meet.goto.com", "/"),
    ("around.co", "/"),
    ("discord.gg", "/"),
];

/// The first link to a call in `text`: an `https://` link to a known meeting service.
pub fn call_link_in(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find("https://") {
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '|'))
            .unwrap_or(tail.len());
        let link = tail[..end].trim_end_matches(['.', ',', ';', ')', ']', '}', '!', '?']);
        if is_call_link(link) {
            return Some(link.to_string());
        }
        rest = &tail[end.max("https://".len())..];
    }
    None
}

fn is_call_link(link: &str) -> bool {
    let Some(after) = link.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    let host = host.to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    CALL_HOSTS.iter().any(|(known, prefix)| {
        (host == *known || host.ends_with(&format!(".{known}")))
            && path.starts_with(prefix)
            && path.len() > 1
    })
}

/// What the person chose for the calendar source of a space: how many days back and ahead are
/// kept, and whether attendees' addresses are written. Kept as JSON in [`OPTIONS_SETTING`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarOptions {
    /// How many days before today. With the history `from-today`, never before the day the
    /// source was connected.
    #[serde(default = "default_days_back")]
    pub days_back: u32,
    /// How many days after today.
    #[serde(default = "default_days_ahead")]
    pub days_ahead: u32,
    /// Whether attendees' mail addresses are written. Off unless the person turns it on.
    #[serde(default)]
    pub attendee_emails: bool,
}

fn default_days_back() -> u32 {
    30
}

fn default_days_ahead() -> u32 {
    14
}

impl Default for CalendarOptions {
    fn default() -> Self {
        CalendarOptions {
            days_back: default_days_back(),
            days_ahead: default_days_ahead(),
            attendee_emails: false,
        }
    }
}

impl CalendarOptions {
    /// The most days back.
    pub const MAX_DAYS_BACK: u32 = 3650;
    /// The most days ahead.
    pub const MAX_DAYS_AHEAD: u32 = 365;

    fn check(&self) -> Result<()> {
        if self.days_back > Self::MAX_DAYS_BACK {
            return Err(Error::Invalid(format!(
                "days_back is at most {}",
                Self::MAX_DAYS_BACK
            )));
        }
        if self.days_ahead > Self::MAX_DAYS_AHEAD {
            return Err(Error::Invalid(format!(
                "days_ahead is at most {}",
                Self::MAX_DAYS_AHEAD
            )));
        }
        Ok(())
    }
}

/// The calendar options of the space; the defaults when it has none, or they cannot be read.
pub fn options(space: &Space) -> Result<CalendarOptions> {
    Ok(space
        .setting(OPTIONS_SETTING)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

/// Changes the calendar options of the space, as the person. Returns them.
pub fn set_options(
    space: &mut Space,
    options: &CalendarOptions,
    by: &Provenance,
) -> Result<CalendarOptions> {
    if by.kind != knode_format::ActorKind::Person {
        return Err(Error::Invalid(
            "only a person can change what the calendar brings in".into(),
        ));
    }
    options.check()?;
    let value = if *options == CalendarOptions::default() {
        None
    } else {
        Some(serde_json::to_string(options).map_err(|e| Error::Invalid(e.to_string()))?)
    };
    space.set_setting(OPTIONS_SETTING, value.as_deref(), by)?;
    Ok(*options)
}

/// The days of events kept, from `start` to `end`, in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// The earliest an event may end and be kept.
    pub start: i64,
    /// The latest an event may start and be kept.
    pub end: i64,
}

impl Window {
    /// Whether an event from `start` to `end` is in the window: it ends after the window
    /// starts, and starts before it ends.
    pub fn holds(&self, start: i64, end: i64) -> bool {
        end.max(start) >= self.start && start <= self.end
    }
}

/// The events to keep now: from `days_back` days before the start of today (with
/// [`History::FromToday`], never before `since`, the day the source was connected) to
/// `days_ahead` days after the end of today. `today` is the start of today in local time.
pub fn window(
    history: History,
    since: Timestamp,
    today: Timestamp,
    options: &CalendarOptions,
) -> Window {
    let back = today.0 - i64::from(options.days_back) * DAY_MS;
    let start = match history {
        History::FromToday => back.max(since.0),
        History::Everything => back,
    };
    Window {
        start,
        end: today.0 + (i64::from(options.days_ahead) + 1) * DAY_MS,
    }
}

/// What the app read from the calendars, to write into a space.
#[derive(Debug, Clone, Copy)]
pub struct SyncRequest<'a> {
    /// Every calendar the app read. Events of a calendar read that are no longer there are
    /// handed back as missing; the events of a calendar not read are left as they are.
    pub calendars: &'a [Container],
    /// The calendars the person excluded: never written, and their events left as they are.
    pub excluded: &'a [Container],
    /// The days read. Events outside are left out, and only events in it can be missing.
    pub window: Window,
    /// The events read.
    pub events: &'a [CalendarEvent],
    /// Whether attendees' mail addresses are written.
    pub attendee_emails: bool,
}

/// What writing events did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Events written for the first time.
    pub created: usize,
    /// Events whose node changed.
    pub updated: usize,
    /// Events that were already as the calendar has them.
    pub unchanged: usize,
    /// Events left out: of an excluded calendar, outside the window, cancelled before they
    /// were ever written, or without an id.
    pub skipped: usize,
}

impl SyncReport {
    /// The events written: created or changed.
    pub fn items(&self) -> usize {
        self.created + self.updated
    }
}

/// An event the space has, of a calendar read and in the days read, that was not among the
/// events read: gone from its calendar, or moved outside the days read. The app tells which by
/// asking its calendar for the event ([`remove`] marks those gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingEvent {
    /// Its key ([`CalendarEvent::key`]).
    pub key: String,
    /// The calendar's external id of the event.
    pub external_id: String,
    /// For an occurrence of a recurring event, when it was planned to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<i64>,
    /// The id of its calendar on this device.
    pub calendar_id: String,
}

/// What [`sync`] did, and the events it could not find among those read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synced {
    /// What was written.
    pub report: SyncReport,
    /// The events the space has, of a calendar read and in the window, not among those read.
    pub missing: Vec<MissingEvent>,
}

/// Writes the events the app read into the space, as `by` (the `calendar` source).
///
/// An event is found again by its key; one not in the space is created, one that changed is
/// brought in step (its title, body and properties), one that did not is left alone, so
/// writing the same events twice changes nothing. A cancelled event is marked cancelled; one
/// cancelled before it was ever written is not written. An event the space has, of a calendar
/// read and in the window, that is no longer among the events read is handed back as missing,
/// not changed: it may have moved outside the window ([`remove`] marks those gone). Events of
/// excluded calendars are never written or changed.
pub fn sync(space: &mut Space, request: &SyncRequest<'_>, by: &Provenance) -> Result<Synced> {
    sync_in(space, request, by, &Local)
}

fn sync_in<Tz: TimeZone>(
    space: &mut Space,
    request: &SyncRequest<'_>,
    by: &Provenance,
    zone: &Tz,
) -> Result<Synced>
where
    Tz::Offset: fmt::Display,
{
    let excluded: BTreeSet<&str> = request.excluded.iter().map(|c| c.id.as_str()).collect();
    let mut report = SyncReport::default();
    // The last of the same key wins.
    let mut wanted: BTreeMap<String, &CalendarEvent> = BTreeMap::new();
    for event in request.events {
        let key = event.key();
        if event.external_id.trim().is_empty()
            || excluded.contains(event.calendar_id.as_str())
            || !request.window.holds(event.start, event.end)
        {
            report.skipped += 1;
            continue;
        }
        wanted.insert(key, event);
    }
    let people = people(space, wanted.values().copied(), request.attendee_emails)?;
    for (key, event) in &wanted {
        let existing = find(space, key)?;
        let rendered = render(event, request.attendee_emails, &people, zone);
        match existing {
            None if event.status == EventStatus::Cancelled => report.skipped += 1,
            None => {
                let title = free_title(space, &rendered.title, None)?;
                space.create_node_with_properties(
                    NewNode::note(title, rendered.body).with_kind(EVENT_KIND),
                    &rendered
                        .properties
                        .iter()
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    by,
                )?;
                report.created += 1;
            }
            Some(id) => {
                if bring_in_step(space, &id, rendered, by)? {
                    report.updated += 1;
                } else {
                    report.unchanged += 1;
                }
            }
        }
    }
    let mut missing = Vec::new();
    for calendar in request.calendars {
        if excluded.contains(calendar.id.as_str()) {
            continue;
        }
        for summary in space.find_by_property(keys::CALENDAR_ID, &calendar.id)? {
            if summary.kind != EVENT_KIND {
                continue;
            }
            let props = space.properties(&summary.id)?;
            let Some(key) = props.get(keys::KEY) else {
                continue;
            };
            if wanted.contains_key(key) {
                continue;
            }
            let number = |k: &str| props.get(k).and_then(|v| v.parse::<i64>().ok());
            let (Some(start), Some(end)) = (number(keys::START), number(keys::END)) else {
                continue;
            };
            let status = EventStatus::from_text(props.get(keys::STATUS).map_or("", |s| s));
            if status == EventStatus::Removed || !request.window.holds(start, end) {
                continue;
            }
            let external_id = props
                .get(keys::EXTERNAL_ID)
                .cloned()
                .unwrap_or_else(|| key.clone());
            let occurrence = key
                .strip_prefix(&format!("{external_id}@"))
                .and_then(|at| at.parse::<i64>().ok());
            missing.push(MissingEvent {
                key: key.clone(),
                external_id,
                occurrence,
                calendar_id: calendar.id.clone(),
            });
        }
    }
    Ok(Synced { report, missing })
}

/// Marks the events with these keys removed from their calendar, as `by`: those the app found
/// gone among the [`MissingEvent`]s of [`sync`]. Events of excluded calendars, events already
/// marked and keys the space does not have are left alone. Returns how many were marked.
pub fn remove(
    space: &mut Space,
    event_keys: &[String],
    excluded: &[Container],
    by: &Provenance,
) -> Result<usize> {
    let excluded: BTreeSet<&str> = excluded.iter().map(|c| c.id.as_str()).collect();
    let mut removed = 0;
    for key in event_keys.iter().collect::<BTreeSet<_>>() {
        let Some(id) = find(space, key)? else {
            continue;
        };
        let props = space.properties(&id)?;
        let calendar = props.get(keys::CALENDAR_ID).map_or("", |s| s);
        let status = EventStatus::from_text(props.get(keys::STATUS).map_or("", |s| s));
        if excluded.contains(calendar) || status == EventStatus::Removed {
            continue;
        }
        mark_removed(space, &id, by)?;
        removed += 1;
    }
    Ok(removed)
}

/// The event of the space with this key, if any.
pub fn find(space: &Space, key: &str) -> Result<Option<NodeId>> {
    Ok(space
        .find_by_property(keys::KEY, key)?
        .into_iter()
        .find(|s| s.kind == EVENT_KIND)
        .map(|s| s.id))
}

/// The `person` nodes of the space named like an attendee of these events: their titles, by
/// the attendee's name in lower case.
fn people<'a>(
    space: &Space,
    events: impl Iterator<Item = &'a CalendarEvent>,
    emails: bool,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for event in events {
        for (name, _) in attendee_names(event, emails) {
            let lower = name.to_lowercase();
            if out.contains_key(&lower) {
                continue;
            }
            if let Some(title) = space
                .find_by_title(&name)?
                .into_iter()
                .find(|s| s.kind == PERSON_KIND)
                .map(|s| s.title)
            {
                out.insert(lower, title);
            }
        }
    }
    Ok(out)
}

/// An event as it should be written.
struct Rendered {
    title: String,
    body: String,
    properties: Properties,
}

/// The names to write for the attendees: their name (or, when the space asks for addresses,
/// their address when they have no name), never an address otherwise.
fn attendee_names(event: &CalendarEvent, emails: bool) -> Vec<(String, Option<String>)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for attendee in &event.attendees {
        let name = one_line(&attendee.name);
        let email = attendee
            .email
            .as_deref()
            .map(str::trim)
            .filter(|e| e.contains('@'))
            .map(str::to_string);
        let name = if name.is_empty() || name.contains('@') {
            if !emails {
                continue;
            }
            match email
                .clone()
                .or_else(|| (!name.is_empty()).then(|| name.clone()))
            {
                Some(address) => address,
                None => continue,
            }
        } else {
            name
        };
        if seen.insert(name.to_lowercase()) {
            out.push((name, if emails { email } else { None }));
        }
    }
    out
}

fn render<Tz: TimeZone>(
    event: &CalendarEvent,
    emails: bool,
    people: &BTreeMap<String, String>,
    zone: &Tz,
) -> Rendered
where
    Tz::Offset: fmt::Display,
{
    let title = match one_line(&event.title) {
        t if t.is_empty() => "Event".to_string(),
        t => t,
    };
    let start = zone.timestamp_millis_opt(event.start).single();
    let day = start
        .as_ref()
        .map(|s| s.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let node_title = if day.is_empty() {
        title.clone()
    } else {
        format!("{title} ({day})")
    };
    let attendees = attendee_names(event, emails);
    let call = event.call_link();
    let location = event
        .location
        .as_deref()
        .map(one_line)
        .filter(|l| !l.is_empty());

    let mut lines = Vec::new();
    match event.status {
        EventStatus::Cancelled => lines.push("**Cancelled.**".to_string()),
        EventStatus::Tentative => lines.push("**Tentative.**".to_string()),
        _ => {}
    }
    lines.push(format!(
        "**When:** {}",
        when(event.start, event.end, event.all_day, zone)
    ));
    if let Some(location) = &location
        && Some(location) != call.as_ref()
    {
        lines.push(format!("**Where:** {location}"));
    }
    if let Some(call) = &call {
        lines.push(format!("**Call:** <{call}>"));
    }
    if !attendees.is_empty() {
        let mut named: Vec<String> = attendees
            .iter()
            .take(MAX_ATTENDEES)
            .map(|(name, email)| {
                let shown = match people.get(&name.to_lowercase()) {
                    Some(title) => format!("[[{title}]]"),
                    None => escape(name),
                };
                match email {
                    Some(email) if email != name => format!("{shown} ({email})"),
                    _ => shown,
                }
            })
            .collect();
        if attendees.len() > MAX_ATTENDEES {
            named.push(format!("and {} more", attendees.len() - MAX_ATTENDEES));
        }
        lines.push(format!("**Attendees:** {}", named.join(", ")));
    }
    let calendar = one_line(&event.calendar);
    if !calendar.is_empty() {
        lines.push(format!("**Calendar:** {calendar}"));
    }
    let mut body = lines.join("  \n");
    body.push('\n');
    if let Some(notes) = event
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        body.push('\n');
        body.push_str(&truncate(notes, MAX_NOTES));
        body.push('\n');
    }

    let mut properties = Properties::new();
    properties.insert(keys::KEY.into(), event.key());
    properties.insert(keys::EXTERNAL_ID.into(), event.external_id.trim().into());
    properties.insert(keys::TITLE.into(), title);
    properties.insert(keys::CALENDAR_ID.into(), event.calendar_id.clone());
    properties.insert(keys::CALENDAR.into(), calendar);
    properties.insert(keys::START.into(), event.start.to_string());
    properties.insert(keys::END.into(), event.end.to_string());
    properties.insert(keys::DAY.into(), day);
    properties.insert(keys::ALL_DAY.into(), event.all_day.to_string());
    properties.insert(
        keys::RECURRING.into(),
        event.occurrence.is_some().to_string(),
    );
    properties.insert(keys::STATUS.into(), event.status.as_str().into());
    properties.insert(keys::LOCATION.into(), location.unwrap_or_default());
    properties.insert(keys::CALL_LINK.into(), call.unwrap_or_default());
    let names: Vec<&str> = attendees.iter().map(|(n, _)| n.as_str()).collect();
    properties.insert(keys::ATTENDEES.into(), json_list(&names));
    let addresses: Vec<&str> = attendees.iter().filter_map(|(_, e)| e.as_deref()).collect();
    properties.insert(keys::ATTENDEE_EMAILS.into(), json_list(&addresses));
    Rendered {
        title: node_title,
        body,
        properties,
    }
}

fn json_list(items: &[&str]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        serde_json::to_string(items).unwrap_or_default()
    }
}

/// Brings an event's node in step with how it should be written. Returns whether anything
/// changed. An empty property is removed.
fn bring_in_step(
    space: &mut Space,
    id: &NodeId,
    rendered: Rendered,
    by: &Provenance,
) -> Result<bool> {
    let node = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let title = free_title(space, &rendered.title, Some(*id))?;
    let mut changed = false;
    let update = NodeUpdate {
        title: (node.title != title).then_some(title),
        body: (node.body != rendered.body).then_some(rendered.body),
        ..Default::default()
    };
    if update.title.is_some() || update.body.is_some() {
        space.update_node(id, update, by)?;
        changed = true;
    }
    let current = space.properties(id)?;
    let mut set = Properties::new();
    for (key, value) in rendered.properties {
        if value.is_empty() {
            if current.contains_key(&key) {
                space.remove_property(id, &key)?;
                changed = true;
            }
        } else if current.get(&key) != Some(&value) {
            set.insert(key, value);
        }
    }
    if !set.is_empty() {
        space.set_properties(id, &set, by)?;
        changed = true;
    }
    Ok(changed)
}

const REMOVED_LINE: &str = "**Removed from the calendar.**";

/// Marks an event no longer in its calendar: its status, and a line at the top of its body.
fn mark_removed(space: &mut Space, id: &NodeId, by: &Provenance) -> Result<()> {
    let node = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let body = node
        .body
        .strip_prefix("**Cancelled.**  \n")
        .or_else(|| node.body.strip_prefix("**Tentative.**  \n"))
        .unwrap_or(&node.body);
    if !body.starts_with(REMOVED_LINE) {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(format!("{REMOVED_LINE}  \n{body}")),
                ..Default::default()
            },
            by,
        )?;
    }
    space.set_property(id, keys::STATUS, EventStatus::Removed.as_str(), by)?;
    Ok(())
}

/// An event of a space, as [`happening`] and [`get`] read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInfo {
    /// Its node.
    pub id: NodeId,
    /// Its title, as the calendar has it (without the day the node's title adds).
    pub title: String,
    /// When it starts.
    pub start: i64,
    /// When it ends.
    pub end: i64,
    /// Whether it lasts whole days.
    pub all_day: bool,
    /// The name of its calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
    /// The link to its call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_link: Option<String>,
    /// Where it stands.
    pub status: EventStatus,
    /// The project it is part of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<NodeId>,
}

/// An event, read from its node. Fails with [`Error::NotFound`] without such a node and
/// [`Error::Invalid`] for a node that is not an event.
pub fn get(space: &Space, id: &NodeId) -> Result<EventInfo> {
    let node = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    if node.kind != EVENT_KIND {
        return Err(Error::Invalid(format!("node {id} is not an event")));
    }
    let props = space.properties(id)?;
    let text = |key: &str| props.get(key).filter(|v| !v.is_empty()).cloned();
    let number = |key: &str| text(key).and_then(|v| v.parse::<i64>().ok());
    let start = number(keys::START).unwrap_or_else(|| node.created_at.as_millis());
    Ok(EventInfo {
        id: *id,
        title: text(keys::TITLE).unwrap_or(node.title),
        start,
        end: number(keys::END).unwrap_or(start),
        all_day: text(keys::ALL_DAY).as_deref() == Some("true"),
        calendar: text(keys::CALENDAR),
        call_link: text(keys::CALL_LINK),
        status: EventStatus::from_text(&text(keys::STATUS).unwrap_or_default()),
        project: project_of(space, id)?,
    })
}

fn project_of(space: &Space, id: &NodeId) -> Result<Option<NodeId>> {
    for link in space.links_from(id)? {
        if link.kind == link_kinds::PART_OF
            && space
                .get_node(&link.to)?
                .is_some_and(|n| n.kind == kinds::PROJECT)
        {
            return Ok(Some(link.to));
        }
    }
    Ok(None)
}

/// The event happening at `at` that a meeting recorded then belongs to: a timed event (not
/// all-day, not cancelled or removed) that started at most [`EARLY_START_MS`] after `at` and has
/// not ended. Among several, one with a link to a call comes first, then the one that started
/// closest to `at`.
pub fn happening(space: &Space, at: i64) -> Result<Option<EventInfo>> {
    happening_in(space, at, &Local)
}

fn happening_in<Tz: TimeZone>(space: &Space, at: i64, zone: &Tz) -> Result<Option<EventInfo>>
where
    Tz::Offset: fmt::Display,
{
    let Some(now) = zone.timestamp_millis_opt(at).single() else {
        return Ok(None);
    };
    let days = [
        now.clone() - chrono::Duration::days(1),
        now.clone(),
        now + chrono::Duration::days(1),
    ];
    let mut seen = BTreeSet::new();
    let mut found: Vec<EventInfo> = Vec::new();
    for day in days {
        let day = day.format("%Y-%m-%d").to_string();
        for summary in space.find_by_property(keys::DAY, &day)? {
            if summary.kind != EVENT_KIND || !seen.insert(summary.id) {
                continue;
            }
            let event = get(space, &summary.id)?;
            if event.all_day || event.status.is_off() {
                continue;
            }
            if event.start - EARLY_START_MS <= at && at < event.end.max(event.start + 1) {
                found.push(event);
            }
        }
    }
    found.sort_by_key(|e| (e.call_link.is_none(), (e.start - at).abs(), e.id));
    Ok(found.into_iter().next())
}

/// Links a recording (or any node) to the event it was made at, `related`, and, when
/// `project` is given and the event is part of no project yet, the event `part-of` it. Returns
/// the event.
pub fn link_recording(
    space: &mut Space,
    node: &NodeId,
    event: &NodeId,
    project: Option<&NodeId>,
    by: &Provenance,
) -> Result<EventInfo> {
    let info = get(space, event)?;
    if space.get_node(node)?.is_none() {
        return Err(Error::NotFound(*node));
    }
    if node == event {
        return Err(Error::Invalid("an event cannot be linked to itself".into()));
    }
    space.link(node, event, link_kinds::RELATED, by)?;
    if let Some(project) = project
        && info.project.is_none()
    {
        let is_project = space
            .get_node(project)?
            .ok_or(Error::NotFound(*project))?
            .kind
            == kinds::PROJECT;
        if !is_project {
            return Err(Error::Invalid(format!("node {project} is not a project")));
        }
        space.link(event, project, link_kinds::PART_OF, by)?;
    }
    get(space, event)
}

/// How an event's time reads: `Monday 24 November 2026, 10:00–11:00`, `Monday 24 November 2026,
/// all day`, or both ends in full when it spans days.
fn when<Tz: TimeZone>(start: i64, end: i64, all_day: bool, zone: &Tz) -> String
where
    Tz::Offset: fmt::Display,
{
    let (Some(s), Some(e)) = (
        zone.timestamp_millis_opt(start).single(),
        zone.timestamp_millis_opt(end.max(start)).single(),
    ) else {
        return String::new();
    };
    let day = |t: &DateTime<Tz>| t.format("%A %-d %B %Y").to_string();
    if all_day {
        // An all-day event ends at the start of the day after its last one.
        let last = zone
            .timestamp_millis_opt((end - 1).max(start))
            .single()
            .unwrap_or_else(|| s.clone());
        return if last.date_naive() == s.date_naive() {
            format!("{}, all day", day(&s))
        } else {
            format!("{} to {}, all day", day(&s), day(&last))
        };
    }
    if s.date_naive() == e.date_naive() {
        format!("{}, {}–{}", day(&s), s.format("%H:%M"), e.format("%H:%M"))
    } else {
        format!(
            "{}, {} to {}, {}",
            day(&s),
            s.format("%H:%M"),
            day(&e),
            e.format("%H:%M")
        )
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A name as plain text in Markdown: no `[[link]]` or emphasis made by accident.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '[' | ']' | '*' | '_' | '`' | '<' | '>' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The text cut to at most `max` characters, at a line or word boundary when one is near, with
/// `…` when it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let near = max * 4 / 5;
    let at = cut
        .rfind('\n')
        .filter(|i| cut[..*i].chars().count() >= near)
        .or_else(|| {
            cut.rfind(char::is_whitespace)
                .filter(|i| cut[..*i].chars().count() >= near)
        })
        .unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end())
}

#[cfg(test)]
mod tests;

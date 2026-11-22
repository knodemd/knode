//! Calendar events: the events the app reads through EventKit, written into a space
//! ([`knode_ingest::calendar`]). The same rules as Knode Service's `calendar.*` methods, for
//! when the app writes the space itself.

use knode_ingest::calendar as cal;
use knode_ingest::sources::Container;

use crate::{KnodeSpace, Provenance, Result, parse_id};

/// Where an event stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EventStatus {
    /// It happens.
    Confirmed,
    /// It may happen.
    Tentative,
    /// The organiser cancelled it.
    Cancelled,
    /// It is no longer in the calendar. Only Knode sets it.
    Removed,
}

impl From<cal::EventStatus> for EventStatus {
    fn from(s: cal::EventStatus) -> Self {
        match s {
            cal::EventStatus::Confirmed => EventStatus::Confirmed,
            cal::EventStatus::Tentative => EventStatus::Tentative,
            cal::EventStatus::Cancelled => EventStatus::Cancelled,
            cal::EventStatus::Removed => EventStatus::Removed,
        }
    }
}

impl From<EventStatus> for cal::EventStatus {
    fn from(s: EventStatus) -> Self {
        match s {
            EventStatus::Confirmed => cal::EventStatus::Confirmed,
            EventStatus::Tentative => cal::EventStatus::Tentative,
            EventStatus::Cancelled => cal::EventStatus::Cancelled,
            EventStatus::Removed => cal::EventStatus::Removed,
        }
    }
}

/// A calendar of this device.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarInfo {
    /// Its id on this device.
    pub id: String,
    /// Its name.
    pub name: String,
}

/// Someone invited to an event.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct EventAttendee {
    /// Their name; may be empty or a mail address.
    pub name: String,
    /// Their mail address, written only when the space asks for addresses.
    pub email: Option<String>,
}

/// An event as the app read it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarEventInput {
    /// EventKit's `calendarItemExternalIdentifier`.
    pub external_id: String,
    /// For an occurrence of a recurring event, when it was planned to start.
    pub occurrence: Option<i64>,
    pub calendar_id: String,
    pub calendar: String,
    pub title: String,
    /// Unix milliseconds.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: Option<String>,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub attendees: Vec<EventAttendee>,
    pub status: EventStatus,
}

/// The events read, to write into the space.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarSync {
    /// Every calendar read.
    pub calendars: Vec<CalendarInfo>,
    /// The calendars excluded: never written or changed.
    pub excluded: Vec<CalendarInfo>,
    /// The days read, in Unix milliseconds.
    pub start: i64,
    pub end: i64,
    pub events: Vec<CalendarEventInput>,
    /// Whether attendees' addresses are written.
    pub attendee_emails: bool,
}

/// What writing events did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarSyncReport {
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub skipped: u32,
    /// The keys of the events the space has, of a calendar read and in the days read, that were
    /// not read: gone from their calendar or moved outside the days read. Mark those gone with
    /// `remove_calendar_events`.
    pub missing: Vec<String>,
}

/// An event of the space.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarEventInfo {
    /// Its node.
    pub id: String,
    /// Its title as the calendar has it.
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub calendar: Option<String>,
    pub call_link: Option<String>,
    pub status: EventStatus,
    /// The project it is part of.
    pub project: Option<String>,
}

impl From<cal::EventInfo> for CalendarEventInfo {
    fn from(e: cal::EventInfo) -> Self {
        CalendarEventInfo {
            id: e.id.to_string(),
            title: e.title,
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            calendar: e.calendar,
            call_link: e.call_link,
            status: e.status.into(),
            project: e.project.map(|p| p.to_string()),
        }
    }
}

fn containers(list: Vec<CalendarInfo>) -> Vec<Container> {
    list.into_iter()
        .map(|c| Container::new(c.id, c.name))
        .collect()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[uniffi::export]
impl KnodeSpace {
    /// Writes the events read from the calendars, as `by` (the `calendar` source): one node per
    /// event, found again by its key, updated when it changed, marked when cancelled.
    pub fn sync_calendar(&self, sync: CalendarSync, by: Provenance) -> Result<CalendarSyncReport> {
        let calendars = containers(sync.calendars);
        let excluded = containers(sync.excluded);
        let events: Vec<cal::CalendarEvent> = sync
            .events
            .into_iter()
            .map(|e| cal::CalendarEvent {
                external_id: e.external_id,
                occurrence: e.occurrence,
                calendar_id: e.calendar_id,
                calendar: e.calendar,
                title: e.title,
                start: e.start,
                end: e.end,
                all_day: e.all_day,
                location: e.location,
                url: e.url,
                notes: e.notes,
                attendees: e
                    .attendees
                    .into_iter()
                    .map(|a| cal::Attendee {
                        name: a.name,
                        email: a.email,
                    })
                    .collect(),
                status: e.status.into(),
            })
            .collect();
        let request = cal::SyncRequest {
            calendars: &calendars,
            excluded: &excluded,
            window: cal::Window {
                start: sync.start,
                end: sync.end,
            },
            events: &events,
            attendee_emails: sync.attendee_emails,
        };
        let synced = cal::sync(&mut self.lock(), &request, &by.into())?;
        let r = synced.report;
        Ok(CalendarSyncReport {
            created: count(r.created),
            updated: count(r.updated),
            unchanged: count(r.unchanged),
            skipped: count(r.skipped),
            missing: synced.missing.into_iter().map(|m| m.key).collect(),
        })
    }

    /// Marks the events with these keys removed from their calendar: those of the `missing` of
    /// `sync_calendar` found gone. Returns how many were marked.
    pub fn remove_calendar_events(
        &self,
        keys: Vec<String>,
        excluded: Vec<CalendarInfo>,
        by: Provenance,
    ) -> Result<u32> {
        let excluded = containers(excluded);
        Ok(count(cal::remove(
            &mut self.lock(),
            &keys,
            &excluded,
            &by.into(),
        )?))
    }

    /// The event happening at `at` (Unix milliseconds) that a meeting recorded then belongs to.
    pub fn calendar_event_at(&self, at: i64) -> Result<Option<CalendarEventInfo>> {
        Ok(cal::happening(&self.lock(), at)?.map(CalendarEventInfo::from))
    }

    /// Links a recording to the event it was made at, and the event to `project` when it is
    /// part of none yet.
    pub fn link_to_event(
        &self,
        id: String,
        event: String,
        project: Option<String>,
        by: Provenance,
    ) -> Result<CalendarEventInfo> {
        let id = parse_id(&id)?;
        let event = parse_id(&event)?;
        let project = project.as_deref().map(parse_id).transpose()?;
        Ok(
            cal::link_recording(&mut self.lock(), &id, &event, project.as_ref(), &by.into())?
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorKind, KnodeError};

    fn by(kind: ActorKind, actor: &str) -> Provenance {
        Provenance {
            kind,
            actor: actor.into(),
            session: None,
            source: None,
        }
    }

    fn event(id: &str, start: i64) -> CalendarEventInput {
        CalendarEventInput {
            external_id: id.into(),
            occurrence: None,
            calendar_id: "work".into(),
            calendar: "Work".into(),
            title: "Standup".into(),
            start,
            end: start + 15 * 60 * 1000,
            all_day: false,
            location: None,
            url: Some("https://zoom.us/j/42".into()),
            notes: None,
            attendees: vec![EventAttendee {
                name: "Grace".into(),
                email: Some("grace@example.com".into()),
            }],
            status: EventStatus::Confirmed,
        }
    }

    #[test]
    fn syncs_finds_and_links_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cal.knode").display().to_string();
        let space = KnodeSpace::create(path, "Cal".into()).unwrap();
        let calendar = by(ActorKind::System, "calendar");
        let now = 1_795_510_800_000;
        let sync = |events: Vec<CalendarEventInput>, excluded: Vec<CalendarInfo>| CalendarSync {
            calendars: vec![CalendarInfo {
                id: "work".into(),
                name: "Work".into(),
            }],
            excluded,
            start: now - 86_400_000,
            end: now + 86_400_000,
            events,
            attendee_emails: false,
        };
        let first = space
            .sync_calendar(sync(vec![event("E1", now)], vec![]), calendar.clone())
            .unwrap();
        assert_eq!(first.created, 1);
        let again = space
            .sync_calendar(sync(vec![event("E1", now)], vec![]), calendar.clone())
            .unwrap();
        assert_eq!((again.created, again.unchanged), (0, 1));
        let excluded = space
            .sync_calendar(
                sync(
                    vec![event("E2", now)],
                    vec![CalendarInfo {
                        id: "work".into(),
                        name: "Work".into(),
                    }],
                ),
                calendar.clone(),
            )
            .unwrap();
        assert_eq!(
            (excluded.created, excluded.skipped, excluded.missing.len()),
            (0, 1, 0)
        );

        let found = space.calendar_event_at(now + 60_000).unwrap().unwrap();
        assert_eq!(found.title, "Standup");
        assert_eq!(found.call_link.as_deref(), Some("https://zoom.us/j/42"));
        let body = space.get_node(found.id.clone()).unwrap().unwrap().body;
        assert!(body.contains("Grace") && !body.contains("example.com"));

        let me = by(ActorKind::Person, "Ada");
        let meeting = space
            .create_node("Meeting".into(), String::new(), me.clone())
            .unwrap();
        let linked = space
            .link_to_event(meeting.id.clone(), found.id.clone(), None, me.clone())
            .unwrap();
        assert_eq!(linked.id, found.id);
        assert!(matches!(
            space.link_to_event(found.id.clone(), meeting.id, None, me),
            Err(KnodeError::Invalid { .. })
        ));

        let cancelled = CalendarEventInput {
            status: EventStatus::Cancelled,
            ..event("E1", now)
        };
        space
            .sync_calendar(sync(vec![cancelled], vec![]), calendar)
            .unwrap();
        assert!(space.calendar_event_at(now + 60_000).unwrap().is_none());
    }
}

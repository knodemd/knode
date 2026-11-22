use chrono::FixedOffset;
use knode_format::{ActorKind, NewNode, Provenance, Space, Timestamp, kinds, link_kinds};

use super::*;

const HOUR: i64 = 3600 * 1000;
/// 2026-11-24 10:00 in UTC+1.
const T0: i64 = 1_795_510_800_000;

fn zone() -> FixedOffset {
    FixedOffset::east_opt(3600).unwrap()
}

fn calendar() -> Provenance {
    Provenance::new(ActorKind::System, SOURCE_ID)
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn space() -> (tempfile::TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Cal.knode"), "Cal").unwrap();
    (dir, space)
}

fn work() -> Container {
    Container::new("work", "Work")
}

fn home() -> Container {
    Container::new("home", "Home")
}

fn event(id: &str, start: i64) -> CalendarEvent {
    CalendarEvent {
        external_id: id.into(),
        calendar_id: "work".into(),
        calendar: "Work".into(),
        title: "Weekly sync".into(),
        start,
        end: start + HOUR,
        ..Default::default()
    }
}

fn wide() -> Window {
    Window {
        start: T0 - 30 * DAY_MS,
        end: T0 + 30 * DAY_MS,
    }
}

fn run(space: &mut Space, events: &[CalendarEvent], excluded: &[Container]) -> SyncReport {
    run_all(space, events, excluded).report
}

fn run_all(space: &mut Space, events: &[CalendarEvent], excluded: &[Container]) -> Synced {
    let calendars = [work(), home()];
    let request = SyncRequest {
        calendars: &calendars,
        excluded,
        window: wide(),
        events,
        attendee_emails: false,
    };
    sync_in(space, &request, &calendar(), &zone()).unwrap()
}

fn node(space: &Space, key: &str) -> (knode_format::Node, Properties) {
    let id = find(space, key).unwrap().expect("the event is written");
    (
        space.get_node(&id).unwrap().unwrap(),
        space.properties(&id).unwrap(),
    )
}

#[test]
fn an_event_becomes_one_node_written_by_the_calendar() {
    let (_dir, mut space) = space();
    let mut e = event("E1", T0);
    e.location = Some("Room 3".into());
    e.notes = Some("Agenda: launch.\nJoin: https://meet.google.com/abc-defg-hij".into());
    e.attendees = vec![
        Attendee {
            name: "Grace Hopper".into(),
            email: Some("grace@example.com".into()),
        },
        Attendee {
            name: "bob@example.com".into(),
            email: Some("bob@example.com".into()),
        },
    ];
    let report = run(&mut space, &[e], &[]);
    assert_eq!(report.created, 1);
    let (node, props) = node(&space, "E1");
    assert_eq!(node.kind, EVENT_KIND);
    assert_eq!(node.title, "Weekly sync (2026-11-24)");
    assert_eq!(node.created_by.kind, ActorKind::System);
    assert_eq!(node.created_by.actor, "calendar");
    assert!(
        node.body
            .contains("**When:** Tuesday 24 November 2026, 10:00–11:00"),
        "{}",
        node.body
    );
    assert!(node.body.contains("**Where:** Room 3"));
    assert!(
        node.body
            .contains("**Call:** <https://meet.google.com/abc-defg-hij>")
    );
    // Names only: no address, and an attendee known only by address is left out.
    assert!(
        node.body.contains("**Attendees:** Grace Hopper\n")
            || node.body.contains("**Attendees:** Grace Hopper  \n")
    );
    assert!(!node.body.contains("example.com"));
    assert!(node.body.contains("Agenda: launch."));
    assert_eq!(
        props[keys::CALL_LINK],
        "https://meet.google.com/abc-defg-hij"
    );
    assert_eq!(props[keys::ATTENDEES], r#"["Grace Hopper"]"#);
    assert!(!props.contains_key(keys::ATTENDEE_EMAILS));
    assert_eq!(props[keys::STATUS], "confirmed");
    assert_eq!(props[keys::DAY], "2026-11-24");
    assert_eq!(props[keys::RECURRING], "false");
}

#[test]
fn addresses_only_when_the_space_asks() {
    let (_dir, mut space) = space();
    let mut e = event("E1", T0);
    e.attendees = vec![
        Attendee {
            name: "Grace".into(),
            email: Some("grace@example.com".into()),
        },
        Attendee {
            name: String::new(),
            email: Some("bob@example.com".into()),
        },
    ];
    let calendars = [work()];
    let request = SyncRequest {
        calendars: &calendars,
        excluded: &[],
        window: wide(),
        events: std::slice::from_ref(&e),
        attendee_emails: true,
    };
    sync_in(&mut space, &request, &calendar(), &zone()).unwrap();
    let (node, props) = node(&space, "E1");
    assert!(
        node.body
            .contains("Grace (grace@example.com), bob@example.com")
    );
    assert_eq!(
        props[keys::ATTENDEE_EMAILS],
        r#"["grace@example.com","bob@example.com"]"#
    );
}

#[test]
fn attendees_link_to_people_of_the_space() {
    let (_dir, mut space) = space();
    space
        .create_node(
            NewNode::note("Grace Hopper", "").with_kind(PERSON_KIND),
            &me(),
        )
        .unwrap();
    let mut e = event("E1", T0);
    e.attendees = vec![Attendee {
        name: "grace hopper".into(),
        email: None,
    }];
    run(&mut space, &[e], &[]);
    let (node, _) = node(&space, "E1");
    assert!(node.body.contains("[[Grace Hopper]]"), "{}", node.body);
}

#[test]
fn writing_again_adds_nothing_and_a_change_updates_the_node() {
    let (_dir, mut space) = space();
    let e = event("E1", T0);
    run(&mut space, std::slice::from_ref(&e), &[]);
    let again = run(&mut space, std::slice::from_ref(&e), &[]);
    assert_eq!((again.created, again.updated, again.unchanged), (0, 0, 1));
    let before = node(&space, "E1").0;

    let mut moved = e.clone();
    moved.title = "Weekly sync, moved".into();
    moved.start = T0 + HOUR;
    moved.end = T0 + 2 * HOUR;
    moved.location = Some("Room 4".into());
    let report = run(&mut space, &[moved], &[]);
    assert_eq!((report.created, report.updated), (0, 1));
    let (after, props) = node(&space, "E1");
    assert_eq!(after.id, before.id);
    assert_eq!(after.title, "Weekly sync, moved (2026-11-24)");
    assert!(after.body.contains("11:00–12:00"));
    assert_eq!(props[keys::LOCATION], "Room 4");
    assert_eq!(props[keys::START], (T0 + HOUR).to_string());
    assert_eq!(space.find_by_property(keys::KEY, "E1").unwrap().len(), 1);

    // A place removed from the event is removed from the node.
    let mut placeless = e.clone();
    placeless.location = None;
    run(&mut space, &[placeless], &[]);
    assert!(!node(&space, "E1").1.contains_key(keys::LOCATION));
}

#[test]
fn every_occurrence_of_a_recurring_event_has_its_own_node() {
    let (_dir, mut space) = space();
    let occurrences: Vec<CalendarEvent> = (0..3)
        .map(|week| {
            let start = T0 + week * 7 * DAY_MS;
            CalendarEvent {
                occurrence: Some(start),
                ..event("SERIES", start)
            }
        })
        .collect();
    assert_eq!(run(&mut space, &occurrences, &[]).created, 3);
    let (first, props) = node(&space, &format!("SERIES@{T0}"));
    assert_eq!(first.title, "Weekly sync (2026-11-24)");
    assert_eq!(props[keys::RECURRING], "true");
    assert_eq!(props[keys::EXTERNAL_ID], "SERIES");
    // One occurrence moved keeps its key: when it was planned to start.
    let mut moved = occurrences.clone();
    moved[1].start += 2 * HOUR;
    moved[1].end += 2 * HOUR;
    let report = run(&mut space, &moved, &[]);
    assert_eq!(
        (report.created, report.updated, report.unchanged),
        (0, 1, 2)
    );
    assert_eq!(
        space
            .find_by_property(keys::EXTERNAL_ID, "SERIES")
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn a_cancelled_event_is_marked_and_one_never_written_is_left_out() {
    let (_dir, mut space) = space();
    let e = event("E1", T0);
    run(&mut space, std::slice::from_ref(&e), &[]);
    let cancelled = CalendarEvent {
        status: EventStatus::Cancelled,
        ..e.clone()
    };
    let fresh = CalendarEvent {
        status: EventStatus::Cancelled,
        ..event("E2", T0)
    };
    let report = run(&mut space, &[cancelled, fresh], &[]);
    assert_eq!((report.updated, report.skipped), (1, 1));
    let (node, props) = node(&space, "E1");
    assert!(node.body.starts_with("**Cancelled.**"));
    assert_eq!(props[keys::STATUS], "cancelled");
    assert!(find(&space, "E2").unwrap().is_none());
    // Back on the calendar: no longer marked.
    run(&mut space, &[e], &[]);
    assert!(!node_body(&space, "E1").contains("Cancelled"));
}

fn node_body(space: &Space, key: &str) -> String {
    node(space, key).0.body
}

#[test]
fn an_event_gone_from_its_calendar_is_marked_removed_not_deleted() {
    let (_dir, mut space) = space();
    let keep = event("KEEP", T0);
    let gone = event("GONE", T0 + DAY_MS);
    let mut other = event("OTHER", T0);
    other.calendar_id = "travel".into();
    let far = event("FAR", T0 - 100 * DAY_MS);
    // Written with a window that held `FAR`.
    let calendars = [work(), Container::new("travel", "Travel")];
    let all = [keep.clone(), gone.clone(), other.clone(), far.clone()];
    let request = SyncRequest {
        calendars: &calendars,
        excluded: &[],
        window: Window {
            start: T0 - 200 * DAY_MS,
            end: T0 + 30 * DAY_MS,
        },
        events: &all,
        attendee_emails: false,
    };
    assert_eq!(
        sync_in(&mut space, &request, &calendar(), &zone())
            .unwrap()
            .report
            .created,
        4
    );
    // Now `travel` is not read and `FAR` is outside the window: both stay as they are. `GONE`
    // is only handed back as missing; marked once the app found it gone.
    let synced = run_all(&mut space, std::slice::from_ref(&keep), &[]);
    assert_eq!(
        synced.missing,
        vec![MissingEvent {
            key: "GONE".into(),
            external_id: "GONE".into(),
            occurrence: None,
            calendar_id: "work".into(),
        }]
    );
    assert_eq!(node_status(&space, "GONE"), "confirmed");
    let gone_keys = vec!["GONE".to_string(), "GONE".to_string(), "NONE".to_string()];
    assert_eq!(remove(&mut space, &gone_keys, &[], &calendar()).unwrap(), 1);
    let (node, props) = node(&space, "GONE");
    assert!(node.body.starts_with("**Removed from the calendar.**"));
    assert_eq!(props[keys::STATUS], "removed");
    assert_eq!(node_status(&space, "OTHER"), "confirmed");
    assert_eq!(node_status(&space, "FAR"), "confirmed");
    // Marking again changes nothing, and a removed event is no longer missing; coming back
    // restores it.
    assert_eq!(remove(&mut space, &gone_keys, &[], &calendar()).unwrap(), 0);
    assert!(
        run_all(&mut space, std::slice::from_ref(&keep), &[])
            .missing
            .is_empty()
    );
    run(&mut space, &[keep, gone], &[]);
    assert_eq!(node_status(&space, "GONE"), "confirmed");
    assert!(!node_body(&space, "GONE").contains("Removed"));
}

fn node_status(space: &Space, key: &str) -> String {
    node(space, key).1[keys::STATUS].clone()
}

#[test]
fn an_event_moved_outside_the_window_is_not_marked_removed() {
    let (_dir, mut space) = space();
    let series = event("SERIES", T0 + DAY_MS);
    let mut occurrence = event("SERIES", T0 + 2 * DAY_MS);
    occurrence.occurrence = Some(T0 + 2 * DAY_MS);
    run(&mut space, &[series.clone(), occurrence.clone()], &[]);
    // Both moved 60 days ahead, outside the window read: the app no longer reads them.
    let synced = run_all(&mut space, &[], &[]);
    assert_eq!(synced.report, SyncReport::default());
    assert_eq!(node_status(&space, "SERIES"), "confirmed");
    let key = format!("SERIES@{}", T0 + 2 * DAY_MS);
    assert_eq!(node_status(&space, &key), "confirmed");
    assert!(!node_body(&space, "SERIES").contains("Removed"));
    let mut missing = synced.missing;
    missing.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(missing.len(), 2);
    assert_eq!(
        (missing[0].key.as_str(), missing[0].occurrence),
        ("SERIES", None)
    );
    assert_eq!(missing[1].key, key);
    assert_eq!(missing[1].external_id, "SERIES");
    assert_eq!(missing[1].occurrence, Some(T0 + 2 * DAY_MS));
}

#[test]
fn events_of_the_same_title_and_day_have_their_own_titles() {
    let (_dir, mut space) = space();
    let morning = event("MORNING", T0);
    let evening = event("EVENING", T0 + 6 * HOUR);
    run(&mut space, &[morning.clone(), evening.clone()], &[]);
    let mut titles = vec![
        node(&space, "MORNING").0.title,
        node(&space, "EVENING").0.title,
    ];
    titles.sort();
    assert_eq!(
        titles,
        vec!["Weekly sync (2026-11-24)", "Weekly sync (2026-11-24) 2"]
    );
    // Written again: each keeps its own.
    let report = run(&mut space, &[morning.clone(), evening.clone()], &[]);
    assert_eq!((report.updated, report.unchanged), (0, 2));
    // One renamed to the other's title takes a free one.
    let mut renamed = event("OTHER", T0 + DAY_MS);
    renamed.title = "Review".into();
    run(&mut space, std::slice::from_ref(&renamed), &[]);
    renamed.start = T0;
    renamed.end = T0 + HOUR;
    renamed.title = "Weekly sync".into();
    run(&mut space, &[morning, evening, renamed], &[]);
    assert_eq!(node(&space, "OTHER").0.title, "Weekly sync (2026-11-24) 3");
}

#[test]
fn excluded_calendars_are_never_written_or_changed() {
    let (_dir, mut space) = space();
    let mut private = event("P1", T0);
    private.calendar_id = "home".into();
    run(&mut space, std::slice::from_ref(&private), &[]);
    let synced = run_all(&mut space, &[event("W1", T0)], &[home()]);
    // The home event is not missing, nor marked removed: its calendar is excluded.
    assert!(synced.missing.is_empty());
    let p1 = vec!["P1".to_string()];
    assert_eq!(remove(&mut space, &p1, &[home()], &calendar()).unwrap(), 0);
    assert_eq!(node_status(&space, "P1"), "confirmed");
    let mut changed = private.clone();
    changed.title = "Changed".into();
    let report = run(&mut space, &[changed], &[home()]);
    assert_eq!(report.skipped, 1);
    assert_eq!(node(&space, "P1").0.title, "Weekly sync (2026-11-24)");
    let mut new = event("P2", T0);
    new.calendar_id = "home".into();
    run(&mut space, &[new], &[home()]);
    assert!(find(&space, "P2").unwrap().is_none());
}

#[test]
fn the_window_starts_from_today_or_goes_back() {
    let today = Timestamp(T0 - 10 * HOUR);
    let since = Timestamp(today.0 - 2 * DAY_MS);
    let options = CalendarOptions::default();
    let from_today = window(History::FromToday, since, today, &options);
    assert_eq!(from_today.start, since.0);
    assert_eq!(from_today.end, today.0 + 15 * DAY_MS);
    let everything = window(History::Everything, since, today, &options);
    assert_eq!(everything.start, today.0 - 30 * DAY_MS);
    // Long after connecting, from today keeps only the days back.
    let later = Timestamp(today.0 + 100 * DAY_MS);
    assert_eq!(
        window(History::FromToday, since, later, &options).start,
        later.0 - 30 * DAY_MS
    );

    let (_dir, mut space) = space();
    let old = event("OLD", since.0 - DAY_MS);
    let new = event("NEW", since.0 + HOUR);
    let calendars = [work()];
    let events = [old, new];
    let request = SyncRequest {
        calendars: &calendars,
        excluded: &[],
        window: from_today,
        events: &events,
        attendee_emails: false,
    };
    let report = sync_in(&mut space, &request, &calendar(), &zone())
        .unwrap()
        .report;
    assert_eq!((report.created, report.skipped), (1, 1));
    assert!(find(&space, "OLD").unwrap().is_none());
}

#[test]
fn options_are_kept_in_the_space_by_a_person() {
    let (_dir, mut space) = space();
    assert_eq!(options(&space).unwrap(), CalendarOptions::default());
    let chosen = CalendarOptions {
        days_back: 7,
        days_ahead: 60,
        attendee_emails: true,
    };
    set_options(&mut space, &chosen, &me()).unwrap();
    assert_eq!(options(&space).unwrap(), chosen);
    assert!(set_options(&mut space, &chosen, &calendar()).is_err());
    let too_far = CalendarOptions {
        days_ahead: 400,
        ..chosen
    };
    assert!(set_options(&mut space, &too_far, &me()).is_err());
    set_options(&mut space, &CalendarOptions::default(), &me()).unwrap();
    assert_eq!(space.setting(OPTIONS_SETTING).unwrap(), None);
}

#[test]
fn a_recording_finds_the_event_happening_and_links_to_it() {
    let (_dir, mut space) = space();
    let mut call = event("CALL", T0);
    call.url = Some("https://zoom.us/j/123456".into());
    let mut room = event("ROOM", T0 - 30 * 60 * 1000);
    room.title = "Lunch".into();
    let all_day = CalendarEvent {
        all_day: true,
        start: T0 - 10 * HOUR,
        end: T0 + 14 * HOUR,
        ..event("HOLIDAY", T0)
    };
    run(&mut space, &[call, room, all_day], &[]);

    let at = T0 - 5 * 60 * 1000;
    let found = happening_in(&space, at, &zone()).unwrap().unwrap();
    assert_eq!(found.title, "Weekly sync");
    assert_eq!(found.call_link.as_deref(), Some("https://zoom.us/j/123456"));
    // Too early for any event but the one already going.
    let early = happening_in(&space, T0 - 40 * 60 * 1000, &zone())
        .unwrap()
        .unwrap();
    assert_eq!(early.title, "Lunch");
    assert!(
        happening_in(&space, T0 + 3 * HOUR, &zone())
            .unwrap()
            .is_none()
    );

    let project = space
        .create_node(NewNode::note("Launch", "").with_kind(kinds::PROJECT), &me())
        .unwrap();
    let meeting = space
        .create_node(NewNode::note("Meeting", "").with_kind("meeting"), &me())
        .unwrap();
    let linked =
        link_recording(&mut space, &meeting.id, &found.id, Some(&project.id), &me()).unwrap();
    assert_eq!(linked.project, Some(project.id));
    let links = space.links_from(&meeting.id).unwrap();
    assert!(
        links
            .iter()
            .any(|l| l.to == found.id && l.kind == link_kinds::RELATED)
    );
    // Not an event: refused.
    assert!(link_recording(&mut space, &found.id, &meeting.id, None, &me()).is_err());
}

#[test]
fn call_links_are_found_in_text() {
    assert_eq!(
        call_link_in("Join at https://us02web.zoom.us/j/8123?pwd=x)."),
        Some("https://us02web.zoom.us/j/8123?pwd=x".into())
    );
    assert_eq!(
        call_link_in("<https://teams.microsoft.com/l/meetup-join/19%3a>"),
        Some("https://teams.microsoft.com/l/meetup-join/19%3a".into())
    );
    assert_eq!(
        call_link_in("https://example.com/j/1 and https://zoom.us/"),
        None
    );
    assert_eq!(call_link_in("no link"), None);
}

#[test]
fn long_notes_are_cut() {
    let notes = "word ".repeat(1000);
    let cut = truncate(&notes, MAX_NOTES);
    assert!(cut.chars().count() <= MAX_NOTES + 1);
    assert!(cut.ends_with("word…"));
    assert_eq!(truncate("short", 10), "short");
}

#[test]
fn all_day_and_multi_day_times_read_well() {
    let day_start = T0 - 10 * HOUR; // midnight in UTC+1
    let z = zone();
    assert_eq!(
        when(day_start, day_start + DAY_MS, true, &z),
        "Tuesday 24 November 2026, all day"
    );
    assert_eq!(
        when(day_start, day_start + 3 * DAY_MS, true, &z),
        "Tuesday 24 November 2026 to Thursday 26 November 2026, all day"
    );
    assert_eq!(
        when(T0, T0 + DAY_MS, false, &z),
        "Tuesday 24 November 2026, 10:00 to Wednesday 25 November 2026, 10:00"
    );
}

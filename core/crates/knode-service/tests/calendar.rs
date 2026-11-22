//! The calendar source in the service: run by the app, which hands the events it read to
//! `calendar.sync`. Synthetic events only; nothing here reads a real calendar.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use knode_format::{ActorKind, NewNode, Provenance, Space, kinds};
use knode_service::sources::calendar::CalendarConnector;
use knode_service::sources::{SourceState, SourcesOptions, start_of_today};
use knode_service::{Client, Config, ServiceHandle, start};
use serde_json::{Value, json};
use tempfile::TempDir;

const HOUR: i64 = 3600 * 1000;
const DAY: i64 = 24 * HOUR;

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        drop(Space::create(dir.path().join("Personal.knode"), "Personal").unwrap());
        let mut f = Fixture { dir, service: None };
        f.write_config("");
        let options = SourcesOptions {
            settings_poll: Duration::from_millis(50),
            ..SourcesOptions::with_connectors(vec![Arc::new(CalendarConnector)])
        };
        let config = Config::new(f.home()).with_sources(options);
        f.service = Some(start(config).unwrap());
        f
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn space(&self) -> PathBuf {
        fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap()
    }

    fn write_config(&self, extra: &str) {
        fs::create_dir_all(self.home()).unwrap();
        let space = fs::canonicalize(self.dir.path().join("Personal.knode")).unwrap();
        let text = format!("default_space = {:?}\n{extra}", space.to_str().unwrap());
        fs::write(self.home().join("config.toml"), text).unwrap();
    }

    fn call(&self, method: &str, mut params: Value) -> Value {
        params["space"] = json!(self.space());
        Client::connect(self.home().join("service.sock"))
            .unwrap()
            .call(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn try_call(&self, method: &str, mut params: Value) -> Result<Value, String> {
        params["space"] = json!(self.space());
        Client::connect(self.home().join("service.sock"))
            .unwrap()
            .call(method, params)
            .map_err(|e| e.to_string())
    }

    fn status(&self) -> Value {
        self.call("sources.list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "calendar")
            .unwrap()
            .clone()
    }

    fn connect(&self) {
        self.call(
            "sources.connect",
            json!({ "source": "calendar", "on": false, "by": me() }),
        );
        self.call(
            "sources.exclude",
            json!({ "source": "calendar", "id": "home", "name": "Home", "by": me() }),
        );
        self.call(
            "sources.resume",
            json!({ "source": "calendar", "by": me() }),
        );
    }

    fn events(&self) -> Vec<Value> {
        let space = Space::open(self.space()).unwrap();
        let mut out = Vec::new();
        for s in space
            .find_by_property("event.calendar_id", "work")
            .unwrap()
            .into_iter()
            .chain(space.find_by_property("event.calendar_id", "home").unwrap())
        {
            let node = space.get_node(&s.id).unwrap().unwrap();
            let props = space.properties(&s.id).unwrap();
            out.push(json!({
                "title": node.title,
                "kind": node.kind,
                "actor": node.updated_by.actor,
                "actor_kind": format!("{:?}", node.updated_by.kind),
                "key": props["event.key"],
                "status": props["event.status"],
            }));
        }
        out
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            let _ = service.stop();
        }
    }
}

fn me() -> Provenance {
    Provenance::person("Example")
}

fn calendars() -> Value {
    json!([{ "id": "work", "name": "Work" }, { "id": "home", "name": "Home" }])
}

fn event(id: &str, calendar: &str, start: i64) -> Value {
    json!({
        "external_id": id, "calendar_id": calendar, "calendar": calendar,
        "title": format!("Event {id}"), "start": start, "end": start + HOUR,
    })
}

fn sync(f: &Fixture, events: Value) -> Value {
    f.call(
        "calendar.sync",
        json!({ "calendars": calendars(), "events": events }),
    )
}

#[test]
fn the_calendar_is_listed_as_a_connector_the_app_runs() {
    let f = Fixture::new();
    let status = f.status();
    assert_eq!(status["kind"], "connector");
    assert_eq!(status["state"], "off");
    assert_eq!(status["permissions"], json!(["calendar"]));
    assert_eq!(status["container"], "calendar");
    // Not connected: no plan, and nothing is written.
    assert!(f.try_call("calendar.plan", json!({})).is_err());
    assert!(
        f.try_call("calendar.sync", json!({ "events": [] }))
            .is_err()
    );
}

#[test]
fn the_calendars_the_app_reports_can_be_excluded() {
    let f = Fixture::new();
    f.call("calendar.calendars", json!({ "calendars": calendars() }));
    f.call(
        "sources.connect",
        json!({ "source": "calendar", "on": false, "by": me() }),
    );
    let listed = f.call("sources.containers", json!({ "source": "calendar" }));
    assert_eq!(
        listed,
        json!([
            { "id": "work", "name": "Work", "excluded": false },
            { "id": "home", "name": "Home", "excluded": false },
        ])
    );
    f.call(
        "sources.exclude",
        json!({ "source": "calendar", "id": "home", "name": "Home", "by": me() }),
    );
    let plan = f.call("calendar.plan", json!({}));
    assert_eq!(plan["on"], false);
    assert_eq!(plan["exclude"], json!(["home"]));
    // A sync names only the calendars it read: the list of the Mac's calendars stays whole.
    f.call(
        "calendar.sync",
        json!({ "calendars": [{ "id": "work", "name": "Work" }], "events": [] }),
    );
    let listed = f.call("sources.containers", json!({ "source": "calendar" }));
    assert_eq!(listed.as_array().unwrap().len(), 2);
}

#[test]
fn sync_writes_events_once_from_today_and_never_excluded_ones() {
    let f = Fixture::new();
    f.connect();
    let today = start_of_today().0;
    let plan = f.call("calendar.plan", json!({}));
    assert_eq!(plan["on"], true);
    // From today: the window starts the day it was connected.
    assert_eq!(plan["start"], today);
    assert_eq!(plan["end"], today + 15 * DAY);
    assert_eq!(plan["attendee_emails"], false);

    let series: Vec<Value> = (0..2)
        .map(|w| {
            let start = today + 9 * HOUR + w * 7 * DAY;
            let mut e = event("SERIES", "work", start);
            e["occurrence"] = json!(start);
            e
        })
        .collect();
    let mut events = vec![
        event("W1", "work", today + 10 * HOUR),
        event("H1", "home", today + 10 * HOUR),
        event("OLD", "work", today - 3 * DAY),
    ];
    events.extend(series);
    let result = sync(&f, json!(events));
    assert_eq!(result["written"]["created"], 3);
    assert_eq!(result["written"]["skipped"], 2);
    assert_eq!(result["source"]["state"], "idle");
    assert_eq!(result["source"]["items"], 3);
    let written = f.events();
    assert_eq!(written.len(), 3);
    assert!(written.iter().all(|e| e["kind"] == "event"
        && e["actor"] == "calendar"
        && e["actor_kind"] == format!("{:?}", ActorKind::System)));
    let keys: Vec<&str> = written.iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&format!("SERIES@{}", today + 9 * HOUR).as_str()));

    // The same events again: nothing changes.
    let again = sync(&f, json!(events));
    assert_eq!(again["written"]["created"], 0);
    assert_eq!(again["written"]["updated"], 0);
    assert_eq!(again["source"]["last_items"], 0);

    // W1 cancelled, the second occurrence gone from the calendar.
    let mut cancelled = event("W1", "work", today + 10 * HOUR);
    cancelled["status"] = json!("cancelled");
    let first = events[3].clone();
    let result = sync(&f, json!([cancelled, first]));
    assert_eq!(result["written"]["updated"], 1);
    let second = format!("SERIES@{}", today + 9 * HOUR + 7 * DAY);
    // Only handed back as missing: the app asks its calendar, then marks those gone.
    assert_eq!(
        result["missing"],
        json!([{
            "key": second, "external_id": "SERIES",
            "occurrence": today + 9 * HOUR + 7 * DAY, "calendar_id": "work",
        }])
    );
    assert!(f.events().iter().all(|e| e["status"] != "removed"));
    let removed = f.call("calendar.remove", json!({ "keys": [second] }));
    assert_eq!(removed["removed"], 1);
    assert_eq!(removed["source"]["last_items"], 2);
    let statuses: Vec<(String, String)> = f
        .events()
        .iter()
        .map(|e| {
            (
                e["key"].as_str().unwrap().into(),
                e["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(statuses.contains(&("W1".into(), "cancelled".into())));
    assert!(statuses.contains(&(second.clone(), "removed".into())));
    assert_eq!(f.events().len(), 3);
}

#[test]
fn a_paused_calendar_or_sources_off_write_nothing() {
    let f = Fixture::new();
    f.connect();
    f.call("sources.pause", json!({ "source": "calendar", "by": me() }));
    let today = start_of_today().0;
    let result = sync(&f, json!([event("W1", "work", today + HOUR)]));
    assert_eq!(result["written"]["created"], 0);
    assert_eq!(result["source"]["state"], "paused");
    f.call(
        "sources.resume",
        json!({ "source": "calendar", "by": me() }),
    );
    f.write_config("sources = false\n");
    assert_eq!(f.call("calendar.plan", json!({}))["on"], false);
    let result = sync(&f, json!([event("W1", "work", today + HOUR)]));
    assert_eq!(result["written"]["created"], 0);
    assert!(f.events().is_empty());
}

#[test]
fn run_now_and_options_ask_the_app_to_run_it() {
    let f = Fixture::new();
    f.connect();
    let today = start_of_today().0;
    sync(&f, json!([]));
    let next = f.call("calendar.plan", json!({}))["next_run"]
        .as_i64()
        .unwrap();
    assert!(next > knode_format::Timestamp::now().0 + 10 * 60 * 1000);
    f.call("sources.run", json!({ "source": "calendar" }));
    let next = f.call("calendar.plan", json!({}))["next_run"]
        .as_i64()
        .unwrap();
    assert!(next <= knode_format::Timestamp::now().0);
    // The service never runs it itself: nothing failed, nothing ran.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(f.status()["state"], "idle");

    let options = f.call(
        "calendar.configure",
        json!({ "days_ahead": 30, "attendee_emails": true, "by": me() }),
    );
    assert_eq!(
        options,
        json!({ "days_back": 30, "days_ahead": 30, "attendee_emails": true })
    );
    assert_eq!(f.call("calendar.options", json!({})), options);
    let plan = f.call("calendar.plan", json!({}));
    assert_eq!(plan["end"], today + 31 * DAY);
    assert_eq!(plan["attendee_emails"], true);
    assert!(
        f.try_call(
            "calendar.configure",
            json!({ "days_ahead": 1000, "by": me() })
        )
        .is_err()
    );
}

#[test]
fn a_denied_permission_needs_attention() {
    let f = Fixture::new();
    f.connect();
    let status = f.call(
        "calendar.fail",
        json!({ "kind": "permission", "message": "Calendar access was not allowed" }),
    );
    assert_eq!(
        status["state"],
        SourceStateName::of(SourceState::NeedsAttention)
    );
    assert_eq!(status["last_error"]["kind"], "permission");
    assert!(
        f.try_call("calendar.fail", json!({ "kind": "nope" }))
            .is_err()
    );
    // A good run clears it.
    let status = sync(&f, json!([]))["source"].clone();
    assert_eq!(status["state"], "idle");
}

struct SourceStateName;

impl SourceStateName {
    fn of(state: SourceState) -> Value {
        serde_json::to_value(state).unwrap()
    }
}

#[test]
fn a_recording_links_to_the_event_happening() {
    let f = Fixture::new();
    f.connect();
    let now = knode_format::Timestamp::now().0;
    let mut e = event("CALL", "work", now - 5 * 60 * 1000);
    e["url"] = json!("https://meet.google.com/abc-defg-hij");
    sync(&f, json!([e]));
    let current = f.call("calendar.current", json!({ "at": now }));
    assert_eq!(current["title"], "Event CALL");
    assert_eq!(current["call_link"], "https://meet.google.com/abc-defg-hij");
    assert_eq!(
        f.call("calendar.current", json!({ "at": now + 3 * HOUR })),
        Value::Null
    );

    let (meeting, project) = {
        let mut space = Space::open(f.space()).unwrap();
        let project = space
            .create_node(NewNode::note("Launch", "").with_kind(kinds::PROJECT), &me())
            .unwrap();
        let meeting = space
            .create_node(NewNode::note("Meeting", "").with_kind("meeting"), &me())
            .unwrap();
        (meeting.id, project.id)
    };
    let linked = f.call(
        "calendar.link",
        json!({ "id": meeting, "event": current["id"], "project": project, "by": me() }),
    );
    assert_eq!(linked["project"], json!(project));
    let calendar = Provenance::new(ActorKind::System, "calendar");
    assert!(
        f.try_call(
            "calendar.link",
            json!({ "id": meeting, "event": current["id"], "by": calendar }),
        )
        .is_err()
    );
}

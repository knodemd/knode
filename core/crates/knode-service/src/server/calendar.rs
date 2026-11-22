//! `calendar.*`: the calendar source, run by the Knode app ([`crate::sources::calendar`]).
//! Blocking: called on a blocking thread, like [`super::call`]. Only a person calls them: the
//! app, as the person who owns the Knode home.

use std::path::{Path, PathBuf};
use std::time::Duration;

use knode_format::{NodeId, Provenance, Space, Timestamp};
use knode_ingest::calendar::{
    self, CalendarEvent, CalendarOptions, EventInfo, SOURCE_ID, SyncReport, SyncRequest, Window,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::protocol::{ChangeKind, RpcError, SpaceParams, codes, methods};
use crate::sources::{
    self, Container, Cursor, LastError, SourceSetting, SourceStatus, start_of_today, state_key,
};
use crate::spaces::SpaceEventKind;

/// How long after a good run the app is asked to run the calendar again, at the latest. The
/// app also runs it when a calendar changes.
pub(crate) const SYNC_EVERY: Duration = Duration::from_secs(15 * 60);

/// `calendar.plan`: what a space wants from the calendar now.
#[derive(Debug, Clone, Serialize)]
struct Plan {
    /// Whether the source is on in the space: when off, the app reads nothing for it.
    on: bool,
    /// The days to read.
    start: i64,
    end: i64,
    /// The ids of the calendars excluded: never read.
    exclude: Vec<String>,
    /// Whether attendees' addresses are written.
    attendee_emails: bool,
    /// When the service asks the app to run it next, at the latest. `None` before its first run.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run: Option<Timestamp>,
}

#[derive(Debug, Deserialize)]
struct CalendarsParams {
    calendars: Vec<Container>,
}

#[derive(Debug, Deserialize)]
struct SyncParams {
    space: PathBuf,
    /// Every calendar the app read.
    #[serde(default)]
    calendars: Vec<Container>,
    /// The days the app read; within the plan's.
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    end: Option<i64>,
    #[serde(default)]
    events: Vec<CalendarEvent>,
}

#[derive(Debug, Serialize)]
struct SyncResult {
    /// What was written; nothing when the source is paused.
    written: SyncReport,
    /// The source, as `sources.list` shows it.
    source: SourceStatus,
}

#[derive(Debug, Deserialize)]
struct FailParams {
    space: PathBuf,
    /// `permission`, `unavailable` or `failed`.
    kind: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct CurrentParams {
    space: PathBuf,
    /// When; now when left out.
    #[serde(default)]
    at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LinkParams {
    space: PathBuf,
    /// The recording, or any node made at the event.
    id: NodeId,
    event: NodeId,
    #[serde(default)]
    project: Option<NodeId>,
    by: Provenance,
}

#[derive(Debug, Deserialize)]
struct ConfigureParams {
    space: PathBuf,
    #[serde(default)]
    days_back: Option<u32>,
    #[serde(default)]
    days_ahead: Option<u32>,
    #[serde(default)]
    attendee_emails: Option<bool>,
    by: Provenance,
}

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::CALENDAR_PLAN => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(plan(state, &space)?)
        }
        methods::CALENDAR_CALENDARS => {
            let p: CalendarsParams = params(method, raw)?;
            state.registry().report_containers(SOURCE_ID, p.calendars);
            to_value(true)
        }
        methods::CALENDAR_SYNC => sync(state, params(method, raw)?),
        methods::CALENDAR_FAIL => fail(state, params(method, raw)?),
        methods::CALENDAR_CURRENT => {
            let p: CurrentParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            let at = p.at.unwrap_or_else(|| Timestamp::now().0);
            to_value(calendar::happening(&space, at)?)
        }
        methods::CALENDAR_LINK => {
            let p: LinkParams = params(method, raw)?;
            person(&p.by)?;
            write(state, &p.space, |space| {
                calendar::link_recording(space, &p.id, &p.event, p.project.as_ref(), &p.by)
            })
        }
        methods::CALENDAR_OPTIONS => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(calendar::options(&space)?)
        }
        methods::CALENDAR_CONFIGURE => {
            let p: ConfigureParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let current = calendar::options(&space)?;
            let chosen = CalendarOptions {
                days_back: p.days_back.unwrap_or(current.days_back),
                days_ahead: p.days_ahead.unwrap_or(current.days_ahead),
                attendee_emails: p.attendee_emails.unwrap_or(current.attendee_emails),
            };
            let options = calendar::set_options(&mut space, &chosen, &p.by)?;
            if options != current {
                state.notify_space(&space, SpaceEventKind::Settings, |e| {
                    e.key = Some(calendar::OPTIONS_SETTING.to_string());
                });
                // The days changed: the app reads them again at once.
                ask_to_run(state, &space)?;
            }
            to_value(options)
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

fn person(by: &Provenance) -> Result<(), RpcError> {
    if by.kind == knode_format::ActorKind::Person {
        Ok(())
    } else {
        Err(RpcError::from(knode_format::Error::Invalid(
            "only a person links a recording to its event".into(),
        )))
    }
}

/// The settings of the calendar source in the space, or `invalid` when it is not connected.
fn setting(space: &Space) -> Result<SourceSetting, RpcError> {
    sources::source_setting(space, SOURCE_ID)?.ok_or_else(|| {
        RpcError::from(knode_format::Error::Invalid(format!(
            "{SOURCE_ID} is not connected to this space: connect it in Settings > Sources"
        )))
    })
}

/// Whether `config.toml` lets connectors run (`sources` and `capture`).
fn sources_on(state: &State) -> bool {
    let home = state.registry().home();
    knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
        .map(|c| c.sources_enabled())
        .unwrap_or(false)
}

fn plan(state: &State, space: &Space) -> Result<Plan, RpcError> {
    let setting = setting(space)?;
    let options = calendar::options(space)?;
    let window = calendar::window(setting.history, setting.since, start_of_today(), &options);
    let key = state_key(&space.manifest().id, SOURCE_ID);
    let next_run = state.registry().states().get(&key).and_then(|s| s.next_run);
    Ok(Plan {
        on: setting.on && sources_on(state),
        start: window.start,
        end: window.end,
        exclude: setting.exclude.iter().map(|c| c.id.clone()).collect(),
        attendee_emails: options.attendee_emails,
        next_run,
    })
}

fn sync(state: &State, p: SyncParams) -> Result<Value, RpcError> {
    let registry = state.registry();
    if !p.calendars.is_empty() {
        registry.report_containers(SOURCE_ID, p.calendars.clone());
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let setting = setting(&space)?;
    let options = calendar::options(&space)?;
    let planned = calendar::window(setting.history, setting.since, start_of_today(), &options);
    let window = Window {
        start: p.start.map_or(planned.start, |s| s.max(planned.start)),
        end: p.end.map_or(planned.end, |e| e.min(planned.end)),
    };
    let mut written = SyncReport::default();
    if setting.on && sources_on(state) {
        let by = sources::provenance(SOURCE_ID);
        let mark = last_change(&space)?;
        let request = SyncRequest {
            calendars: &p.calendars,
            excluded: &setting.exclude,
            window,
            events: &p.events,
            attendee_emails: options.attendee_emails,
        };
        let result = calendar::sync(&mut space, &request, &by);
        state.notify_since(&space, mark)?;
        written = match result {
            Ok(written) => written,
            Err(e) => {
                record(state, &space, Err(("space", e.to_string())))?;
                return Err(e.into());
            }
        };
        let cursor = serde_json::json!({ "start": window.start, "end": window.end });
        record(
            state,
            &space,
            Ok((written.items(), Cursor::new(cursor.to_string()))),
        )?;
    }
    to_value(SyncResult {
        written,
        source: registry.get(&space, SOURCE_ID)?,
    })
}

fn fail(state: &State, p: FailParams) -> Result<Value, RpcError> {
    let kind = match p.kind.as_str() {
        kind @ ("permission" | "unavailable" | "failed") => kind,
        other => {
            return Err(RpcError::invalid_params(format!(
                "kind is permission, unavailable or failed, not {other:?}"
            )));
        }
    };
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    setting(&space)?;
    let message = match p.message.trim() {
        "" => "the calendars could not be read".to_string(),
        message => message.to_string(),
    };
    record(state, &space, Err((kind, message)))?;
    to_value(state.registry().get(&space, SOURCE_ID)?)
}

/// Records a run of the calendar in the space on this machine: what it wrote and where it
/// stopped, or why it failed. Either way the app is asked to run it again within
/// [`SYNC_EVERY`].
fn record(
    state: &State,
    space: &Space,
    outcome: Result<(usize, Cursor), (&str, String)>,
) -> Result<(), RpcError> {
    let registry = state.registry();
    let key = state_key(&space.manifest().id, SOURCE_ID);
    let now = Timestamp::now();
    let mut states = registry.states();
    let run = states.entry(&key);
    run.last_run = Some(now);
    run.next_run = Some(Timestamp(now.0 + SYNC_EVERY.as_millis() as i64));
    match outcome {
        Ok((items, cursor)) => {
            run.last_success = Some(now);
            run.last_items = items;
            run.items += items as u64;
            run.last_error = None;
            run.failures = 0;
            run.cursor = Some(cursor);
        }
        Err((kind, message)) => {
            run.failures += 1;
            run.last_error = Some(LastError {
                kind: kind.to_string(),
                message,
                at: now,
            });
        }
    }
    states.save(registry.home()).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("the state of {SOURCE_ID} could not be saved: {e}"),
        )
    })
}

/// Asks the app to run the calendar for the space now, when it is connected.
fn ask_to_run(state: &State, space: &Space) -> Result<(), RpcError> {
    if sources::source_setting(space, SOURCE_ID)?.is_none() {
        return Ok(());
    }
    let registry = state.registry();
    let mut states = registry.states();
    states
        .entry(&state_key(&space.manifest().id, SOURCE_ID))
        .next_run = Some(Timestamp::now());
    states.save(registry.home()).map_err(|e| {
        RpcError::new(
            codes::STORAGE,
            format!("the state of {SOURCE_ID} could not be saved: {e}"),
        )
    })
}

/// Runs a write and announces what it changed; a write that changed nothing in the history
/// (a link already there) announces the event.
fn write(
    state: &State,
    space: &Path,
    run: impl FnOnce(&mut Space) -> knode_format::Result<EventInfo>,
) -> Result<Value, RpcError> {
    let space = state.space(space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = run(&mut space);
    state.notify_since(&space, mark)?;
    let event = result?;
    if last_change(&space)? == mark {
        state.notify(&space, event.id, ChangeKind::Update);
    }
    to_value(event)
}

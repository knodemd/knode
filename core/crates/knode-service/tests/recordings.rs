//! Recordings over the protocol: a real service, the person and agents as clients.

use std::path::PathBuf;
use std::time::Duration;

use knode_format::{ActorKind, GrantLevel, NewNode, Provenance, kinds};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::recordings::{
    ActionItem, AudioInput, Failure, NewRecording, Recording, RecordingKind, Segment, Stage,
    Summary, SummaryState, Track, Transcript, TranscriptState,
};
use knode_service::{ChangeKind, Client, Config, Error, Event, ServiceHandle, Subscription, start};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn engine() -> Provenance {
    Provenance::new(ActorKind::System, "apple-speech")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/service.sock")
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }

    fn agent(&self, name: &str, level: GrantLevel) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, "claude-code", &fingerprint(&key), &me())
            .unwrap();
        self.person
            .set_grant(name, self.space(), level, &me())
            .unwrap();
        Client::connect_as_agent(self.socket(), &key).unwrap()
    }

    fn audio(&self, name: &str) -> PathBuf {
        let path = std::fs::canonicalize(self.dir.path()).unwrap().join(name);
        std::fs::write(&path, name.as_bytes()).unwrap();
        path
    }

    fn meeting(&self, project: Option<knode_format::NodeId>) -> NewRecording {
        NewRecording {
            kind: RecordingKind::Meeting,
            title: "Weekly sync".into(),
            started_at: 1_795_343_400_000,
            ended_at: 1_795_343_400_000 + 30 * 60_000,
            project,
            audio: vec![
                AudioInput {
                    path: self.audio("microphone.m4a"),
                    track: Track::Microphone,
                },
                AudioInput {
                    path: self.audio("system.m4a"),
                    track: Track::System,
                },
            ],
            key: None,
            app: Some("zoom.us".into()),
            summaries: true,
            original: None,
        }
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn next_change(events: &Subscription) -> knode_service::Change {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Changed(change)) => return change,
            Some(_) => continue,
            None => panic!("no changed notification"),
        }
    }
}

fn transcript() -> Transcript {
    let segment = |start_ms: i64, speaker: &str, text: &str| Segment {
        start_ms,
        end_ms: start_ms + 1000,
        speaker: speaker.into(),
        text: text.into(),
    };
    Transcript {
        language: Some("en-GB".into()),
        engine: "Apple SpeechAnalyzer".into(),
        segments: vec![
            segment(0, "Me", "Shall we start?"),
            segment(1_500, "Others", "Yes."),
            segment(2_500, "Others", "Go ahead."),
        ],
    }
}

#[test]
fn a_meeting_goes_from_recorded_to_summarised() {
    let f = Fixture::new();
    let space = f.space();
    let project = f
        .person
        .create_node(
            &space,
            NewNode::note("Launch", "").with_kind(kinds::PROJECT),
            &me(),
        )
        .unwrap();
    let other = Client::connect(f.socket()).unwrap();
    let events = other.subscribe(Some(&space)).unwrap();

    let rec = f
        .person
        .create_recording(&space, &f.meeting(Some(project.id)), &me())
        .unwrap();
    assert_eq!(rec.project, Some(project.id));
    assert_eq!(rec.tracks.len(), 2);
    assert_eq!(rec.summary, SummaryState::Pending);
    let change = next_change(&events);
    assert_eq!((change.node_id, change.op), (rec.id, ChangeKind::Create));
    let body = f.person.get_node(&space, &rec.id).unwrap().unwrap().body;
    assert!(body.contains("![[microphone.m4a]]\n\n![[system.m4a]]"));
    assert_eq!(f.person.attachments(&space, &rec.id).unwrap().len(), 2);

    let pending = f.person.pending_recordings(&space).unwrap();
    assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), [rec.id]);

    let done = f
        .person
        .set_recording_transcript(&space, &rec.id, &transcript(), &engine())
        .unwrap();
    assert_eq!(done.transcript, TranscriptState::Done);
    assert_eq!(done.language.as_deref(), Some("en-GB"));
    let tid = done.transcript_id.unwrap();
    let node = f.person.get_node(&space, &tid).unwrap().unwrap();
    assert_eq!(node.created_by.actor, "apple-speech");
    assert!(node.body.contains("**Others** (00:00:01): Yes. Go ahead."));
    // Someone else hears of the transcript and of the recording.
    let mut seen = Vec::new();
    while !(seen.contains(&tid) && seen.contains(&rec.id)) {
        seen.push(next_change(&events).node_id);
    }

    let summary = Summary {
        model: "Apple Foundation Models".into(),
        overview: "A short sync.".into(),
        action_items: vec![ActionItem {
            task: "Send notes".into(),
            owner: Some("Me".into()),
        }],
        ..Default::default()
    };
    let summarised = f
        .person
        .set_recording_summary(
            &space,
            &rec.id,
            &summary,
            &Provenance::new(ActorKind::System, "apple-foundation-models"),
        )
        .unwrap();
    assert_eq!(summarised.summary, SummaryState::Done);
    let body = f.person.get_node(&space, &rec.id).unwrap().unwrap().body;
    assert!(body.contains("## Action items\n\n- [ ] Send notes — Me"));
    let transcript_title = f.person.get_node(&space, &tid).unwrap().unwrap().title;
    assert!(transcript_title.starts_with("Transcript: Weekly sync ("));
    assert!(body.contains(&format!("[[{transcript_title}]]")));
    assert!(body.contains("<!-- knode:minutes -->\n## Summary"));
    assert!(f.person.pending_recordings(&space).unwrap().is_empty());
    assert_eq!(f.person.recording(&space, &rec.id).unwrap(), summarised);
}

#[test]
fn the_protocol_takes_and_gives_plain_json() {
    let f = Fixture::new();
    let space = f.space();
    let memo = f.audio("memo.m4a");
    let params = json!({
        "space": space,
        "kind": "voice-memo",
        "title": "Idea",
        "started_at": 1_795_343_400_000_i64,
        "ended_at": 1_795_343_460_000_i64,
        "audio": [{ "path": memo, "track": "mixed" }],
        "key": "vm-1",
        "summaries": false,
        "by": { "kind": "person", "actor": "Ada" },
    });
    let value = f.person.call("recordings.create", params.clone()).unwrap();
    assert_eq!(value["kind"], "voice-memo");
    assert_eq!(value["source"], "voice-memos");
    assert_eq!(value["transcript"], "pending");
    assert_eq!(value["summary"], "off");
    assert_eq!(value["duration_ms"], 60_000);
    assert_eq!(
        value["tracks"],
        json!([{ "name": "memo.m4a", "track": "mixed" }])
    );
    assert_eq!(value["transcript_id"], json!(null));
    let first: Recording = serde_json::from_value(value).unwrap();

    // The same key again: the recording stored before.
    let again = f.person.call("recordings.create", params).unwrap();
    assert_eq!(serde_json::from_value::<Recording>(again).unwrap(), first);

    // Found by its key before anything is made again; an unknown key finds nothing.
    let found = f.person.find_recording(&space, "vm-1").unwrap();
    assert_eq!(found, Some(first.clone()));
    let none = f
        .person
        .call("recordings.find", json!({ "space": space, "key": "vm-2" }))
        .unwrap();
    assert_eq!(none, json!(null));

    let failed = f
        .person
        .call(
            "recordings.fail",
            json!({ "space": space, "id": first.id, "stage": "transcript", "state": "failed",
                    "error": "no speech", "by": { "kind": "system", "actor": "apple-speech" } }),
        )
        .unwrap();
    assert_eq!(failed["transcript"], "failed");
    assert_eq!(failed["error"], "no speech");
    let retried = f
        .person
        .retry_recording(&space, &first.id, Stage::Transcript, &me())
        .unwrap();
    assert_eq!(retried.transcript, TranscriptState::Pending);
    assert_eq!(retried.error, None);

    let refused = rpc(f.person.call(
        "recordings.create",
        json!({ "space": space, "kind": "recording", "title": "x", "started_at": 0,
                "ended_at": 0, "audio": [{ "path": "relative.m4a", "track": "mixed" }],
                "by": { "kind": "person", "actor": "Ada" } }),
    ));
    assert_eq!(refused.code, codes::INVALID_PARAMS);

    // Without `summaries`, the recording is summarised.
    let other = f.audio("other.m4a");
    let value = f
        .person
        .call(
            "recordings.create",
            json!({ "space": space, "kind": "recording", "title": "Talk", "started_at": 0,
                    "ended_at": 1000, "audio": [{ "path": other, "track": "mixed" }],
                    "by": { "kind": "person", "actor": "Ada" } }),
        )
        .unwrap();
    assert_eq!(value["summary"], "pending");
}

#[test]
fn failed_recordings_are_listed_for_retry() {
    let f = Fixture::new();
    let space = f.space();
    let rec = f
        .person
        .create_recording(&space, &f.meeting(None), &me())
        .unwrap();
    assert!(f.person.failed_recordings(&space).unwrap().is_empty());
    f.person
        .fail_recording(
            &space,
            &rec.id,
            Stage::Transcript,
            Failure::Failed,
            "no speech",
            &me(),
        )
        .unwrap();
    let failed = f.person.failed_recordings(&space).unwrap();
    assert_eq!(failed.iter().map(|r| r.id).collect::<Vec<_>>(), [rec.id]);
    assert_eq!(failed[0].error.as_deref(), Some("no speech"));
    let value = f
        .person
        .call("recordings.failed", json!({ "space": space }))
        .unwrap();
    assert_eq!(value[0]["transcript"], "failed");
    let reader = f.agent("reader", GrantLevel::Read);
    assert_eq!(reader.failed_recordings(&space).unwrap().len(), 1);
    f.person
        .retry_recording(&space, &rec.id, Stage::Transcript, &me())
        .unwrap();
    assert!(f.person.failed_recordings(&space).unwrap().is_empty());
}

#[test]
fn recordings_summaries_is_a_setting() {
    let f = Fixture::new();
    let space = f.space();
    let settings = f
        .person
        .set_setting(&space, "recordings.summaries", Some("on"), &me())
        .unwrap();
    assert_eq!(settings["recordings.summaries"], "on");
    assert!(
        f.person
            .set_setting(&space, "recordings.summaries", Some("maybe"), &me())
            .is_err()
    );
}

#[test]
fn agents_read_recordings_but_never_write_them() {
    let f = Fixture::new();
    let space = f.space();
    let rec = f
        .person
        .create_recording(&space, &f.meeting(None), &me())
        .unwrap();

    let writer = f.agent("writer", GrantLevel::Write);
    assert_eq!(writer.recording(&space, &rec.id).unwrap().id, rec.id);
    assert_eq!(writer.pending_recordings(&space).unwrap().len(), 1);
    assert_eq!(
        rpc(writer.create_recording(&space, &f.meeting(None), &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.set_recording_transcript(&space, &rec.id, &transcript(), &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.set_recording_summary(&space, &rec.id, &Summary::default(), &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.fail_recording(&space, &rec.id, Stage::Summary, Failure::Failed, "x", &me()))
            .code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.retry_recording(&space, &rec.id, Stage::Transcript, &me())).code,
        codes::FORBIDDEN
    );

    let none = f.agent("none", GrantLevel::None);
    assert!(none.recording(&space, &rec.id).is_err());
}

use std::path::Path;

use chrono::Utc;
use knode_format::{ActorKind, NewNode, Provenance, Space, kinds, link_kinds};

use super::*;

fn me() -> Provenance {
    Provenance::person("Ada")
}

fn engine() -> Provenance {
    Provenance::new(ActorKind::System, "apple-speech")
}

fn space(dir: &Path) -> Space {
    Space::create(dir.join("Rec.knode"), "Rec").unwrap()
}

fn audio(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// 2026-11-22 10:30:00 UTC.
const START: i64 = 1_795_343_400_000;

fn meeting(dir: &Path, project: Option<NodeId>) -> NewRecording {
    NewRecording {
        kind: RecordingKind::Meeting,
        title: "Weekly sync".into(),
        started_at: START,
        ended_at: START + 45 * 60_000,
        project,
        audio: vec![
            AudioInput {
                path: audio(dir, "microphone.m4a", b"mic"),
                track: Track::Microphone,
            },
            AudioInput {
                path: audio(dir, "system.m4a", b"sys"),
                track: Track::System,
            },
        ],
        key: None,
        app: Some("zoom.us".into()),
        summaries: true,
    }
}

fn segment(start: i64, speaker: &str, text: &str) -> Segment {
    Segment {
        start_ms: start,
        end_ms: start + 1000,
        speaker: speaker.into(),
        text: text.into(),
    }
}

fn transcript() -> Transcript {
    Transcript {
        language: Some("es-ES".into()),
        engine: "Apple SpeechAnalyzer".into(),
        segments: vec![
            segment(0, "Me", "Hello."),
            segment(2_000, "Me", "Can you hear me?"),
            segment(5_000, "Others", "Yes."),
            segment(3_725_000, "Me", "Bye."),
        ],
    }
}

#[test]
fn creates_a_meeting_with_two_tracks_and_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let project = space
        .create_node(NewNode::note("Knode", "").with_kind(kinds::PROJECT), &me())
        .unwrap()
        .id;
    let rec = create(&mut space, &meeting(dir.path(), Some(project)), &me()).unwrap();

    assert_eq!(rec.kind, RecordingKind::Meeting);
    assert_eq!(rec.source, Source::Recorder);
    assert_eq!(rec.duration_ms, 45 * 60_000);
    assert_eq!(rec.transcript, TranscriptState::Pending);
    assert_eq!(rec.summary, SummaryState::Pending);
    assert_eq!(rec.app.as_deref(), Some("zoom.us"));
    assert_eq!(rec.project, Some(project));
    assert_eq!(rec.transcript_id, None);
    assert_eq!(
        rec.tracks,
        vec![
            AudioTrack {
                name: "microphone.m4a".into(),
                track: Track::Microphone
            },
            AudioTrack {
                name: "system.m4a".into(),
                track: Track::System
            },
        ]
    );

    let node = space.get_node(&rec.id).unwrap().unwrap();
    assert_eq!(node.kind, "meeting");
    assert!(node.body.contains("![[microphone.m4a]]"));
    assert!(node.body.contains("![[system.m4a]]"));
    assert!(node.body.starts_with("Recorded on "));
    let props = space.properties(&rec.id).unwrap();
    assert_eq!(props[keys::STARTED_AT], START.to_string());
    assert_eq!(props[keys::SOURCE], "recorder");
    assert_eq!(
        props[keys::TRACKS],
        r#"[{"name":"microphone.m4a","track":"microphone"},{"name":"system.m4a","track":"system"}]"#
    );
    let names: Vec<String> = space
        .attachments(&rec.id)
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["microphone.m4a", "system.m4a"]);
    let links = space.links_from(&rec.id).unwrap();
    assert!(
        links
            .iter()
            .any(|l| l.kind == link_kinds::PART_OF && l.to == project)
    );
    assert_eq!(get(&space, &rec.id).unwrap(), rec);
}

#[test]
fn the_header_says_when_and_how_long() {
    assert_eq!(
        header(START, START + 45 * 60_000, &Utc),
        "Recorded on Sunday 22 November 2026, 10:30 to 11:15 (45 minutes)."
    );
    assert_eq!(duration_words(30_000), "30 seconds");
    assert_eq!(duration_words(60_000), "1 minute");
    assert_eq!(duration_words(3_900_000), "1 hour 5 minutes");
    assert_eq!(duration_words(7_200_000), "2 hours");
}

#[test]
fn files_with_the_same_name_are_both_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let other = tempfile::tempdir().unwrap();
    let mut new = meeting(dir.path(), None);
    new.audio[1] = AudioInput {
        path: audio(other.path(), "microphone.m4a", b"other"),
        track: Track::System,
    };
    let rec = create(&mut space, &new, &me()).unwrap();
    assert_eq!(rec.tracks[1].name, "microphone 2.m4a");
    let body = space.get_node(&rec.id).unwrap().unwrap().body;
    assert!(body.contains("![[microphone 2.m4a]]"));
}

#[test]
fn a_key_finds_the_recording_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut memo = meeting(dir.path(), None);
    memo.kind = RecordingKind::VoiceMemo;
    memo.key = Some("memo-1".into());
    memo.summaries = false;
    let first = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(first.source, Source::VoiceMemos);
    assert_eq!(first.summary, SummaryState::Off);
    assert_eq!(first.key.as_deref(), Some("memo-1"));
    memo.title = "Renamed".into();
    let again = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(again, first);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn refuses_missing_files_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut new = meeting(dir.path(), None);
    new.audio[0].path = dir.path().join("nothing.m4a");
    assert!(matches!(
        create(&mut space, &new, &me()),
        Err(Error::Invalid(_))
    ));
    assert_eq!(space.node_count().unwrap(), 0);
}

#[test]
fn pending_lists_oldest_first_and_follows_the_stages() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut late = meeting(dir.path(), None);
    late.started_at += 3_600_000;
    late.ended_at += 3_600_000;
    let late = create(&mut space, &late, &me()).unwrap();
    let early = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let mut off = meeting(dir.path(), None);
    off.summaries = false;
    let off = create(&mut space, &off, &me()).unwrap();
    let ids = |space: &Space| -> Vec<NodeId> {
        pending(space).unwrap().into_iter().map(|r| r.id).collect()
    };
    // Same start: by id, which is creation order.
    assert_eq!(ids(&space), [early.id, off.id, late.id]);

    set_transcript(&mut space, &early.id, &transcript(), &engine()).unwrap();
    set_transcript(&mut space, &off.id, &transcript(), &engine()).unwrap();
    // The summary of `early` is still to do; `off` asks for none.
    assert_eq!(ids(&space), [early.id, late.id]);

    set_failed(
        &mut space,
        &late.id,
        Stage::Transcript,
        Failure::Failed,
        "no speech",
        &engine(),
    )
    .unwrap();
    let summary = Summary {
        model: "Apple Foundation Models".into(),
        overview: "Short.".into(),
        ..Default::default()
    };
    set_summary(&mut space, &early.id, &summary, &engine()).unwrap();
    assert!(ids(&space).is_empty());
}

#[test]
fn transcripts_merge_turns_and_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let rec = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let rec = set_transcript(&mut space, &rec.id, &transcript(), &engine()).unwrap();
    assert_eq!(rec.transcript, TranscriptState::Done);
    assert_eq!(rec.language.as_deref(), Some("es-ES"));
    let tid = rec.transcript_id.unwrap();
    let node = space.get_node(&tid).unwrap().unwrap();
    assert_eq!(node.kind, TRANSCRIPT_KIND);
    assert_eq!(node.title, "Transcript: Weekly sync");
    assert_eq!(node.created_by.actor, "apple-speech");
    assert_eq!(
        node.body,
        "Transcribed on this Mac by Apple SpeechAnalyzer (es-ES).\n\n\
         **Me** (00:00:00): Hello. Can you hear me?\n\n\
         **Others** (00:00:05): Yes.\n\n\
         **Me** (01:02:05): Bye.\n"
    );
    assert!(
        space
            .links_from(&tid)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::DERIVED_FROM && l.to == rec.id)
    );
    let body = space.get_node(&rec.id).unwrap().unwrap().body;
    assert!(body.ends_with("[[Transcript: Weekly sync]]\n"));

    let mut again = transcript();
    again.segments.truncate(1);
    let rec2 = set_transcript(&mut space, &rec.id, &again, &engine()).unwrap();
    assert_eq!(rec2.transcript_id, Some(tid));
    assert_eq!(
        space
            .find_by_title("Transcript: Weekly sync")
            .unwrap()
            .len(),
        1
    );
    assert!(
        space
            .get_node(&tid)
            .unwrap()
            .unwrap()
            .body
            .ends_with("**Me** (00:00:00): Hello.\n")
    );
    let body2 = space.get_node(&rec.id).unwrap().unwrap().body;
    assert_eq!(body2, body);
}

#[test]
fn summaries_omit_empty_sections_and_replace_only_themselves() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let rec = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    set_transcript(&mut space, &rec.id, &transcript(), &engine()).unwrap();
    let llm = Provenance::new(ActorKind::System, "apple-foundation-models");
    let summary = Summary {
        model: "Apple Foundation Models".into(),
        overview: "We planned the release.".into(),
        key_points: vec!["Ship on Friday".into()],
        decisions: vec![],
        action_items: vec![
            ActionItem {
                task: "Write the notes".into(),
                owner: Some("Ada".into()),
            },
            ActionItem {
                task: "Tag the build".into(),
                owner: None,
            },
        ],
        open_questions: vec![],
    };
    let rec = set_summary(&mut space, &rec.id, &summary, &llm).unwrap();
    assert_eq!(rec.summary, SummaryState::Done);
    let node = space.get_node(&rec.id).unwrap().unwrap();
    assert_eq!(node.updated_by.actor, "apple-foundation-models");
    let (head, _) = node.body.split_once("## Summary").unwrap();
    assert!(head.contains("![[microphone.m4a]]"));
    assert!(head.contains("[[Transcript: Weekly sync]]"));
    let minutes = node.body.split_once("## Summary").unwrap().1;
    assert_eq!(
        minutes,
        "\n\nWe planned the release.\n\n\
         ## Key points\n\n- Ship on Friday\n\n\
         ## Action items\n\n- [ ] Write the notes — Ada\n- [ ] Tag the build\n\n\
         _Written on this Mac by Apple Foundation Models from the transcript._\n"
    );
    assert!(!node.body.contains("## Decisions"));
    assert!(!node.body.contains("## Open questions"));

    // Text the person added after the minutes stays; the minutes are replaced.
    let edited = format!("{}\nMy own note.\n", node.body);
    space
        .update_node(
            &rec.id,
            NodeUpdate {
                body: Some(edited),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    let shorter = Summary {
        model: "Apple Foundation Models".into(),
        decisions: vec!["Use Rust".into()],
        ..Default::default()
    };
    set_summary(&mut space, &rec.id, &shorter, &llm).unwrap();
    let body = space.get_node(&rec.id).unwrap().unwrap().body;
    assert_eq!(body.matches("_Written on this Mac").count(), 1);
    assert!(!body.contains("## Key points"));
    assert!(body.contains("## Decisions\n\n- Use Rust"));
    assert!(body.contains("![[system.m4a]]"));
    assert!(body.contains("[[Transcript: Weekly sync]]"));
    assert!(body.ends_with("from the transcript._\n\nMy own note.\n"));
    assert!(body.starts_with(head.trim_end()));
}

#[test]
fn fail_and_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let rec = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    assert!(matches!(
        set_failed(
            &mut space,
            &rec.id,
            Stage::Transcript,
            Failure::Unavailable,
            "x",
            &me()
        ),
        Err(Error::Invalid(_))
    ));
    let failed = set_failed(
        &mut space,
        &rec.id,
        Stage::Transcript,
        Failure::Failed,
        "no speech",
        &me(),
    )
    .unwrap();
    assert_eq!(failed.transcript, TranscriptState::Failed);
    assert_eq!(failed.error.as_deref(), Some("no speech"));
    // A summary waits for the transcript.
    assert!(matches!(
        retry(&mut space, &rec.id, Stage::Summary, &me()),
        Err(Error::Invalid(_))
    ));
    let again = retry(&mut space, &rec.id, Stage::Transcript, &me()).unwrap();
    assert_eq!(again.transcript, TranscriptState::Pending);
    assert_eq!(again.error, None);

    set_transcript(&mut space, &rec.id, &transcript(), &engine()).unwrap();
    let unavailable = set_failed(
        &mut space,
        &rec.id,
        Stage::Summary,
        Failure::Unavailable,
        "Apple Intelligence is off",
        &me(),
    )
    .unwrap();
    assert_eq!(unavailable.summary, SummaryState::Unavailable);
    assert!(pending(&space).unwrap().is_empty());
    let again = retry(&mut space, &rec.id, Stage::Summary, &me()).unwrap();
    assert_eq!(again.summary, SummaryState::Pending);
    assert_eq!(again.error, None);
    assert_eq!(pending(&space).unwrap().len(), 1);
}

#[test]
fn other_nodes_are_not_recordings() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let note = space.create_node(NewNode::note("A", ""), &me()).unwrap();
    assert!(matches!(get(&space, &note.id), Err(Error::Invalid(_))));
}

#[test]
fn minutes_are_on_unless_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    assert!(summaries_enabled(&space).unwrap(), "on by default");
    space
        .set_setting(SUMMARIES_SETTING, Some("off"), &me())
        .unwrap();
    assert!(!summaries_enabled(&space).unwrap());
    space
        .set_setting(SUMMARIES_SETTING, Some("on"), &me())
        .unwrap();
    assert!(summaries_enabled(&space).unwrap());
}

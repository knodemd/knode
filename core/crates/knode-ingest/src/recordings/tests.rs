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
        original: None,
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

/// The title of the transcript of a recording that started at `START`, here.
fn transcript_name(title: &str) -> String {
    transcript_title(title, START, &Local)
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
    assert_eq!(
        header(START, START, &Utc),
        "Recorded on Sunday 22 November 2026 at 10:30."
    );
}

#[test]
fn a_recording_taken_from_a_large_file_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let new = NewRecording {
        kind: RecordingKind::Recording,
        title: "Talk".into(),
        started_at: START,
        ended_at: START + 60_000,
        project: None,
        audio: vec![AudioInput {
            path: audio(dir.path(), "Talk.m4a", b"aac"),
            track: Track::Mixed,
        }],
        key: Some("sha256:abc".into()),
        app: None,
        summaries: false,
        original: Some("Talk.mov".into()),
    };
    let recording = create(&mut space, &new, &me()).unwrap();
    assert_eq!(recording.original.as_deref(), Some("Talk.mov"));
    assert_eq!(recording.source, Source::File);
    let body = space.get_node(&recording.id).unwrap().unwrap().body;
    assert!(
        body.contains("taken from Talk.mov, which is larger than 100 MB"),
        "{body}"
    );
    assert!(body.contains("![[Talk.m4a]]"));
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
fn one_speaker_for_a_whole_room_still_reads_in_paragraphs() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let rec = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let long = "word ".repeat(150);
    let t = Transcript {
        language: None,
        engine: "Apple SpeechAnalyzer".into(),
        segments: vec![
            segment(0, "Speaker", "We start with the budget."),
            segment(1_500, "Speaker", "It is tight."),
            // A pause: a new paragraph.
            segment(20_000, "Speaker", "Next, the demo."),
            // Long talk without pauses: broken once it grows long.
            segment(21_500, "Speaker", long.trim()),
            segment(23_000, "Speaker", long.trim()),
        ],
    };
    let rec = set_transcript(&mut space, &rec.id, &t, &engine()).unwrap();
    let body = space
        .get_node(&rec.transcript_id.unwrap())
        .unwrap()
        .unwrap()
        .body;
    let starts: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("**Speaker**"))
        .map(|l| &l[..22])
        .collect();
    assert_eq!(
        starts,
        vec![
            "**Speaker** (00:00:00)",
            "**Speaker** (00:00:20)",
            "**Speaker** (00:00:23)"
        ]
    );
    assert!(body.contains("(00:00:00): We start with the budget. It is tight.\n"));
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
    let name = transcript_name("Weekly sync");
    assert_eq!(node.title, name);
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
    assert!(body.ends_with(&format!("[[{name}]]\n")));

    let mut again = transcript();
    again.segments.truncate(1);
    let rec2 = set_transcript(&mut space, &rec.id, &again, &engine()).unwrap();
    assert_eq!(rec2.transcript_id, Some(tid));
    assert_eq!(space.find_by_title(&name).unwrap().len(), 1);
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
    let name = transcript_name("Weekly sync");
    let (head, minutes) = node.body.split_once(MINUTES_START).unwrap();
    assert!(head.contains("![[microphone.m4a]]"));
    assert!(head.contains(&format!("[[{name}]]")));
    assert_eq!(
        minutes,
        "\n## Summary\n\nWe planned the release.\n\n\
         ## Key points\n\n- Ship on Friday\n\n\
         ## Action items\n\n- [ ] Write the notes — Ada\n- [ ] Tag the build\n\n\
         _Written on this Mac by Apple Foundation Models from the transcript._\n\
         <!-- /knode:minutes -->\n"
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
    assert!(body.contains(&format!("[[{name}]]")));
    assert!(body.ends_with("from the transcript._\n<!-- /knode:minutes -->\n\nMy own note.\n"));
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

#[test]
fn a_half_made_recording_is_completed_when_stored_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let project = space
        .create_node(NewNode::note("Knode", "").with_kind(kinds::PROJECT), &me())
        .unwrap()
        .id;
    let mut memo = meeting(dir.path(), Some(project));
    memo.key = Some("memo-1".into());
    // What an interrupted store left: the node with its key and one file, no link.
    let props = Properties::from([
        (keys::KEY.to_string(), "memo-1".to_string()),
        (keys::TRANSCRIPT.to_string(), "pending".to_string()),
        (keys::SUMMARY.to_string(), "pending".to_string()),
        (
            keys::TRACKS.to_string(),
            r#"[{"name":"microphone.m4a","track":"microphone"},{"name":"system.m4a","track":"system"}]"#
                .to_string(),
        ),
    ]);
    let partial = space
        .create_node_with_properties(
            NewNode::note(
                "Weekly sync",
                "Recorded.\n\n![[microphone.m4a]]\n\n![[system.m4a]]\n",
            )
            .with_kind("meeting"),
            &props,
            &me(),
        )
        .unwrap()
        .id;
    space
        .attach_file(&partial, &memo.audio[0].path, &me())
        .unwrap();

    let rec = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(rec.id, partial);
    assert_eq!(rec.project, Some(project));
    let names: Vec<String> = space
        .attachments(&rec.id)
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["microphone.m4a", "system.m4a"]);
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
    let body = space.get_node(&rec.id).unwrap().unwrap().body;
    assert_eq!(body.matches("![[system.m4a]]").count(), 1);

    // Whole now: storing it again changes nothing.
    let again = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(again, rec);
    assert_eq!(space.attachments(&rec.id).unwrap().len(), 2);
}

#[test]
fn tracks_list_only_what_is_attached() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut memo = meeting(dir.path(), None);
    memo.key = Some("memo-2".into());
    let rec = create(&mut space, &memo, &me()).unwrap();
    space.detach(&rec.id, "system.m4a", &me()).unwrap();
    memo.audio.truncate(1);
    let again = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(
        again.tracks,
        vec![AudioTrack {
            name: "microphone.m4a".into(),
            track: Track::Microphone
        }]
    );
}

#[cfg(unix)]
#[test]
fn a_store_that_fails_halfway_leaves_no_key() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let mut memo = meeting(dir.path(), None);
    memo.key = Some("memo-3".into());
    // It passes the checks but cannot be read: attaching it fails after the node is made.
    fs::set_permissions(&memo.audio[1].path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&memo.audio[1].path).is_ok() {
        return; // Running as root reads it anyway.
    }
    assert!(create(&mut space, &memo, &me()).is_err());
    assert_eq!(find_by_key(&space, "memo-3").unwrap(), None);
    assert_eq!(space.node_count().unwrap(), 0);

    fs::set_permissions(&memo.audio[1].path, fs::Permissions::from_mode(0o644)).unwrap();
    let rec = create(&mut space, &memo, &me()).unwrap();
    assert_eq!(rec.tracks.len(), 2);
    assert_eq!(space.node_count().unwrap(), 1);
}

#[test]
fn summaries_default_to_on_when_omitted() {
    let new: NewRecording = serde_json::from_str(
        r#"{"kind":"voice-memo","title":"Memo","started_at":0,"ended_at":1000}"#,
    )
    .unwrap();
    assert!(new.summaries);
}

#[test]
fn minutes_never_touch_the_persons_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let rec = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    set_transcript(&mut space, &rec.id, &transcript(), &engine()).unwrap();
    let node = space.get_node(&rec.id).unwrap().unwrap();
    // The person writes headings of their own, one with the minutes' name, before any minutes.
    let mine = "## Summary\n\nMy own summary.\n\n## Decisions\n\nMine too.\n";
    let edited = format!("{}\n{mine}", node.body);
    space
        .update_node(
            &rec.id,
            NodeUpdate {
                body: Some(edited.clone()),
                ..Default::default()
            },
            &me(),
        )
        .unwrap();
    let summary = Summary {
        model: "Apple Foundation Models".into(),
        overview: "Generated.".into(),
        ..Default::default()
    };
    set_summary(&mut space, &rec.id, &summary, &engine()).unwrap();
    let body = space.get_node(&rec.id).unwrap().unwrap().body;
    assert!(body.ends_with(mine), "{body}");
    assert!(body.contains("My own summary."));
    let name = transcript_name("Weekly sync");
    let (before, minutes) = body.split_once(MINUTES_START).unwrap();
    assert!(before.trim_end().ends_with(&format!("[[{name}]]")));
    assert!(minutes.contains("Generated."));

    // Again: only what is between the markers changes.
    let again = Summary {
        model: "Apple Foundation Models".into(),
        overview: "Second.".into(),
        ..Default::default()
    };
    set_summary(&mut space, &rec.id, &again, &engine()).unwrap();
    let body2 = space.get_node(&rec.id).unwrap().unwrap().body;
    assert_eq!(body2, body.replace("Generated.", "Second."));
}

#[test]
fn minutes_markers_are_found_only_when_unambiguous() {
    let block = format!("{MINUTES_START}\nx\n{MINUTES_END}");
    let body = format!("Head\n\n{block}\n\nAfter\n");
    let (s, e) = minutes_range(&body).unwrap();
    assert_eq!(&body[s..e], block);
    assert_eq!(
        minutes_range(&format!("Head\n\n{MINUTES_START}\nx\n")),
        None
    );
    assert_eq!(
        minutes_range(&format!(
            "{MINUTES_START}\n{MINUTES_START}\nx\n{MINUTES_END}\n"
        )),
        None
    );
    assert_eq!(minutes_range(&format!("{MINUTES_END}\nx\n")), None);
}

#[test]
fn each_recording_links_to_its_own_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let a = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let b = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let a = set_transcript(&mut space, &a.id, &transcript(), &engine()).unwrap();
    let b = set_transcript(&mut space, &b.id, &transcript(), &engine()).unwrap();
    assert_ne!(a.transcript_id, b.transcript_id);
    let name = transcript_name("Weekly sync");
    let title_of =
        |space: &Space, id: Option<NodeId>| space.get_node(&id.unwrap()).unwrap().unwrap().title;
    assert_eq!(title_of(&space, a.transcript_id), name);
    assert_eq!(title_of(&space, b.transcript_id), format!("{name} 2"));
    for rec in [&a, &b] {
        let link = format!("[[{}]]", title_of(&space, rec.transcript_id));
        assert!(
            space
                .get_node(&rec.id)
                .unwrap()
                .unwrap()
                .body
                .contains(&link)
        );
        let target = space
            .links_from(&rec.id)
            .unwrap()
            .into_iter()
            .find(|l| l.kind == link_kinds::MENTIONS)
            .map(|l| l.to);
        assert_eq!(target, rec.transcript_id);
    }
    // Written again, each keeps its title.
    let b2 = set_transcript(&mut space, &b.id, &transcript(), &engine()).unwrap();
    assert_eq!(title_of(&space, b2.transcript_id), format!("{name} 2"));
}

#[test]
fn transcript_titles_say_when() {
    assert_eq!(
        transcript_title("Weekly sync", START, &Utc),
        "Transcript: Weekly sync (2026-11-22 10:30)"
    );
}

#[test]
fn failed_lists_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let early = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let mut late = meeting(dir.path(), None);
    late.started_at += 3_600_000;
    late.ended_at += 3_600_000;
    let late = create(&mut space, &late, &me()).unwrap();
    let mut later = meeting(dir.path(), None);
    later.started_at += 7_200_000;
    later.ended_at += 7_200_000;
    let later = create(&mut space, &later, &me()).unwrap();
    let fine = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    let ids = |space: &Space| -> Vec<NodeId> {
        failed(space).unwrap().into_iter().map(|r| r.id).collect()
    };
    assert!(ids(&space).is_empty());

    set_failed(
        &mut space,
        &early.id,
        Stage::Transcript,
        Failure::Failed,
        "x",
        &me(),
    )
    .unwrap();
    set_transcript(&mut space, &late.id, &transcript(), &engine()).unwrap();
    set_failed(
        &mut space,
        &late.id,
        Stage::Summary,
        Failure::Unavailable,
        "y",
        &me(),
    )
    .unwrap();
    set_transcript(&mut space, &later.id, &transcript(), &engine()).unwrap();
    set_failed(
        &mut space,
        &later.id,
        Stage::Summary,
        Failure::Failed,
        "z",
        &me(),
    )
    .unwrap();
    set_transcript(&mut space, &fine.id, &transcript(), &engine()).unwrap();
    assert_eq!(ids(&space), [later.id, late.id, early.id]);

    retry(&mut space, &later.id, Stage::Summary, &me()).unwrap();
    assert_eq!(ids(&space), [late.id, early.id]);
}

#[test]
fn an_unreadable_recording_does_not_hide_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let other = space
        .create_node(NewNode::note("Elsewhere", ""), &me())
        .unwrap()
        .id;
    let broken = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    space
        .link(&broken.id, &other, link_kinds::PART_OF, &me())
        .unwrap();
    let fine = create(&mut space, &meeting(dir.path(), None), &me()).unwrap();
    // A node it links to is damaged: the recording cannot be read.
    let db = rusqlite::Connection::open(space.path().join("knode.db")).unwrap();
    db.execute_batch(&format!(
        "PRAGMA foreign_keys = OFF; PRAGMA ignore_check_constraints = ON;
         UPDATE nodes SET id = 'damaged' WHERE id = '{other}';
         UPDATE links SET target_id = 'damaged' WHERE target_id = '{other}';"
    ))
    .unwrap();
    assert!(get(&space, &broken.id).is_err());
    let ids: Vec<NodeId> = pending(&space).unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, [fine.id]);
}

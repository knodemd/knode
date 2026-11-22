//! Recordings: meetings recorded on this device, voice memos and audio files added by hand,
//! with their transcript and their summary, stored as nodes.
//!
//! A recording is one node of kind [`MEETING_KIND`], [`VOICE_MEMO_KIND`] or [`RECORDING_KIND`]:
//! a short header saying when it was recorded and for how long, an `![[name]]` embed for each
//! audio file attached to it and, once written, the minutes between two HTML comment lines
//! ([`set_summary`]). Its properties ([`keys`]) say when it started and ended, where it came
//! from, which audio track is which and how far its transcript and summary are. A recording made
//! for a project is `part-of` it.
//!
//! The transcript is a node of its own, of kind [`TRANSCRIPT_KIND`], `derived-from` the
//! recording, written as Markdown turns: one paragraph per run of the same speaker, broken after
//! a pause or when it grows long so one microphone for a whole room still reads in paragraphs,
//! each with the time it started ([`set_transcript`]). Its title says when the recording started,
//! so it is unique, and the recording's body links to it.
//!
//! Transcribing and summarising happen on the device, in the app; this module only stores what
//! they produce and keeps track of what is still to do ([`pending`]) and what did not get through
//! ([`failed`]). Nothing is sent anywhere.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, FixedOffset, Local, TimeZone};
use knode_format::{
    Error, MAX_ATTACHMENT_SIZE, NewNode, NodeId, NodeUpdate, Properties, Provenance, Result, Space,
    kinds, link_kinds, setting_keys,
};
use serde::{Deserialize, Serialize};

/// The kind of a meeting recorded by the app, in person or online.
pub const MEETING_KIND: &str = "meeting";
/// The kind of a recording brought in from Voice Memos.
pub const VOICE_MEMO_KIND: &str = "voice-memo";
/// The kind of an audio file added by hand.
pub const RECORDING_KIND: &str = "recording";
/// The kind of the transcript of a recording.
pub const TRANSCRIPT_KIND: &str = "transcript";

/// The properties of a recording's node. Every value is a string.
pub mod keys {
    /// When it started, in Unix milliseconds.
    pub const STARTED_AT: &str = "recording.started_at";
    /// When it ended, in Unix milliseconds.
    pub const ENDED_AT: &str = "recording.ended_at";
    /// How long it lasted, in milliseconds.
    pub const DURATION_MS: &str = "recording.duration_ms";
    /// Where it came from: `recorder`, `voice-memos` or `file`.
    pub const SOURCE: &str = "recording.source";
    /// A key that finds it again, such as the unique id of a voice memo.
    pub const KEY: &str = "recording.key";
    /// The language of its transcript, BCP-47, such as `es-ES`.
    pub const LANGUAGE: &str = "recording.language";
    /// Its transcript: `pending`, `done` or `failed`.
    pub const TRANSCRIPT: &str = "recording.transcript";
    /// Its summary: `pending`, `done`, `off`, `unavailable` or `failed`.
    pub const SUMMARY: &str = "recording.summary";
    /// Why the transcript or the summary failed or is unavailable.
    pub const ERROR: &str = "recording.error";
    /// The meeting app, such as `zoom.us`.
    pub const APP: &str = "recording.app";
    /// Which attached audio file is which track: JSON, `[{"name": ..., "track": ...}]`.
    pub const TRACKS: &str = "recording.tracks";
    /// The file its audio was taken from, when that file is not attached: a video larger than a
    /// space attaches, kept only where it was.
    pub const ORIGINAL: &str = "recording.original";
}

/// The space setting that says whether recordings are summarised: `on` (the default) or `off`.
/// Minutes are written on the device and the transcript is always kept, so they are on unless
/// the person turns them off.
pub const SUMMARIES_SETTING: &str = setting_keys::RECORDINGS_SUMMARIES;

/// Whether the space asks for recordings to be summarised: its `recordings.summaries` setting is
/// not `off`.
pub fn summaries_enabled(space: &Space) -> Result<bool> {
    Ok(space.setting(SUMMARIES_SETTING)?.as_deref() != Some("off"))
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Its name, as stored and as the protocol takes it.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(Error::Invalid(format!(
                        concat!("{:?} is not a ", stringify!($name)),
                        other
                    ))),
                }
            }
        }
    };
}
pub(crate) use string_enum;

string_enum! {
    /// What a recording is.
    RecordingKind {
        /// A meeting recorded by the app, in person or online.
        Meeting => "meeting",
        /// A recording from Voice Memos.
        VoiceMemo => "voice-memo",
        /// An audio file added by hand.
        Recording => "recording",
    }
}

string_enum! {
    /// Where a recording came from.
    Source {
        /// Recorded by the app.
        Recorder => "recorder",
        /// Brought in from Voice Memos.
        VoiceMemos => "voice-memos",
        /// An audio file added by hand.
        File => "file",
    }
}

string_enum! {
    /// Who an audio file holds.
    Track {
        /// The person recording: "Me".
        Microphone => "microphone",
        /// The others in an online meeting, as the Mac played them.
        System => "system",
        /// Everyone in one file: a voice memo, or a meeting in person on one microphone.
        Mixed => "mixed",
    }
}

string_enum! {
    /// How far the transcript of a recording is.
    TranscriptState {
        /// Still to be made.
        Pending => "pending",
        /// Written.
        Done => "done",
        /// Making it failed; [`Recording::error`] says why.
        Failed => "failed",
    }
}

string_enum! {
    /// How far the summary of a recording is.
    SummaryState {
        /// Still to be made, once the transcript is done.
        Pending => "pending",
        /// Written.
        Done => "done",
        /// Not asked for.
        Off => "off",
        /// This device cannot make one; [`Recording::error`] says why.
        Unavailable => "unavailable",
        /// Making it failed; [`Recording::error`] says why.
        Failed => "failed",
    }
}

string_enum! {
    /// A step that happens after a recording is stored.
    Stage {
        /// Transcribing it.
        Transcript => "transcript",
        /// Summarising its transcript.
        Summary => "summary",
    }
}

string_enum! {
    /// How a stage did not succeed.
    Failure {
        /// It was tried and failed.
        Failed => "failed",
        /// This device cannot do it (summaries only), such as without Apple Intelligence.
        Unavailable => "unavailable",
    }
}

impl RecordingKind {
    /// The kind of its node.
    pub fn node_kind(self) -> &'static str {
        self.as_str()
    }

    /// Where a recording of this kind comes from.
    pub fn source(self) -> Source {
        match self {
            RecordingKind::Meeting => Source::Recorder,
            RecordingKind::VoiceMemo => Source::VoiceMemos,
            RecordingKind::Recording => Source::File,
        }
    }

    fn of_node(kind: &str) -> Option<RecordingKind> {
        kind.parse().ok()
    }
}

/// An audio file to attach to a new recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInput {
    /// The file. It is copied into the space.
    pub path: PathBuf,
    /// Who it holds.
    pub track: Track,
}

/// An audio file attached to a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrack {
    /// The attachment's name in the recording's node.
    pub name: String,
    /// Who it holds.
    pub track: Track,
}

/// A recording to store with [`create`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRecording {
    /// What it is.
    pub kind: RecordingKind,
    /// Its title.
    pub title: String,
    /// When it started, in Unix milliseconds.
    pub started_at: i64,
    /// When it ended, in Unix milliseconds.
    pub ended_at: i64,
    /// The project it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<NodeId>,
    /// Its audio files, each attached to the node.
    #[serde(default)]
    pub audio: Vec<AudioInput>,
    /// A key that finds it again: storing a recording with a key the space already has returns
    /// that recording, with any audio file or project link it lacks added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The meeting app, such as `zoom.us`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Whether to summarise it once it is transcribed. True when omitted.
    #[serde(default = "yes")]
    pub summaries: bool,
    /// The name of the file its audio was taken from, when that file is not attached, such as a
    /// video larger than a space attaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
}

fn yes() -> bool {
    true
}

/// A stored recording, as its node and properties say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// Its node.
    pub id: NodeId,
    /// What it is.
    pub kind: RecordingKind,
    /// Its title.
    pub title: String,
    /// When it started, in Unix milliseconds.
    pub started_at: i64,
    /// When it ended, in Unix milliseconds.
    pub ended_at: i64,
    /// How long it lasted, in milliseconds.
    pub duration_ms: i64,
    /// Where it came from.
    pub source: Source,
    /// The key that finds it again, if it has one.
    pub key: Option<String>,
    /// The language of its transcript, BCP-47.
    pub language: Option<String>,
    /// How far its transcript is.
    pub transcript: TranscriptState,
    /// How far its summary is.
    pub summary: SummaryState,
    /// Why the transcript or the summary failed or is unavailable.
    pub error: Option<String>,
    /// The meeting app.
    pub app: Option<String>,
    /// The project it is part of.
    pub project: Option<NodeId>,
    /// Its audio files.
    pub tracks: Vec<AudioTrack>,
    /// The node of its transcript, once written.
    pub transcript_id: Option<NodeId>,
    /// The file its audio was taken from, when that file is not attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
}

/// What was said in a recording, as the app's speech recognition heard it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    /// The language, BCP-47, such as `es-ES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// What made it, such as `Apple SpeechAnalyzer`.
    pub engine: String,
    /// What was said, in order.
    pub segments: Vec<Segment>,
}

/// One stretch of speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Where it starts in the recording, in milliseconds.
    pub start_ms: i64,
    /// Where it ends, in milliseconds.
    pub end_ms: i64,
    /// Who spoke: `Me`, `Others`, `Speaker 1`...
    pub speaker: String,
    /// What they said.
    pub text: String,
}

/// The minutes of a recording, made from its transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// What made it, such as `Apple Foundation Models`.
    pub model: String,
    /// What it was about, in a few sentences.
    #[serde(default)]
    pub overview: String,
    /// The main points.
    #[serde(default)]
    pub key_points: Vec<String>,
    /// What was decided.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// What is to be done, and by whom.
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    /// What was left open.
    #[serde(default)]
    pub open_questions: Vec<String>,
}

/// Something to do after a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    /// What to do.
    pub task: String,
    /// Who does it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Stores a new recording: its node, with its audio files attached (copied into the space), its
/// properties, and a `part-of` link to its project. Its transcript is pending, and so is its
/// summary when `summaries` is set (else off). Every audio file is checked before anything is
/// written.
///
/// The `recording.key` is written last, once the files are attached and the link made, and a
/// node left half made by a failed step is deleted, so a key the space has always names a whole
/// recording. When the recording has a `key` that a recording of the space already has, that
/// recording is returned, with the audio files of `new` it lacks (by track) attached and its
/// project link added.
pub fn create(space: &mut Space, new: &NewRecording, by: &Provenance) -> Result<Recording> {
    let key = new.key.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if let Some(key) = key
        && let Some(id) = find_by_key(space, key)?
    {
        return complete(space, &id, new, by);
    }
    if new.ended_at < new.started_at {
        return Err(Error::Invalid(
            "a recording cannot end before it starts".into(),
        ));
    }
    for audio in &new.audio {
        check_audio(audio)?;
    }
    if let Some(project) = &new.project
        && space.get_node(project)?.is_none()
    {
        return Err(Error::NotFound(*project));
    }

    let planned: Vec<AudioTrack> = new
        .audio
        .iter()
        .map(|a| AudioTrack {
            name: file_name(a),
            track: a.track,
        })
        .collect();
    let mut props = Properties::new();
    props.insert(keys::STARTED_AT.into(), new.started_at.to_string());
    props.insert(keys::ENDED_AT.into(), new.ended_at.to_string());
    props.insert(
        keys::DURATION_MS.into(),
        (new.ended_at - new.started_at).to_string(),
    );
    props.insert(keys::SOURCE.into(), new.kind.source().to_string());
    props.insert(
        keys::TRANSCRIPT.into(),
        TranscriptState::Pending.to_string(),
    );
    let summary = if new.summaries {
        SummaryState::Pending
    } else {
        SummaryState::Off
    };
    props.insert(keys::SUMMARY.into(), summary.to_string());
    props.insert(keys::TRACKS.into(), tracks_json(&planned));
    if let Some(app) = new.app.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        props.insert(keys::APP.into(), app.to_string());
    }
    let original = new
        .original
        .as_deref()
        .map(one_line)
        .filter(|o| !o.is_empty());
    let mut header = header(new.started_at, new.ended_at, &Local);
    if let Some(original) = &original {
        props.insert(keys::ORIGINAL.into(), original.clone());
        header.push_str(&format!(
            "\n\nThe audio was taken from {original}, which is larger than {} MB and is kept only \
             where it was, not in the space.",
            MAX_ATTACHMENT_SIZE / (1024 * 1024)
        ));
    }
    let node = space.create_node_with_properties(
        NewNode::note(new.title.clone(), header_body(&header, &planned))
            .with_kind(new.kind.node_kind()),
        &props,
        by,
    )?;
    let id = node.id;
    let filled = fill(space, &id, new, key, &header, &planned, by);
    if let Err(e) = filled {
        // Best effort: the error says what went wrong either way.
        let _ = space.delete_node(&id, by);
        return Err(e);
    }
    get(space, &id)
}

/// The steps of [`create`] after its node is made: the files, the link, and the key last.
fn fill(
    space: &mut Space,
    id: &NodeId,
    new: &NewRecording,
    key: Option<&str>,
    header: &str,
    planned: &[AudioTrack],
    by: &Provenance,
) -> Result<()> {
    // A name the node already uses for other content is attached under another one: the body
    // and the tracks then say so.
    let mut attached = Vec::with_capacity(new.audio.len());
    for audio in &new.audio {
        let attachment = space.attach_file(id, &audio.path, by)?;
        attached.push(AudioTrack {
            name: attachment.name,
            track: audio.track,
        });
    }
    if attached != planned {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(header_body(header, &attached)),
                ..Default::default()
            },
            by,
        )?;
        space.set_property(id, keys::TRACKS, &tracks_json(&attached), by)?;
    }
    if let Some(project) = &new.project {
        space.link(id, project, link_kinds::PART_OF, by)?;
    }
    if let Some(key) = key {
        space.set_property(id, keys::KEY, key, by)?;
    }
    Ok(())
}

/// A recording found again by its key, made whole: each audio file of `new` it lacks (no
/// attached track of the same kind) is attached and embedded, and its project link is added.
/// Its tracks then list only what is attached.
fn complete(
    space: &mut Space,
    id: &NodeId,
    new: &NewRecording,
    by: &Provenance,
) -> Result<Recording> {
    let recording = get(space, id)?;
    let names: Vec<String> = space.attachments(id)?.into_iter().map(|a| a.name).collect();
    let mut tracks: Vec<AudioTrack> = recording
        .tracks
        .iter()
        .filter(|t| names.contains(&t.name))
        .cloned()
        .collect();
    let mut used = vec![false; tracks.len()];
    let mut missing = Vec::new();
    for audio in &new.audio {
        let name = file_name(audio);
        let same = |exact: bool| {
            tracks
                .iter()
                .enumerate()
                .position(|(i, t)| !used[i] && t.track == audio.track && (!exact || t.name == name))
        };
        match same(true).or_else(|| same(false)) {
            Some(i) => used[i] = true,
            None => missing.push(audio),
        }
    }
    for audio in &missing {
        check_audio(audio)?;
    }
    let original = space.get_node(id)?.ok_or(Error::NotFound(*id))?.body;
    let mut body = original.clone();
    let mut anchors: Vec<String> = tracks.iter().map(|t| format!("![[{}]]", t.name)).collect();
    for audio in missing {
        let attachment = space.attach_file(id, &audio.path, by)?;
        let embed = format!("![[{}]]", attachment.name);
        if !body.contains(&embed) {
            body = insert_after(&body, &anchors, &embed);
        }
        anchors.push(embed);
        tracks.push(AudioTrack {
            name: attachment.name,
            track: audio.track,
        });
    }
    if body != original {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    if tracks != recording.tracks {
        space.set_property(id, keys::TRACKS, &tracks_json(&tracks), by)?;
    }
    if let Some(project) = &new.project
        && space.get_node(project)?.is_some()
    {
        space.link(id, project, link_kinds::PART_OF, by)?;
    }
    get(space, id)
}

fn file_name(audio: &AudioInput) -> String {
    audio
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fails unless `audio` is a file a space can attach.
fn check_audio(audio: &AudioInput) -> Result<()> {
    let metadata = fs::metadata(&audio.path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", audio.path.display())))?;
    if !metadata.is_file() {
        return Err(Error::Invalid(format!(
            "{} is not a file",
            audio.path.display()
        )));
    }
    if metadata.len() > MAX_ATTACHMENT_SIZE {
        return Err(Error::Invalid(format!(
            "{} is larger than {} MB, the most a space attaches",
            audio.path.display(),
            MAX_ATTACHMENT_SIZE / (1024 * 1024)
        )));
    }
    Ok(())
}

/// The recording of the space with this key, if any.
pub fn find_by_key(space: &Space, key: &str) -> Result<Option<NodeId>> {
    Ok(space
        .find_by_property(keys::KEY, key)?
        .into_iter()
        .find(|s| RecordingKind::of_node(&s.kind).is_some())
        .map(|s| s.id))
}

/// A recording, read from its node. Fails with [`Error::NotFound`] when there is no such node
/// and [`Error::Invalid`] when the node is not a recording.
pub fn get(space: &Space, id: &NodeId) -> Result<Recording> {
    let node = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let kind = RecordingKind::of_node(&node.kind)
        .ok_or_else(|| Error::Invalid(format!("node {id} is not a recording")))?;
    let props = space.properties(id)?;
    let text = |key: &str| props.get(key).filter(|v| !v.is_empty()).cloned();
    let number = |key: &str| text(key).and_then(|v| v.parse::<i64>().ok());
    let started_at = number(keys::STARTED_AT).unwrap_or_else(|| node.created_at.as_millis());
    let ended_at = number(keys::ENDED_AT).unwrap_or(started_at);
    let tracks = text(keys::TRACKS)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut project = None;
    for link in space.links_from(id)? {
        if link.kind == link_kinds::PART_OF
            && space
                .get_node(&link.to)?
                .is_some_and(|n| n.kind == kinds::PROJECT)
        {
            project = Some(link.to);
            break;
        }
    }
    Ok(Recording {
        id: *id,
        kind,
        title: node.title,
        started_at,
        ended_at,
        duration_ms: number(keys::DURATION_MS).unwrap_or(ended_at - started_at),
        source: text(keys::SOURCE)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| kind.source()),
        key: text(keys::KEY),
        language: text(keys::LANGUAGE),
        transcript: text(keys::TRANSCRIPT)
            .and_then(|s| s.parse().ok())
            .unwrap_or(TranscriptState::Pending),
        summary: text(keys::SUMMARY)
            .and_then(|s| s.parse().ok())
            .unwrap_or(SummaryState::Off),
        error: text(keys::ERROR),
        app: text(keys::APP),
        project,
        tracks,
        transcript_id: transcript_of(space, id)?,
        original: text(keys::ORIGINAL),
    })
}

/// The recordings with work to do, oldest first (by when they started): those whose transcript
/// is pending, and those whose summary is pending once their transcript is done. A recording that
/// cannot be read is left out, and said on standard error, so it does not hide the others.
pub fn pending(space: &Space) -> Result<Vec<Recording>> {
    let mut list = find(
        space,
        &[
            (keys::TRANSCRIPT, TranscriptState::Pending.as_str()),
            (keys::SUMMARY, SummaryState::Pending.as_str()),
        ],
        |r| {
            r.transcript == TranscriptState::Pending
                || (r.summary == SummaryState::Pending && r.transcript == TranscriptState::Done)
        },
    )?;
    list.sort_by_key(|r| (r.started_at, r.id));
    Ok(list)
}

/// The recordings that did not get through, newest first (by when they started): those whose
/// transcript failed, and those whose summary failed or is unavailable. A recording that cannot
/// be read is left out, and said on standard error.
pub fn failed(space: &Space) -> Result<Vec<Recording>> {
    let mut list = find(
        space,
        &[
            (keys::TRANSCRIPT, TranscriptState::Failed.as_str()),
            (keys::SUMMARY, SummaryState::Failed.as_str()),
            (keys::SUMMARY, SummaryState::Unavailable.as_str()),
        ],
        |r| {
            r.transcript == TranscriptState::Failed
                || matches!(r.summary, SummaryState::Failed | SummaryState::Unavailable)
        },
    )?;
    list.sort_by_key(|r| std::cmp::Reverse((r.started_at, r.id)));
    Ok(list)
}

/// The recordings with any of these properties for which `keep` holds, each once. One that
/// cannot be read is skipped.
fn find(
    space: &Space,
    properties: &[(&str, &str)],
    keep: impl Fn(&Recording) -> bool,
) -> Result<Vec<Recording>> {
    let mut found = BTreeMap::new();
    for (key, value) in properties {
        for summary in space.find_by_property(key, value)? {
            if RecordingKind::of_node(&summary.kind).is_none() || found.contains_key(&summary.id) {
                continue;
            }
            match get(space, &summary.id) {
                Ok(recording) => {
                    if keep(&recording) {
                        found.insert(summary.id, recording);
                    }
                }
                Err(e) => eprintln!("Knode: skipping recording {}: {e}", summary.id),
            }
        }
    }
    Ok(found.into_values().collect())
}

/// Writes the transcript of a recording: creates its node, `derived-from` the recording, or
/// replaces the one written before, so a recording has one. The recording's transcript is then
/// done, in the transcript's language, and its body links to the transcript.
pub fn set_transcript(
    space: &mut Space,
    id: &NodeId,
    transcript: &Transcript,
    by: &Provenance,
) -> Result<Recording> {
    let recording = get(space, id)?;
    let base = transcript_title(&recording.title, recording.started_at, &Local);
    let title = free_title(space, &base, recording.transcript_id)?;
    let body = render_transcript(transcript);
    let old_title = match recording.transcript_id {
        Some(existing) => space.get_node(&existing)?.map(|n| n.title),
        None => None,
    };
    let transcript_id = match recording.transcript_id {
        Some(existing) => {
            space.update_node(
                &existing,
                NodeUpdate {
                    title: Some(title.clone()),
                    body: Some(body),
                    ..Default::default()
                },
                by,
            )?;
            existing
        }
        None => {
            let node = space.create_node(
                NewNode::note(title.clone(), body).with_kind(TRANSCRIPT_KIND),
                by,
            )?;
            space.link(&node.id, id, link_kinds::DERIVED_FROM, by)?;
            node.id
        }
    };
    let link_title = space
        .get_node(&transcript_id)?
        .map(|n| n.title)
        .unwrap_or(title);
    let current = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let link = format!("[[{link_title}]]");
    if !current.body.contains(&link) {
        let old_link = old_title.map(|t| format!("[[{t}]]"));
        let body = match old_link.filter(|l| current.body.contains(l.as_str())) {
            Some(old_link) => current.body.replacen(&old_link, &link, 1),
            None => insert_after(&current.body, &embeds(&recording.tracks), &link),
        };
        space.update_node(
            id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    let mut props = Properties::from([(
        keys::TRANSCRIPT.to_string(),
        TranscriptState::Done.to_string(),
    )]);
    if let Some(language) = transcript
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        props.insert(keys::LANGUAGE.into(), language.to_string());
    }
    space.set_properties(id, &props, by)?;
    if recording.summary != SummaryState::Failed && recording.summary != SummaryState::Unavailable {
        space.remove_property(id, keys::ERROR)?;
    }
    get(space, id)
}

/// Writes the minutes of a recording into its body, between the lines [`MINUTES_START`] and
/// [`MINUTES_END`]: replacing what is between them when the body has them once, in that order,
/// and else as a new block after its header, audio files and transcript link. Nothing outside
/// the markers changes. Its summary is then done.
pub fn set_summary(
    space: &mut Space,
    id: &NodeId,
    summary: &Summary,
    by: &Provenance,
) -> Result<Recording> {
    let recording = get(space, id)?;
    let current = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let block = format!(
        "{MINUTES_START}\n{}\n{MINUTES_END}",
        render_summary(summary)
    );
    let body = match minutes_range(&current.body) {
        Some((start, end)) => format!("{}{block}{}", &current.body[..start], &current.body[end..]),
        None => {
            let mut anchors = embeds(&recording.tracks);
            if let Some(tid) = recording.transcript_id
                && let Some(t) = space.get_node(&tid)?
            {
                anchors.push(format!("[[{}]]", t.title));
            }
            insert_after(&current.body, &anchors, &block)
        }
    };
    if body != current.body {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    space.set_property(id, keys::SUMMARY, SummaryState::Done.as_str(), by)?;
    if recording.transcript != TranscriptState::Failed {
        space.remove_property(id, keys::ERROR)?;
    }
    get(space, id)
}

/// Marks a stage of a recording as failed or unavailable, with why. A transcript is only ever
/// failed.
pub fn set_failed(
    space: &mut Space,
    id: &NodeId,
    stage: Stage,
    state: Failure,
    error: &str,
    by: &Provenance,
) -> Result<Recording> {
    get(space, id)?;
    let (key, value) = match (stage, state) {
        (Stage::Transcript, Failure::Failed) => {
            (keys::TRANSCRIPT, TranscriptState::Failed.as_str())
        }
        (Stage::Transcript, Failure::Unavailable) => {
            return Err(Error::Invalid(
                "a transcript is failed, never unavailable".into(),
            ));
        }
        (Stage::Summary, Failure::Failed) => (keys::SUMMARY, SummaryState::Failed.as_str()),
        (Stage::Summary, Failure::Unavailable) => {
            (keys::SUMMARY, SummaryState::Unavailable.as_str())
        }
    };
    let mut props = Properties::from([(key.to_string(), value.to_string())]);
    let error = error.trim();
    if error.is_empty() {
        space.remove_property(id, keys::ERROR)?;
    } else {
        props.insert(keys::ERROR.into(), error.to_string());
    }
    space.set_properties(id, &props, by)?;
    get(space, id)
}

/// Puts a stage of a recording back to pending, so the app does it again. A summary is pending
/// only once the transcript is done.
pub fn retry(space: &mut Space, id: &NodeId, stage: Stage, by: &Provenance) -> Result<Recording> {
    let recording = get(space, id)?;
    match stage {
        Stage::Transcript => {
            space.set_property(id, keys::TRANSCRIPT, TranscriptState::Pending.as_str(), by)?;
            if !matches!(
                recording.summary,
                SummaryState::Failed | SummaryState::Unavailable
            ) {
                space.remove_property(id, keys::ERROR)?;
            }
        }
        Stage::Summary => {
            if recording.transcript != TranscriptState::Done {
                return Err(Error::Invalid(
                    "a recording is summarised once its transcript is done".into(),
                ));
            }
            space.set_property(id, keys::SUMMARY, SummaryState::Pending.as_str(), by)?;
            space.remove_property(id, keys::ERROR)?;
        }
    }
    get(space, id)
}

/// The transcript node of a recording: a `transcript` node `derived-from` it.
fn transcript_of(space: &Space, id: &NodeId) -> Result<Option<NodeId>> {
    for link in space.backlinks(id)? {
        if link.kind == link_kinds::DERIVED_FROM
            && space
                .get_node(&link.from)?
                .is_some_and(|n| n.kind == TRANSCRIPT_KIND)
        {
            return Ok(Some(link.from));
        }
    }
    Ok(None)
}

fn tracks_json(tracks: &[AudioTrack]) -> String {
    serde_json::to_string(tracks).unwrap_or_else(|_| "[]".into())
}

/// The first line of a recording's body: when it was recorded and for how long, in the time
/// zone given.
fn header<Tz: TimeZone>(started_at: i64, ended_at: i64, zone: &Tz) -> String
where
    Tz::Offset: fmt::Display,
{
    let local = |ms: i64| -> Option<DateTime<FixedOffset>> {
        zone.timestamp_millis_opt(ms)
            .single()
            .map(|t| t.fixed_offset())
    };
    let length = duration_words(ended_at - started_at);
    if ended_at <= started_at {
        // A file whose length is not known.
        return match local(started_at) {
            Some(start) => format!(
                "Recorded on {} at {}.",
                start.format("%A %-d %B %Y"),
                start.format("%H:%M")
            ),
            None => "Recorded.".into(),
        };
    }
    match (local(started_at), local(ended_at)) {
        (Some(start), Some(end)) => format!(
            "Recorded on {}, {} to {} ({length}).",
            start.format("%A %-d %B %Y"),
            start.format("%H:%M"),
            end.format("%H:%M"),
        ),
        _ => format!("Recorded for {length}."),
    }
}

/// A length of time in words: `45 minutes`, `1 hour 5 minutes`, `30 seconds`.
fn duration_words(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let plural = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    if seconds < 60 {
        return plural(seconds, "second");
    }
    let minutes = (seconds + 30) / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    match (hours, minutes) {
        (0, m) => plural(m, "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{} {}", plural(h, "hour"), plural(m, "minute")),
    }
}

fn header_body(header: &str, tracks: &[AudioTrack]) -> String {
    let mut body = header.to_string();
    for track in tracks {
        body.push_str(&format!("\n\n![[{}]]", track.name));
    }
    body.push('\n');
    body
}

/// The headings of the minutes, in order.
const SECTIONS: [&str; 5] = [
    "## Summary",
    "## Key points",
    "## Decisions",
    "## Action items",
    "## Open questions",
];

const WRITTEN_BY: &str = "_Written on this Mac by ";
const FROM_TRANSCRIPT: &str = " from the transcript._";

/// The line before the minutes in a recording's body. An HTML comment: Markdown hides it, and
/// it comes back unchanged from a Markdown file.
pub const MINUTES_START: &str = "<!-- knode:minutes -->";
/// The line after the minutes in a recording's body.
pub const MINUTES_END: &str = "<!-- /knode:minutes -->";

/// Where the minutes are in a body, from the start of its [`MINUTES_START`] line to the end of
/// its [`MINUTES_END`] line (without the line break): only when the body has one start line, and
/// an end line after it.
fn minutes_range(body: &str) -> Option<(usize, usize)> {
    marked_range(body, MINUTES_START, MINUTES_END)
}

/// Where a block is in a body, from the start of its `open` line to the end of its `close` line
/// (without the line break): only when the body has one `open` line, and a `close` line after it.
pub(crate) fn marked_range(body: &str, open: &str, close: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut start = None;
    let mut end = None;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed == open {
            if start.is_some() {
                return None;
            }
            start = Some(offset);
        } else if trimmed == close && start.is_some() && end.is_none() {
            end = Some(offset + line.trim_end_matches(['\n', '\r']).len());
        }
        offset += line.len();
    }
    Some((start?, end?))
}

/// The `![[name]]` embeds of these tracks.
fn embeds(tracks: &[AudioTrack]) -> Vec<String> {
    tracks.iter().map(|t| format!("![[{}]]", t.name)).collect()
}

/// `body` with `block` added as a paragraph of its own after the last line that holds one of
/// `anchors`, or after the first line when none does. The rest of the body stays as it is.
pub(crate) fn insert_after(body: &str, anchors: &[String], block: &str) -> String {
    let line_end = |at: usize| body[at..].find('\n').map_or(body.len(), |i| at + i);
    let at = anchors
        .iter()
        .filter_map(|a| body.rfind(a.as_str()).map(line_end))
        .max()
        .unwrap_or_else(|| line_end(0));
    let (head, rest) = body.split_at(at);
    let mut out = String::with_capacity(body.len() + block.len() + 4);
    out.push_str(head);
    if !head.trim().is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
    if rest.trim().is_empty() {
        out.push('\n');
    } else {
        if !rest.starts_with("\n\n") {
            out.push('\n');
        }
        out.push_str(rest);
    }
    out
}

/// The title of a recording's transcript: `Transcript: <title> (<YYYY-MM-DD HH:MM>)`, when it
/// started in the time zone given.
fn transcript_title<Tz: TimeZone>(title: &str, started_at: i64, zone: &Tz) -> String
where
    Tz::Offset: fmt::Display,
{
    let title = one_line(title);
    match zone.timestamp_millis_opt(started_at).single() {
        Some(start) => format!("Transcript: {title} ({})", start.format("%Y-%m-%d %H:%M")),
        None => format!("Transcript: {title}"),
    }
}

/// `base`, or `base 2`, `base 3`... the first that no node but `own` has as its title, so a
/// `[[link]]` to it finds that node and no other.
pub(crate) fn free_title(space: &Space, base: &str, own: Option<NodeId>) -> Result<String> {
    for n in 1.. {
        let title = if n == 1 {
            base.to_string()
        } else {
            format!("{base} {n}")
        };
        if space
            .find_by_title(&title)?
            .iter()
            .all(|s| Some(s.id) == own)
        {
            return Ok(title);
        }
    }
    unreachable!("some title is free")
}

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The minutes as Markdown: a section for each part that has something, and who wrote them.
fn render_summary(summary: &Summary) -> String {
    let mut parts = Vec::new();
    let overview = summary.overview.trim();
    if !overview.is_empty() {
        parts.push(format!("{}\n\n{overview}", SECTIONS[0]));
    }
    let list = |heading: &str, items: Vec<String>| -> Option<String> {
        let items: Vec<String> = items
            .into_iter()
            .filter(|i| !i.is_empty())
            .map(|i| format!("- {i}"))
            .collect();
        (!items.is_empty()).then(|| format!("{heading}\n\n{}", items.join("\n")))
    };
    let lines = |items: &[String]| items.iter().map(|i| one_line(i)).collect::<Vec<_>>();
    parts.extend(list(SECTIONS[1], lines(&summary.key_points)));
    parts.extend(list(SECTIONS[2], lines(&summary.decisions)));
    parts.extend(list(
        SECTIONS[3],
        summary
            .action_items
            .iter()
            .filter(|a| !a.task.trim().is_empty())
            .map(|a| {
                let task = one_line(&a.task);
                match a.owner.as_deref().map(one_line).filter(|o| !o.is_empty()) {
                    Some(owner) => format!("[ ] {task} — {owner}"),
                    None => format!("[ ] {task}"),
                }
            })
            .collect(),
    ));
    parts.extend(list(SECTIONS[4], lines(&summary.open_questions)));
    if parts.is_empty() {
        parts.push(format!("{}\n\nNothing to summarise.", SECTIONS[0]));
    }
    let model = one_line(&summary.model);
    let model = if model.is_empty() { "a model" } else { &model };
    parts.push(format!("{WRITTEN_BY}{model}{FROM_TRANSCRIPT}"));
    parts.join("\n\n")
}

/// A time in a recording: `HH:MM:SS`.
fn timestamp(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A pause, in milliseconds, after which the same speaker starts a new paragraph.
const TURN_PAUSE_MS: i64 = 4_000;
/// About how long a paragraph of one speaker grows before a new one starts.
const TURN_MAX_CHARS: usize = 1_200;

/// A transcript as Markdown: who made it, then one paragraph per turn, `**Speaker**
/// (HH:MM:SS): text`, where a turn is a run of segments by the same speaker, broken after a
/// pause of [`TURN_PAUSE_MS`] or once it is about [`TURN_MAX_CHARS`] long.
fn render_transcript(transcript: &Transcript) -> String {
    let engine = one_line(&transcript.engine);
    let engine = if engine.is_empty() {
        "speech recognition"
    } else {
        &engine
    };
    let mut head = format!("Transcribed on this Mac by {engine}");
    if let Some(language) = transcript
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        head.push_str(&format!(" ({language})"));
    }
    head.push('.');
    let mut turns: Vec<(String, i64, Vec<String>, i64, usize)> = Vec::new();
    for segment in &transcript.segments {
        let text = one_line(&segment.text);
        if text.is_empty() {
            continue;
        }
        let speaker = match one_line(&segment.speaker) {
            s if s.is_empty() => "Unknown".to_string(),
            s => s,
        };
        // A turn goes on while the same speaker talks, until a pause or until it grows long:
        // one microphone for a whole room is all one speaker, and its transcript still reads in
        // paragraphs, each with the moment it starts.
        match turns.last_mut() {
            Some((last, _, texts, end, len))
                if *last == speaker
                    && segment.start_ms - *end < TURN_PAUSE_MS
                    && *len + text.len() <= TURN_MAX_CHARS =>
            {
                *len += text.len() + 1;
                *end = segment.end_ms.max(*end);
                texts.push(text);
            }
            _ => {
                let len = text.len();
                turns.push((speaker, segment.start_ms, vec![text], segment.end_ms, len));
            }
        }
    }
    let mut body = head;
    for (speaker, start, texts, _, _) in turns {
        body.push_str(&format!(
            "\n\n**{speaker}** ({}): {}",
            timestamp(start),
            texts.join(" ")
        ));
    }
    body.push('\n');
    body
}

#[cfg(test)]
mod tests;

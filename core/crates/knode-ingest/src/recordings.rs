//! Recordings: meetings recorded on this device, voice memos and audio files added by hand,
//! with their transcript and their summary, stored as nodes.
//!
//! A recording is one node of kind [`MEETING_KIND`], [`VOICE_MEMO_KIND`] or [`RECORDING_KIND`]:
//! a short header saying when it was recorded and for how long, an `![[name]]` embed for each
//! audio file attached to it and, once written, the minutes ([`set_summary`]). Its properties
//! ([`keys`]) say when it started and ended, where it came from, which audio track is which and
//! how far its transcript and summary are. A recording made for a project is `part-of` it.
//!
//! The transcript is a node of its own, of kind [`TRANSCRIPT_KIND`], `derived-from` the
//! recording, written as Markdown turns: one paragraph per run of the same speaker, each with
//! the time it started ([`set_transcript`]). The recording's body links to it.
//!
//! Transcribing and summarising happen on the device, in the app; this module only stores what
//! they produce and keeps track of what is still to do ([`pending`]). Nothing is sent anywhere.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

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
}

/// The space setting that says whether recordings are summarised: `on` or `off` (the default).
pub const SUMMARIES_SETTING: &str = setting_keys::RECORDINGS_SUMMARIES;

/// Whether the space asks for recordings to be summarised: its `recordings.summaries` setting is
/// `on`.
pub fn summaries_enabled(space: &Space) -> Result<bool> {
    Ok(space.setting(SUMMARIES_SETTING)?.as_deref() == Some("on"))
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

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
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
    /// that recording unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The meeting app, such as `zoom.us`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Whether to summarise it once it is transcribed.
    #[serde(default)]
    pub summaries: bool,
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
/// summary when `summaries` is set (else off).
///
/// When the recording has a `key` that a recording of the space already has, that recording is
/// returned unchanged. Every audio file is checked before anything is written.
pub fn create(space: &mut Space, new: &NewRecording, by: &Provenance) -> Result<Recording> {
    if let Some(key) = new.key.as_deref().map(str::trim).filter(|k| !k.is_empty())
        && let Some(id) = find_by_key(space, key)?
    {
        return get(space, &id);
    }
    if new.ended_at < new.started_at {
        return Err(Error::Invalid(
            "a recording cannot end before it starts".into(),
        ));
    }
    for audio in &new.audio {
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
            name: a
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
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
    if let Some(key) = new.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        props.insert(keys::KEY.into(), key.to_string());
    }
    if let Some(app) = new.app.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        props.insert(keys::APP.into(), app.to_string());
    }
    let header = header(new.started_at, new.ended_at, &Local);
    let node = space.create_node_with_properties(
        NewNode::note(new.title.clone(), header_body(&header, &planned))
            .with_kind(new.kind.node_kind()),
        &props,
        by,
    )?;
    let id = node.id;

    // A name the node already uses for other content is attached under another one: the body
    // and the tracks then say so.
    let mut attached = Vec::with_capacity(new.audio.len());
    for audio in &new.audio {
        let attachment = space.attach_file(&id, &audio.path, by)?;
        attached.push(AudioTrack {
            name: attachment.name,
            track: audio.track,
        });
    }
    if attached != planned {
        space.update_node(
            &id,
            NodeUpdate {
                body: Some(header_body(&header, &attached)),
                ..Default::default()
            },
            by,
        )?;
        space.set_property(&id, keys::TRACKS, &tracks_json(&attached), by)?;
    }
    if let Some(project) = &new.project {
        space.link(&id, project, link_kinds::PART_OF, by)?;
    }
    get(space, &id)
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
    })
}

/// The recordings with work to do, oldest first (by when they started): those whose transcript
/// is pending, and those whose summary is pending once their transcript is done.
pub fn pending(space: &Space) -> Result<Vec<Recording>> {
    let mut found = BTreeMap::new();
    for (key, value) in [
        (keys::TRANSCRIPT, TranscriptState::Pending.as_str()),
        (keys::SUMMARY, SummaryState::Pending.as_str()),
    ] {
        for summary in space.find_by_property(key, value)? {
            if RecordingKind::of_node(&summary.kind).is_none() || found.contains_key(&summary.id) {
                continue;
            }
            let recording = get(space, &summary.id)?;
            let waiting = recording.transcript == TranscriptState::Pending
                || (recording.summary == SummaryState::Pending
                    && recording.transcript == TranscriptState::Done);
            if waiting {
                found.insert(summary.id, recording);
            }
        }
    }
    let mut list: Vec<Recording> = found.into_values().collect();
    list.sort_by_key(|r| (r.started_at, r.id));
    Ok(list)
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
    let title = format!("Transcript: {}", recording.title);
    let body = render_transcript(transcript);
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
        let (head, summary, rest) = split_body(&current.body);
        let head = format!("{}\n\n{link}", head.trim_end());
        space.update_node(
            id,
            NodeUpdate {
                body: Some(join_body(&head, summary, rest)),
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

/// Writes the minutes of a recording into its body, after its header, audio files and
/// transcript link, replacing the minutes written before and nothing else. Its summary is then
/// done.
pub fn set_summary(
    space: &mut Space,
    id: &NodeId,
    summary: &Summary,
    by: &Provenance,
) -> Result<Recording> {
    let recording = get(space, id)?;
    let current = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let (head, _, rest) = split_body(&current.body);
    let minutes = render_summary(summary);
    let body = join_body(head, &minutes, rest);
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

/// Splits a recording's body into what comes before the minutes, the minutes (from their first
/// heading to the line saying who wrote them) and what comes after.
fn split_body(body: &str) -> (&str, &str, &str) {
    let mut offset = 0;
    let mut start = None;
    let mut end = None;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if start.is_none() && SECTIONS.contains(&trimmed) {
            start = Some(offset);
        }
        if start.is_some() && trimmed.starts_with(WRITTEN_BY) && trimmed.ends_with(FROM_TRANSCRIPT)
        {
            end = Some(offset + line.len());
            break;
        }
        offset += line.len();
    }
    match start {
        None => (body, "", ""),
        Some(start) => {
            let end = end.unwrap_or(body.len());
            (&body[..start], &body[start..end], &body[end..])
        }
    }
}

fn join_body(head: &str, minutes: &str, rest: &str) -> String {
    let mut body = head.trim_end().to_string();
    let minutes = minutes.trim();
    if !minutes.is_empty() {
        body.push_str("\n\n");
        body.push_str(minutes);
    }
    let rest = rest.trim();
    if !rest.is_empty() {
        body.push_str("\n\n");
        body.push_str(rest);
    }
    body.push('\n');
    body
}

fn one_line(text: &str) -> String {
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

/// A transcript as Markdown: who made it, then one paragraph per turn, `**Speaker**
/// (HH:MM:SS): text`, where a turn is a run of segments by the same speaker.
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
    let mut turns: Vec<(String, i64, Vec<String>)> = Vec::new();
    for segment in &transcript.segments {
        let text = one_line(&segment.text);
        if text.is_empty() {
            continue;
        }
        let speaker = match one_line(&segment.speaker) {
            s if s.is_empty() => "Unknown".to_string(),
            s => s,
        };
        match turns.last_mut() {
            Some((last, _, texts)) if *last == speaker => texts.push(text),
            _ => turns.push((speaker, segment.start_ms, vec![text])),
        }
    }
    let mut body = head;
    for (speaker, start, texts) in turns {
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

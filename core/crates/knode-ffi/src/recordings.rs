//! Recordings: meetings recorded on this device, voice memos and audio files added by hand,
//! with their transcript and their summary ([`knode_ingest::recordings`]). The same rules as
//! Knode Service's `recordings.*` methods, for when the app writes the space itself.

use knode_ingest::recordings as rec;

use crate::{KnodeSpace, Provenance, Result, parse_id};

macro_rules! mirror_enum {
    ($(#[$meta:meta])* $name:ident = $core:path { $($(#[$vmeta:meta])* $variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl From<$core> for $name {
            fn from(v: $core) -> Self {
                use $core as C;
                match v {
                    $(C::$variant => $name::$variant),+
                }
            }
        }

        impl From<$name> for $core {
            fn from(v: $name) -> Self {
                use $core as C;
                match v {
                    $($name::$variant => C::$variant),+
                }
            }
        }
    };
}

mirror_enum! {
    /// What a recording is.
    RecordingKind = rec::RecordingKind {
        /// A meeting recorded by the app, in person or online (node kind `meeting`).
        Meeting,
        /// A recording from Voice Memos (node kind `voice-memo`).
        VoiceMemo,
        /// An audio file added by hand (node kind `recording`).
        Recording,
    }
}

mirror_enum! {
    /// Where a recording came from.
    RecordingSource = rec::Source {
        /// Recorded by the app.
        Recorder,
        /// Brought in from Voice Memos.
        VoiceMemos,
        /// An audio file added by hand.
        File,
    }
}

mirror_enum! {
    /// Who an audio file of a recording holds.
    RecordingTrack = rec::Track {
        /// The person recording: "Me".
        Microphone,
        /// The others in an online meeting, as the Mac played them.
        System,
        /// Everyone in one file: a voice memo, or a meeting in person on one microphone.
        Mixed,
    }
}

mirror_enum! {
    /// How far the transcript of a recording is.
    TranscriptState = rec::TranscriptState {
        /// Still to be made.
        Pending,
        /// Written.
        Done,
        /// Making it failed; the recording's `error` says why.
        Failed,
    }
}

mirror_enum! {
    /// How far the summary of a recording is.
    SummaryState = rec::SummaryState {
        /// Still to be made, once the transcript is done.
        Pending,
        /// Written.
        Done,
        /// Not asked for.
        Off,
        /// This device cannot make one; the recording's `error` says why.
        Unavailable,
        /// Making it failed; the recording's `error` says why.
        Failed,
    }
}

mirror_enum! {
    /// A step that happens after a recording is stored.
    RecordingStage = rec::Stage {
        /// Transcribing it.
        Transcript,
        /// Summarising its transcript.
        Summary,
    }
}

mirror_enum! {
    /// How a stage of a recording did not succeed.
    RecordingFailure = rec::Failure {
        /// It was tried and failed.
        Failed,
        /// This device cannot do it (summaries only).
        Unavailable,
    }
}

/// An audio file to attach to a new recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AudioInput {
    /// The file on this machine. It is copied into the space.
    pub path: String,
    /// Who it holds.
    pub track: RecordingTrack,
}

/// An audio file attached to a recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AudioTrack {
    /// The attachment's name in the recording's node.
    pub name: String,
    /// Who it holds.
    pub track: RecordingTrack,
}

/// A recording to store.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NewRecording {
    /// What it is.
    pub kind: RecordingKind,
    /// Its title.
    pub title: String,
    /// When it started, in Unix milliseconds.
    pub started_at: i64,
    /// When it ended, in Unix milliseconds.
    pub ended_at: i64,
    /// The project node it belongs to.
    pub project: Option<String>,
    /// Its audio files.
    pub audio: Vec<AudioInput>,
    /// A key that finds it again, such as a voice memo's unique id.
    pub key: Option<String>,
    /// The meeting app, such as `zoom.us`.
    pub app: Option<String>,
    /// Whether to summarise it once it is transcribed (the space's `recordings.summaries`).
    pub summaries: bool,
    /// The name of the file its audio was taken from, when that file is not attached, such as a
    /// video larger than a space attaches.
    pub original: Option<String>,
}

/// A stored recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Recording {
    /// Its node.
    pub id: String,
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
    pub source: RecordingSource,
    /// The key that finds it again.
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
    pub project: Option<String>,
    /// Its audio files.
    pub tracks: Vec<AudioTrack>,
    /// The node of its transcript, once written.
    pub transcript_id: Option<String>,
    /// The file its audio was taken from, when that file is not attached.
    pub original: Option<String>,
}

/// What was said in a recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecordingTranscript {
    /// The language, BCP-47, such as `es-ES`.
    pub language: Option<String>,
    /// What made it, such as `Apple SpeechAnalyzer`.
    pub engine: String,
    /// What was said, in order.
    pub segments: Vec<TranscriptSegment>,
}

/// One stretch of speech.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TranscriptSegment {
    /// Where it starts in the recording, in milliseconds.
    pub start_ms: i64,
    /// Where it ends, in milliseconds.
    pub end_ms: i64,
    /// Who spoke: `Me`, `Others`, `Speaker 1`...
    pub speaker: String,
    /// What they said.
    pub text: String,
}

/// The minutes of a recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecordingSummary {
    /// What made it, such as `Apple Foundation Models`.
    pub model: String,
    /// What it was about.
    pub overview: String,
    /// The main points.
    pub key_points: Vec<String>,
    /// What was decided.
    pub decisions: Vec<String>,
    /// What is to be done.
    pub action_items: Vec<ActionItem>,
    /// What was left open.
    pub open_questions: Vec<String>,
}

/// Something to do after a recording.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ActionItem {
    /// What to do.
    pub task: String,
    /// Who does it.
    pub owner: Option<String>,
}

impl From<rec::Recording> for Recording {
    fn from(r: rec::Recording) -> Self {
        Recording {
            id: r.id.to_string(),
            kind: r.kind.into(),
            title: r.title,
            started_at: r.started_at,
            ended_at: r.ended_at,
            duration_ms: r.duration_ms,
            source: r.source.into(),
            key: r.key,
            language: r.language,
            transcript: r.transcript.into(),
            summary: r.summary.into(),
            error: r.error,
            app: r.app,
            project: r.project.map(|p| p.to_string()),
            tracks: r
                .tracks
                .into_iter()
                .map(|t| AudioTrack {
                    name: t.name,
                    track: t.track.into(),
                })
                .collect(),
            transcript_id: r.transcript_id.map(|t| t.to_string()),
            original: r.original,
        }
    }
}

impl From<RecordingTranscript> for rec::Transcript {
    fn from(t: RecordingTranscript) -> Self {
        rec::Transcript {
            language: t.language,
            engine: t.engine,
            segments: t
                .segments
                .into_iter()
                .map(|s| rec::Segment {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    speaker: s.speaker,
                    text: s.text,
                })
                .collect(),
        }
    }
}

impl From<RecordingSummary> for rec::Summary {
    fn from(s: RecordingSummary) -> Self {
        rec::Summary {
            model: s.model,
            overview: s.overview,
            key_points: s.key_points,
            decisions: s.decisions,
            action_items: s
                .action_items
                .into_iter()
                .map(|a| rec::ActionItem {
                    task: a.task,
                    owner: a.owner,
                })
                .collect(),
            open_questions: s.open_questions,
        }
    }
}

#[uniffi::export]
impl KnodeSpace {
    /// Stores a recording: its node, with its audio files copied in, its properties and a
    /// `part-of` link to its project. A recording with a `key` the space already has is returned
    /// unchanged.
    pub fn create_recording(&self, recording: NewRecording, by: Provenance) -> Result<Recording> {
        let new = rec::NewRecording {
            kind: recording.kind.into(),
            title: recording.title,
            started_at: recording.started_at,
            ended_at: recording.ended_at,
            project: recording.project.as_deref().map(parse_id).transpose()?,
            audio: recording
                .audio
                .into_iter()
                .map(|a| rec::AudioInput {
                    path: a.path.into(),
                    track: a.track.into(),
                })
                .collect(),
            key: recording.key,
            app: recording.app,
            summaries: recording.summaries,
            original: recording.original,
        };
        Ok(rec::create(&mut self.lock(), &new, &by.into())?.into())
    }

    /// The recordings whose transcript, or whose summary once transcribed, is still to be made,
    /// oldest first.
    pub fn pending_recordings(&self) -> Result<Vec<Recording>> {
        Ok(rec::pending(&self.lock())?
            .into_iter()
            .map(Recording::from)
            .collect())
    }

    /// The recordings whose transcript failed, or whose summary failed or is unavailable,
    /// newest first: those the app offers to try again.
    pub fn failed_recordings(&self) -> Result<Vec<Recording>> {
        Ok(rec::failed(&self.lock())?
            .into_iter()
            .map(Recording::from)
            .collect())
    }

    /// A recording, read from its node.
    pub fn recording(&self, id: String) -> Result<Recording> {
        let id = parse_id(&id)?;
        Ok(rec::get(&self.lock(), &id)?.into())
    }

    /// The recording whose file has this hash (`sha256:<hex>`), if any.
    pub fn recording_by_key(&self, key: String) -> Result<Option<Recording>> {
        let space = self.lock();
        Ok(match rec::find_by_key(&space, key.trim())? {
            Some(id) => Some(rec::get(&space, &id)?.into()),
            None => None,
        })
    }

    /// Writes the transcript of a recording, replacing the one written before.
    pub fn set_recording_transcript(
        &self,
        id: String,
        transcript: RecordingTranscript,
        by: Provenance,
    ) -> Result<Recording> {
        let id = parse_id(&id)?;
        Ok(rec::set_transcript(&mut self.lock(), &id, &transcript.into(), &by.into())?.into())
    }

    /// Writes the summary of a recording into its body, replacing the one written before.
    pub fn set_recording_summary(
        &self,
        id: String,
        summary: RecordingSummary,
        by: Provenance,
    ) -> Result<Recording> {
        let id = parse_id(&id)?;
        Ok(rec::set_summary(&mut self.lock(), &id, &summary.into(), &by.into())?.into())
    }

    /// Marks the transcript or the summary of a recording as failed, or the summary as
    /// unavailable, with why.
    pub fn fail_recording(
        &self,
        id: String,
        stage: RecordingStage,
        state: RecordingFailure,
        error: String,
        by: Provenance,
    ) -> Result<Recording> {
        let id = parse_id(&id)?;
        Ok(rec::set_failed(
            &mut self.lock(),
            &id,
            stage.into(),
            state.into(),
            &error,
            &by.into(),
        )?
        .into())
    }

    /// Puts the transcript or the summary of a recording back to pending. A summary is pending
    /// only once the transcript is done.
    pub fn retry_recording(
        &self,
        id: String,
        stage: RecordingStage,
        by: Provenance,
    ) -> Result<Recording> {
        let id = parse_id(&id)?;
        Ok(rec::retry(&mut self.lock(), &id, stage.into(), &by.into())?.into())
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

    #[test]
    fn records_transcribes_and_summarises() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Rec.knode").display().to_string();
        let space = KnodeSpace::create(path, "Rec".into()).unwrap();
        let me = by(ActorKind::Person, "Ada");
        let project = space
            .create_node("Launch".into(), String::new(), me.clone())
            .unwrap();
        // Projects are nodes of kind `project`.
        space
            .lock()
            .update_node(
                &parse_id(&project.id).unwrap(),
                knode_format::NodeUpdate {
                    kind: Some("project".into()),
                    ..Default::default()
                },
                &me.clone().into(),
            )
            .unwrap();
        let audio = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, name).unwrap();
            p.display().to_string()
        };
        let new = NewRecording {
            kind: RecordingKind::Meeting,
            title: "Sync".into(),
            started_at: 1_795_343_400_000,
            ended_at: 1_795_343_400_000 + 60_000,
            project: Some(project.id.clone()),
            audio: vec![
                AudioInput {
                    path: audio("microphone.m4a"),
                    track: RecordingTrack::Microphone,
                },
                AudioInput {
                    path: audio("system.m4a"),
                    track: RecordingTrack::System,
                },
            ],
            key: Some("k1".into()),
            app: None,
            summaries: true,
            original: None,
        };
        let created = space.create_recording(new.clone(), me.clone()).unwrap();
        assert_eq!(created.project.as_deref(), Some(project.id.as_str()));
        assert_eq!(created.source, RecordingSource::Recorder);
        assert_eq!(created.tracks[1].track, RecordingTrack::System);
        assert_eq!(space.create_recording(new, me.clone()).unwrap(), created);
        assert_eq!(space.pending_recordings().unwrap().len(), 1);

        let transcript = RecordingTranscript {
            language: Some("es-ES".into()),
            engine: "Apple SpeechAnalyzer".into(),
            segments: vec![TranscriptSegment {
                start_ms: 61_000,
                end_ms: 62_000,
                speaker: "Me".into(),
                text: "Hola".into(),
            }],
        };
        let speech = by(ActorKind::System, "apple-speech");
        let done = space
            .set_recording_transcript(created.id.clone(), transcript, speech)
            .unwrap();
        assert_eq!(done.transcript, TranscriptState::Done);
        let node = space
            .get_node(done.transcript_id.clone().unwrap())
            .unwrap()
            .unwrap();
        assert!(node.body.contains("**Me** (00:01:01): Hola"));

        let failed = space
            .fail_recording(
                created.id.clone(),
                RecordingStage::Summary,
                RecordingFailure::Unavailable,
                "Apple Intelligence is off".into(),
                me.clone(),
            )
            .unwrap();
        assert_eq!(failed.summary, SummaryState::Unavailable);
        let ids = |list: Vec<Recording>| list.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(space.failed_recordings().unwrap()),
            std::slice::from_ref(&created.id)
        );
        let retried = space
            .retry_recording(created.id.clone(), RecordingStage::Summary, me.clone())
            .unwrap();
        assert_eq!(retried.summary, SummaryState::Pending);
        assert_eq!(retried.error, None);
        assert!(space.failed_recordings().unwrap().is_empty());

        let summary = RecordingSummary {
            model: "Apple Foundation Models".into(),
            overview: "Short.".into(),
            key_points: vec![],
            decisions: vec!["Ship".into()],
            action_items: vec![],
            open_questions: vec![],
        };
        let model = by(ActorKind::System, "apple-foundation-models");
        let summarised = space
            .set_recording_summary(created.id.clone(), summary, model)
            .unwrap();
        assert_eq!(summarised.summary, SummaryState::Done);
        assert_eq!(space.recording(created.id.clone()).unwrap(), summarised);
        let body = space.get_node(created.id.clone()).unwrap().unwrap().body;
        assert!(body.contains("## Decisions\n\n- Ship"));
        assert!(!body.contains("## Key points"));
        assert!(space.pending_recordings().unwrap().is_empty());

        assert!(matches!(
            space.recording(project.id),
            Err(KnodeError::Invalid { .. })
        ));
    }
}

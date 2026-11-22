//! The shapes of the `recordings.*` methods: meetings recorded on the device, voice memos and
//! audio files, with their transcript and summary ([`knode_ingest::recordings`]).
//!
//! Agents may read recordings (`recordings.get`, `recordings.pending`, `recordings.failed`) with
//! `read`; storing
//! one names files on this machine, and transcripts and summaries are written by the app on the
//! device, so every write is for people only. `PROTOCOL.md` describes each method.

use std::path::PathBuf;

use knode_format::{NodeId, Provenance};
pub use knode_ingest::recordings::{
    ActionItem, AudioInput, AudioTrack, Failure, NewRecording, Recording, RecordingKind, Segment,
    Source as RecordingSource, Stage, Summary, SummaryState, Track, Transcript, TranscriptState,
};
use serde::{Deserialize, Serialize};

use crate::protocol::methods;

/// The methods an agent may call with `read` in a space. The others are for people only.
pub(crate) const READS: &[&str] = &[
    methods::RECORDINGS_GET,
    methods::RECORDINGS_PENDING,
    methods::RECORDINGS_FAILED,
];

/// `recordings.create`: a recording and the space to store it in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingCreateParams {
    /// The space.
    pub space: PathBuf,
    /// The recording; its audio paths are absolute.
    #[serde(flatten)]
    pub recording: NewRecording,
    /// Who stores it.
    pub by: Provenance,
}

/// `recordings.transcript`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingTranscriptParams {
    /// The space.
    pub space: PathBuf,
    /// The recording.
    pub id: NodeId,
    /// What was said.
    pub transcript: Transcript,
    /// Who wrote it, such as a system provenance naming the speech engine.
    pub by: Provenance,
}

/// `recordings.summary`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSummaryParams {
    /// The space.
    pub space: PathBuf,
    /// The recording.
    pub id: NodeId,
    /// The minutes.
    pub summary: Summary,
    /// Who wrote them, such as a system provenance naming the model.
    pub by: Provenance,
}

/// `recordings.fail`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingFailParams {
    /// The space.
    pub space: PathBuf,
    /// The recording.
    pub id: NodeId,
    /// `transcript` or `summary`.
    pub stage: Stage,
    /// `failed`, or `unavailable` (summaries only).
    pub state: Failure,
    /// Why, for people.
    #[serde(default)]
    pub error: String,
    /// Who says so.
    pub by: Provenance,
}

/// `recordings.retry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingRetryParams {
    /// The space.
    pub space: PathBuf,
    /// The recording.
    pub id: NodeId,
    /// `transcript` or `summary`.
    pub stage: Stage,
    /// Who asks.
    pub by: Provenance,
}

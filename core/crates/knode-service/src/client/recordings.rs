//! [`Client`] methods for `recordings.*`.

use std::path::Path;

use knode_format::{NodeId, Provenance};

use super::{Client, absolute, space_path};
use crate::Result;
use crate::protocol::{NodeParams, SpaceParams, methods};
use crate::recordings::*;

impl Client {
    /// Stores a recording, copying its audio files into the space; a recording with a `key` the
    /// space already has is returned unchanged. People only.
    pub fn create_recording(
        &self,
        space: impl AsRef<Path>,
        recording: &NewRecording,
        by: &Provenance,
    ) -> Result<Recording> {
        let mut recording = recording.clone();
        for audio in &mut recording.audio {
            audio.path = absolute(&audio.path)?;
        }
        let params = RecordingCreateParams {
            space: space_path(space.as_ref())?,
            recording,
            by: by.clone(),
        };
        self.request(methods::RECORDINGS_CREATE, params)
    }

    /// The recordings whose transcript, or whose summary once transcribed, is still to be made,
    /// oldest first.
    pub fn pending_recordings(&self, space: impl AsRef<Path>) -> Result<Vec<Recording>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::RECORDINGS_PENDING, params)
    }

    /// A recording.
    pub fn recording(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Recording> {
        let params = NodeParams {
            space: space_path(space.as_ref())?,
            id: *id,
        };
        self.request(methods::RECORDINGS_GET, params)
    }

    /// Writes the transcript of a recording, replacing the one written before. People only.
    pub fn set_recording_transcript(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        transcript: &Transcript,
        by: &Provenance,
    ) -> Result<Recording> {
        let params = RecordingTranscriptParams {
            space: space_path(space.as_ref())?,
            id: *id,
            transcript: transcript.clone(),
            by: by.clone(),
        };
        self.request(methods::RECORDINGS_TRANSCRIPT, params)
    }

    /// Writes the summary of a recording into its body, replacing the one written before.
    /// People only.
    pub fn set_recording_summary(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        summary: &Summary,
        by: &Provenance,
    ) -> Result<Recording> {
        let params = RecordingSummaryParams {
            space: space_path(space.as_ref())?,
            id: *id,
            summary: summary.clone(),
            by: by.clone(),
        };
        self.request(methods::RECORDINGS_SUMMARY, params)
    }

    /// Marks the transcript or the summary of a recording as failed or unavailable, with why.
    /// People only.
    pub fn fail_recording(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        stage: Stage,
        state: Failure,
        error: &str,
        by: &Provenance,
    ) -> Result<Recording> {
        let params = RecordingFailParams {
            space: space_path(space.as_ref())?,
            id: *id,
            stage,
            state,
            error: error.to_string(),
            by: by.clone(),
        };
        self.request(methods::RECORDINGS_FAIL, params)
    }

    /// Puts the transcript or the summary of a recording back to pending. People only.
    pub fn retry_recording(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        stage: Stage,
        by: &Provenance,
    ) -> Result<Recording> {
        let params = RecordingRetryParams {
            space: space_path(space.as_ref())?,
            id: *id,
            stage,
            by: by.clone(),
        };
        self.request(methods::RECORDINGS_RETRY, params)
    }
}

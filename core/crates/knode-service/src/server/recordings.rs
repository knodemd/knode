//! `recordings.*`: recordings, their transcripts and summaries. Blocking: called on a blocking
//! thread, like [`super::call`]. Agents only read them (see [`crate::recordings::READS`]).

use std::path::Path;

use knode_format::Space;
use knode_ingest::recordings;
use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::protocol::{ChangeKind, NodeParams, RpcError, SpaceParams, codes, methods};
use crate::recordings::*;

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::RECORDINGS_CREATE => create(state, raw),
        methods::RECORDINGS_PENDING => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(recordings::pending(&space)?)
        }
        methods::RECORDINGS_FAILED => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(recordings::failed(&space)?)
        }
        methods::RECORDINGS_GET => {
            let p: NodeParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(recordings::get(&space, &p.id)?)
        }
        methods::RECORDINGS_TRANSCRIPT => {
            let p: RecordingTranscriptParams = params(method, raw)?;
            write(state, &p.space, |space| {
                recordings::set_transcript(space, &p.id, &p.transcript, &p.by)
            })
        }
        methods::RECORDINGS_SUMMARY => {
            let p: RecordingSummaryParams = params(method, raw)?;
            write(state, &p.space, |space| {
                recordings::set_summary(space, &p.id, &p.summary, &p.by)
            })
        }
        methods::RECORDINGS_FAIL => {
            let p: RecordingFailParams = params(method, raw)?;
            write(state, &p.space, |space| {
                recordings::set_failed(space, &p.id, p.stage, p.state, &p.error, &p.by)
            })
        }
        methods::RECORDINGS_RETRY => {
            let p: RecordingRetryParams = params(method, raw)?;
            write(state, &p.space, |space| {
                recordings::retry(space, &p.id, p.stage, &p.by)
            })
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

/// Runs a write on one recording and announces what it changed. Properties are not part of the
/// history, so a write that changed only them announces the recording itself.
fn write(
    state: &State,
    space: &Path,
    run: impl FnOnce(&mut Space) -> knode_format::Result<Recording>,
) -> Result<Value, RpcError> {
    let space = state.space(space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = run(&mut space);
    state.notify_since(&space, mark)?;
    let recording = result?;
    if last_change(&space)? == mark {
        state.notify(&space, recording.id, ChangeKind::Update);
    }
    to_value(recording)
}

fn create(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: RecordingCreateParams = params(methods::RECORDINGS_CREATE, raw)?;
    if let Some(audio) = p.recording.audio.iter().find(|a| !a.path.is_absolute()) {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            audio.path.display()
        )));
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = recordings::create(&mut space, &p.recording, &p.by);
    // What was written before a failure is announced too.
    state.notify_since(&space, mark)?;
    to_value(result?)
}

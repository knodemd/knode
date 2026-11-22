//! `photos.*`: photos and what the device reads in them. Blocking: called on a blocking thread,
//! like [`super::call`]. Agents only read them (see [`crate::photos::READS`]).

use std::path::Path;

use knode_format::Space;
use knode_ingest::photos;
use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::photos::*;
use crate::protocol::{ChangeKind, NodeParams, RpcError, SpaceParams, codes, methods};

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::PHOTOS_CREATE => create(state, raw),
        methods::PHOTOS_PENDING => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(photos::pending(&space)?)
        }
        methods::PHOTOS_FAILED => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(photos::failed(&space)?)
        }
        methods::PHOTOS_GET => {
            let p: NodeParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(photos::get(&space, &p.id)?)
        }
        methods::PHOTOS_READING => {
            let p: PhotoReadingParams = params(method, raw)?;
            write(state, &p.space, |space| {
                photos::set_reading(space, &p.id, &p.reading, &p.by)
            })
        }
        methods::PHOTOS_FAIL => {
            let p: PhotoFailParams = params(method, raw)?;
            write(state, &p.space, |space| {
                photos::set_failed(space, &p.id, &p.error, &p.by)
            })
        }
        methods::PHOTOS_RETRY => {
            let p: PhotoRetryParams = params(method, raw)?;
            write(state, &p.space, |space| photos::retry(space, &p.id, &p.by))
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

/// Runs a write on one photo and announces what it changed. Properties are not part of the
/// history, so a write that changed only them announces the photo itself.
fn write(
    state: &State,
    space: &Path,
    run: impl FnOnce(&mut Space) -> knode_format::Result<Photo>,
) -> Result<Value, RpcError> {
    let space = state.space(space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = run(&mut space);
    state.notify_since(&space, mark)?;
    let photo = result?;
    if last_change(&space)? == mark {
        state.notify(&space, photo.id, ChangeKind::Update);
    }
    to_value(photo)
}

fn create(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: PhotoCreateParams = params(methods::PHOTOS_CREATE, raw)?;
    if !p.photo.path.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            p.photo.path.display()
        )));
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = photos::create(&mut space, &p.photo, &p.by);
    // What was written before a failure is announced too.
    state.notify_since(&space, mark)?;
    to_value(result?)
}

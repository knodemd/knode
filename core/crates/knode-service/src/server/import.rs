//! `import.run`. Blocking: called on a blocking thread, like [`super::call`].

use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::import::{ImportRunParams, prepare, run as import};
use crate::protocol::{RpcError, methods};

pub(super) fn run(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ImportRunParams = params(methods::IMPORT_RUN, raw)?;
    if let Some(path) = p.path.as_deref().filter(|p| !p.is_absolute()) {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            path.display()
        )));
    }
    // Tell what it is before the space is locked: looking into a large export takes a while.
    let mut what = prepare(p.path.as_deref(), p.url.as_deref(), p.from)?;
    if what.needs_readwise_token() {
        let token = match p.token {
            Some(token) => Some(token.0),
            None => crate::readwise::token(&state.auth.home).map_err(|e| {
                RpcError::new(
                    crate::protocol::codes::INTERNAL_ERROR,
                    format!("the Readwise token could not be read: {e}"),
                )
            })?,
        };
        what = what.with_readwise_token(token);
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let result = import(&mut space, &what, &p.by);
    // What was written before a failure is announced too.
    state.notify_since(&space, mark)?;
    to_value(result?)
}

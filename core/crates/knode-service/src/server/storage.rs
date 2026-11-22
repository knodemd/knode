//! `storage.*`: how much room a space takes, what fills it, and its limits
//! ([`knode_ingest::storage`]). Blocking: called on a blocking thread, like [`super::call`].
//! People only. Sizes are computed here, on the device.

use std::path::PathBuf;

use knode_format::Provenance;
use knode_ingest::storage;
use serde::Deserialize;
use serde_json::Value;

use super::{State, lock_ignoring_poison, params, to_value};
use crate::protocol::{RpcError, codes, methods};
use crate::spaces::SpaceEventKind;

/// The most largest files `storage.summary` lists.
const MAX_TOP: usize = 200;

#[derive(Debug, Deserialize)]
struct SummaryParams {
    space: PathBuf,
    /// How many of the largest files to list.
    #[serde(default)]
    top: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CheckParams {
    space: PathBuf,
    /// The bytes about to be added.
    size: u64,
}

#[derive(Debug, Deserialize)]
struct ConfigureParams {
    space: PathBuf,
    /// The soft limit in bytes; `0` never warns.
    #[serde(default)]
    warn_at: Option<u64>,
    #[serde(default)]
    keep_large_originals: Option<bool>,
    by: Provenance,
}

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::STORAGE_SUMMARY => {
            let p: SummaryParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            let top = p.top.unwrap_or(storage::DEFAULT_TOP).min(MAX_TOP);
            to_value(storage::summary(&space, top)?)
        }
        methods::STORAGE_CHECK => {
            let p: CheckParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(storage::check(&space, p.size)?)
        }
        methods::STORAGE_CONFIGURE => {
            let p: ConfigureParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let before = storage::limits(&space)?;
            let after = storage::set_limits(&mut space, p.warn_at, p.keep_large_originals, &p.by)?;
            if after.warn_at != before.warn_at {
                state.notify_space(&space, SpaceEventKind::Settings, |e| {
                    e.key = Some(storage::WARN_AT_SETTING.to_string());
                });
            }
            if after.keep_large_originals != before.keep_large_originals {
                state.notify_space(&space, SpaceEventKind::Settings, |e| {
                    e.key = Some(knode_ingest::media::KEEP_LARGE_ORIGINALS_SETTING.to_string());
                });
            }
            to_value(after)
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

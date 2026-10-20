//! Editing node bodies together: the `doc.*` methods and bodies written by `node.update`, merged
//! through the nodes' Loro documents (section 21 of the
//! [.knode specification](https://github.com/knodemd/format), with `knode-sync`).
//!
//! The service is the one writer of its spaces, so it keeps one [`Docs`] for all of them, locked
//! after a space and never before one. Each write is merged into the node's document, written to
//! SQLite as its own change, and announced with `changed`, carrying the document's new version.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use knode_format::{Node, NodeId, NodeUpdate, Provenance, Space};
use knode_sync::{BodyEdit, Docs, Splice, Version, Written};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::protocol::*;
use crate::server::{State, lock_ignoring_poison, params, to_value};

#[derive(Debug, Deserialize)]
struct DocParams {
    space: PathBuf,
    id: NodeId,
}

#[derive(Debug, Deserialize)]
struct DocEditParams {
    space: PathBuf,
    id: NodeId,
    /// The version the editor's text is at. Left out, the edit applies to the current text.
    #[serde(default)]
    version: Option<String>,
    /// Splices made in order, as an editor makes them.
    #[serde(default)]
    splices: Option<Vec<DocSplice>>,
    /// Or the new full text, diffed from the text of `version`.
    #[serde(default)]
    text: Option<String>,
    by: Provenance,
}

#[derive(Debug, Deserialize)]
struct DocPeerParams {
    space: PathBuf,
    by: Provenance,
}

#[derive(Debug, Deserialize)]
struct DocMergeParams {
    space: PathBuf,
    id: NodeId,
    /// Loro updates, in base64.
    update: String,
}

#[derive(Debug, Deserialize)]
struct DocUpdatesParams {
    space: PathBuf,
    id: NodeId,
    /// The version the editor has; left out, the whole document.
    #[serde(default)]
    since: Option<String>,
}

/// Runs one of the `doc.*` methods. Blocking: called on a blocking thread.
pub(crate) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::DOC_OPEN => {
            let p: DocParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let mut docs = state.docs();
            let open = docs.open_doc(&mut space, &p.id)?;
            to_value(DocText {
                text: open.text,
                version: open.version.to_json(),
            })
        }
        methods::DOC_EDIT => {
            let p: DocEditParams = params(method, raw)?;
            let edit = match (p.splices, p.text) {
                (Some(splices), None) => BodyEdit::splices(
                    splices
                        .into_iter()
                        .map(|s| Splice {
                            at: s.at,
                            delete: s.delete,
                            insert: s.insert,
                        })
                        .collect(),
                ),
                (None, Some(text)) => BodyEdit::text(text),
                _ => {
                    return Err(RpcError::invalid_params(
                        "doc.edit takes either splices or text",
                    ));
                }
            };
            let edit = match p.version.as_deref() {
                Some(version) => edit.based_on(version_param(version)?),
                None => edit,
            };
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let written = state.docs().apply_local_edit(&mut space, &p.id, edit, &p.by)?;
            announce(state, &space, &p.id, &written, &p.by);
            let version = written.version.to_json();
            let seen = written.seen.to_json();
            to_value(DocEdited {
                text: (version != seen).then(|| written.node.body.clone()),
                version,
                seen,
            })
        }
        methods::DOC_PEER => {
            let p: DocPeerParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let peer = state.docs().new_peer(&mut space, &p.by)?;
            // A string: peer ids use all 64 bits, more than many JSON readers keep.
            Ok(json!({ "peer": peer.to_string() }))
        }
        methods::DOC_MERGE => {
            let p: DocMergeParams = params(method, raw)?;
            let bytes = BASE64
                .decode(p.update.as_bytes())
                .map_err(|e| RpcError::invalid_params(format!("update is not base64: {e}")))?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let written = state.docs().merge_updates(&mut space, &p.id, &bytes)?;
            let by = written.node.updated_by.clone();
            announce(state, &space, &p.id, &written, &by);
            Ok(json!({ "version": written.version.to_json() }))
        }
        methods::DOC_UPDATES => {
            let p: DocUpdatesParams = params(method, raw)?;
            let since = match p.since.as_deref() {
                Some(since) => version_param(since)?,
                None => Version::empty(),
            };
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let mut docs = state.docs();
            let update = docs.export_updates_since(&mut space, &p.id, &since)?;
            let version = docs.open_doc(&mut space, &p.id)?.version;
            Ok(json!({ "update": BASE64.encode(update), "version": version.to_json() }))
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

/// What `node.update` wrote to the body through its document, if it did.
pub(crate) struct BodyWrite {
    pub version: String,
    pub seen: String,
}

/// `node.update`: the title, kind and tags are written as before; a new body is written through
/// the node's document when the caller sent the `version` it read, or when the node already has
/// a document (21.4), and as before otherwise. Announces the body write itself; the caller
/// announces the rest.
pub(crate) fn update_node(
    state: &State,
    space: &mut Space,
    id: &NodeId,
    mut update: NodeUpdate,
    version: Option<&str>,
    by: &Provenance,
) -> Result<(Node, Option<BodyWrite>), RpcError> {
    let base = version.map(version_param).transpose()?;
    let through_loro = update.body.is_some() && (base.is_some() || Docs::has_doc(space, id));
    if !through_loro {
        return Ok((space.update_node(id, update, by)?, None));
    }
    let body = update.body.take().unwrap_or_default();
    // The short fields first: a bad title fails before the body is written.
    space.update_node(id, update, by)?;
    let edit = match base {
        Some(base) => BodyEdit::text(body).based_on(base),
        None => BodyEdit::text(body),
    };
    let written = state.docs().apply_local_edit(space, id, edit, by)?;
    announce(state, space, id, &written, by);
    let write = BodyWrite {
        version: written.version.to_json(),
        seen: written.seen.to_json(),
    };
    Ok((written.node, Some(write)))
}

/// After a node is purged: removes its document, files and all (21.6).
pub(crate) fn purged(state: &State, space: &Space, id: &NodeId) {
    if let Err(e) = state.docs().remove_doc(space, id) {
        eprintln!(
            "Knode Service: cannot remove the document of {id} in {}: {e}",
            space.path().display()
        );
    }
}

/// Announces a body written through its document, with its new version, who wrote it and the
/// Loro updates. A write that changed nothing announces nothing.
fn announce(state: &State, space: &Space, id: &NodeId, written: &Written, by: &Provenance) {
    if written.update.is_empty() {
        return;
    }
    let mut change = Change::new(space.path().to_path_buf(), *id, ChangeKind::Update);
    change.version = Some(written.version.to_json());
    change.by = Some(by.clone());
    change.update = Some(BASE64.encode(&written.update));
    state.send_change(change);
}

fn version_param(text: &str) -> Result<Version, RpcError> {
    Version::from_json(text).map_err(|_| {
        RpcError::invalid_params(format!(
            "version is not a document version, as doc.open returns it: {text}"
        ))
    })
}

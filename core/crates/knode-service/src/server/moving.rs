//! `project.move` and `project.move.undo`: moving a project between two spaces, which holds both
//! spaces at once. Blocking: called on a blocking thread, like [`super::call`].

use std::sync::{Arc, Mutex, MutexGuard};

use knode_format::{NodeId, Space};
use serde_json::Value;

use super::{State, lock_ignoring_poison, params, to_value};
use crate::protocol::{ChangeKind, ProjectMoveParams, ProjectMoveUndoParams, RpcError, methods};
use crate::spaces::SpaceEventKind;

/// Locks two spaces, always in the same order, so two moves between the same spaces in opposite
/// directions never wait for each other. Fails when they are the same space.
fn lock_both<'a>(
    first: &'a Arc<Mutex<Space>>,
    second: &'a Arc<Mutex<Space>>,
) -> Result<(MutexGuard<'a, Space>, MutexGuard<'a, Space>), RpcError> {
    if Arc::ptr_eq(first, second) {
        return Err(RpcError::invalid_params(
            "the project is already in that space; give another space",
        ));
    }
    if Arc::as_ptr(first) < Arc::as_ptr(second) {
        let a = lock_ignoring_poison(first);
        let b = lock_ignoring_poison(second);
        Ok((a, b))
    } else {
        let b = lock_ignoring_poison(second);
        let a = lock_ignoring_poison(first);
        Ok((a, b))
    }
}

/// Announces nodes that left a space and arrived in another, and that both spaces' folders
/// changed, so capture follows the folders that moved.
fn announce(state: &State, gone_from: &Space, now_in: &Space, nodes: &[NodeId], folders: bool) {
    for id in nodes {
        state.notify(gone_from, *id, ChangeKind::Delete);
        state.notify(now_in, *id, ChangeKind::Create);
    }
    if folders {
        for space in [gone_from, now_in] {
            state.notify_space(space, SpaceEventKind::Folders, |_| {});
        }
    }
}

pub(super) fn move_project(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ProjectMoveParams = params(methods::PROJECT_MOVE, raw)?;
    let source = state.space(&p.space)?;
    let target = state.space(&p.to)?;
    let (mut source, mut target) = lock_both(&source, &target)?;
    if p.dry_run {
        return to_value(source.preview_move(&p.project, &target)?);
    }
    let moved = source.move_project(&p.project, &mut target, &p.by)?;
    let mut nodes: Vec<NodeId> = moved.preview.projects.iter().map(|n| n.id).collect();
    if let Some(change) = source.change(moved.from_change)? {
        nodes = change
            .versions
            .iter()
            .map(|v| v.node_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    announce(
        state,
        &source,
        &target,
        &nodes,
        !moved.preview.folders.is_empty(),
    );
    to_value(moved)
}

pub(super) fn undo_move(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ProjectMoveUndoParams = params(methods::PROJECT_MOVE_UNDO, raw)?;
    let here = state.space(&p.space)?;
    let other = state.space(&p.other)?;
    let (mut here, mut other) = lock_both(&here, &other)?;
    let undone = here.undo_move(p.change, &mut other, &p.by)?;
    let (back_in, left) = if here.manifest().id == undone.from.id {
        (&*here, &*other)
    } else {
        (&*other, &*here)
    };
    for id in &undone.nodes {
        crate::coedit::purged(state, left, id);
    }
    announce(state, left, back_in, &undone.nodes, true);
    to_value(undone)
}

//! The methods for link kinds, labelled links, views, canvases and attachments, and
//! `conversations.import`. Blocking: called on a blocking thread, like [`super::call`].

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use knode_format::{GraphFilter, NewNode, Space, View, view_kinds};
use serde_json::Value;

use super::{State, last_change, lock_ignoring_poison, params, to_value};
use crate::maps::*;
use crate::protocol::{ChangeKind, NodeParams, RpcError, SpaceParams, methods};
use crate::spaces::SpaceEventKind;

/// Runs one of these methods, or returns `None` when `method` is none of them.
pub(super) fn call(state: &State, method: &str, raw: Value) -> Option<Result<Value, RpcError>> {
    let result = match method {
        methods::LINK_KINDS_LIST => link_kinds(state, raw),
        methods::LINK_KIND_SET => set_link_kind(state, raw),
        methods::LINK_KIND_REMOVE => remove_link_kind(state, raw),
        methods::LINKS_OF | methods::LINKS_BACK_LABELED => node_links(state, method, raw),
        methods::VIEWS_LIST => views(state, raw),
        methods::VIEW_GET => view(state, raw),
        methods::VIEW_SET => set_view(state, raw),
        methods::VIEW_REMOVE => remove_view(state, raw),
        methods::VIEW_VIEWPORT_SET => set_viewport(state, raw),
        methods::GRAPH => graph(state, raw),
        methods::CANVAS_CREATE => create_canvas(state, raw),
        methods::CANVAS_BOARD => canvas_board(state, raw),
        methods::POSITIONS_LIST => positions(state, raw),
        methods::POSITIONS_SET => set_positions(state, raw),
        methods::POSITIONS_REMOVE => remove_positions(state, raw),
        methods::GROUPS_LIST => groups(state, raw),
        methods::GROUP_SET => set_group(state, raw),
        methods::GROUP_REMOVE => remove_group(state, raw),
        methods::ATTACHMENTS_LIST => attachments(state, raw),
        methods::ATTACHMENT_READ => read_attachment(state, raw),
        methods::ATTACHMENT_ADD => attach(state, raw),
        methods::ATTACHMENT_ADD_FILE => attach_file(state, raw),
        methods::ATTACHMENT_REMOVE => detach(state, raw),
        methods::ATTACHMENT_PATH => attachment_path(state, raw),
        methods::CONVERSATIONS_IMPORT => import_conversations(state, raw),
        _ => return None,
    };
    Some(result)
}

/// Announces that a kind of link was defined, changed or removed.
fn kinds_changed(state: &State, space: &Space, kind: &str) {
    state.notify_space(space, SpaceEventKind::LinkKinds, |e| {
        e.link_kind = Some(kind.to_string());
    });
}

/// Announces that a view changed, or what is on it.
fn view_changed(state: &State, space: &Space, view: &str) {
    state.notify_space(space, SpaceEventKind::Views, |e| {
        e.view = Some(view.to_string());
    });
}

/// A view as the methods return it: a canvas is named by its node's title.
fn named(space: &Space, mut view: View) -> Result<View, RpcError> {
    if view.kind == view_kinds::CANVAS
        && let Some(node) = view.node_id
        && let Some(found) = space.get_node(&node)?
    {
        view.name = found.title;
    }
    Ok(view)
}

fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::from(knode_format::Error::Invalid(message.into()))
}

fn link_kinds(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: SpaceParams = params(methods::LINK_KINDS_LIST, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    to_value(space.link_kinds()?)
}

fn set_link_kind(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: LinkKindSetParams = params(methods::LINK_KIND_SET, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let kind = space.set_link_kind(&p.spec, &p.by)?;
    if last_change(&space)? != mark {
        kinds_changed(state, &space, &kind.kind);
    }
    to_value(kind)
}

fn remove_link_kind(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: LinkKindRemoveParams = params(methods::LINK_KIND_REMOVE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let removed = space.remove_link_kind(&p.kind, &p.by)?;
    if removed {
        kinds_changed(state, &space, p.kind.trim());
    }
    to_value(removed)
}

fn node_links(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    let p: NodeParams = params(method, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    if method == methods::LINKS_OF {
        to_value(space.node_links(&p.id)?)
    } else {
        to_value(space.labeled_backlinks(&p.id)?)
    }
}

fn views(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: SpaceParams = params(methods::VIEWS_LIST, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    let views = space
        .views()?
        .into_iter()
        .map(|v| named(&space, v))
        .collect::<Result<Vec<_>, _>>()?;
    to_value(views)
}

fn view(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ViewParams = params(methods::VIEW_GET, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    let view = space.view(&p.id)?.map(|v| named(&space, v)).transpose()?;
    to_value(view)
}

fn set_view(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ViewSetParams = params(methods::VIEW_SET, raw)?;
    if let Some(filter) = &p.filter {
        GraphFilter::from_json(filter)?;
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let spec = knode_format::GraphViewSpec {
        id: p.id,
        name: p.name,
        filter: p.filter,
    };
    let view = space.set_graph_view(&spec, &p.by)?;
    if last_change(&space)? != mark {
        view_changed(state, &space, &view.id);
    }
    to_value(view)
}

fn remove_view(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ViewRemoveParams = params(methods::VIEW_REMOVE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let removed = space.remove_view(&p.id, &p.by)?;
    if removed {
        view_changed(state, &space, &p.id);
    }
    to_value(removed)
}

/// Saves the viewport. Not announced: it is where one reader looks, not what the view holds.
fn set_viewport(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ViewportSetParams = params(methods::VIEW_VIEWPORT_SET, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    space.set_viewport(&p.id, &p.viewport, &p.by)?;
    Ok(Value::Null)
}

fn graph(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: GraphParams = params(methods::GRAPH, raw)?;
    let filter = p.filter.as_ref().map(GraphFilter::from_json).transpose()?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    to_value(space.graph(p.view.as_deref(), filter.as_ref())?)
}

fn create_canvas(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: CanvasCreateParams = params(methods::CANVAS_CREATE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let new = NewNode::note(p.title, p.body).with_tags(p.tags);
    let view = space.create_canvas(new, &p.by)?;
    if let Some(node) = view.node_id {
        state.notify(&space, node, ChangeKind::Create);
    }
    view_changed(state, &space, &view.id);
    to_value(named(&space, view)?)
}

fn canvas_board(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: NodeParams = params(methods::CANVAS_BOARD, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    let board = space.canvas_board(&p.id)?.map(|mut board| {
        board.view.name = board.title.clone();
        board
    });
    to_value(board)
}

fn positions(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: InViewParams = params(methods::POSITIONS_LIST, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    to_value(space.positions(&p.view)?)
}

fn set_positions(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: PositionsSetParams = params(methods::POSITIONS_SET, raw)?;
    let specs = p
        .positions
        .iter()
        .map(PositionInput::to_spec)
        .collect::<knode_format::Result<Vec<_>>>()?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    space.set_positions(&p.view, &specs, &p.by)?;
    if !specs.is_empty() {
        view_changed(state, &space, &p.view);
    }
    Ok(Value::Null)
}

fn remove_positions(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: PositionsRemoveParams = params(methods::POSITIONS_REMOVE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let removed = space.remove_positions(&p.view, &p.nodes)?;
    if removed > 0 {
        view_changed(state, &space, &p.view);
    }
    to_value(removed)
}

fn groups(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: InViewParams = params(methods::GROUPS_LIST, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    to_value(space.groups(&p.view)?)
}

fn set_group(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: GroupSetParams = params(methods::GROUP_SET, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let group = space.set_group(&p.view, &p.group, &p.by)?;
    view_changed(state, &space, &p.view);
    to_value(group)
}

fn remove_group(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: GroupRemoveParams = params(methods::GROUP_REMOVE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    // A group is removed only from the canvas named, so a call cannot reach into another view.
    if !space.groups(&p.view)?.iter().any(|g| g.id == p.id) {
        return to_value(false);
    }
    let removed = space.remove_group(&p.id)?;
    if removed {
        view_changed(state, &space, &p.view);
    }
    to_value(removed)
}

fn attachments(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: NodeParams = params(methods::ATTACHMENTS_LIST, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    to_value(space.attachments(&p.id)?)
}

fn too_large(name: &str) -> RpcError {
    invalid(format!(
        "{name} is larger than {} MiB, the most an attachment travels over the protocol; a \
         person attaches it with attachment.add_file and opens it from attachment.path",
        MAX_INLINE_ATTACHMENT / (1024 * 1024)
    ))
}

fn read_attachment(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: AttachmentParams = params(methods::ATTACHMENT_READ, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    let Some(attachment) = space.attachment(&p.id, &p.name)? else {
        return Ok(Value::Null);
    };
    if attachment.size > MAX_INLINE_ATTACHMENT {
        return Err(too_large(&attachment.name));
    }
    let bytes = space.read_blob(&attachment.hash)?.ok_or_else(|| {
        invalid(format!(
            "the file of {} is not on this machine yet",
            attachment.name
        ))
    })?;
    to_value(AttachmentContent {
        attachment,
        content: BASE64.encode(bytes),
    })
}

fn attach(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: AttachmentAddParams = params(methods::ATTACHMENT_ADD, raw)?;
    // Four base64 characters hold three bytes: refuse before decoding what is too large.
    if (p.content.len() as u64) / 4 * 3 > MAX_INLINE_ATTACHMENT + 3 {
        return Err(too_large(&p.name));
    }
    let bytes = BASE64
        .decode(p.content.trim())
        .map_err(|e| RpcError::invalid_params(format!("content is not base64: {e}")))?;
    if bytes.len() as u64 > MAX_INLINE_ATTACHMENT {
        return Err(too_large(&p.name));
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let attachment = space.attach(&p.id, &p.name, bytes.as_slice(), &p.by)?;
    if last_change(&space)? != mark {
        state.notify(&space, p.id, ChangeKind::Update);
    }
    to_value(attachment)
}

fn attach_file(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: AttachmentAddFileParams = params(methods::ATTACHMENT_ADD_FILE, raw)?;
    if !p.path.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            p.path.display()
        )));
    }
    let metadata = std::fs::metadata(&p.path)
        .map_err(|e| invalid(format!("cannot read {}: {e}", p.path.display())))?;
    if !metadata.is_file() {
        return Err(invalid(format!("{} is not a file", p.path.display())));
    }
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let attachment = space.attach_file(&p.id, &p.path, &p.by)?;
    if last_change(&space)? != mark {
        state.notify(&space, p.id, ChangeKind::Update);
    }
    to_value(attachment)
}

fn detach(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: AttachmentRemoveParams = params(methods::ATTACHMENT_REMOVE, raw)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let removed = space.detach(&p.id, &p.name, &p.by)?;
    if removed {
        state.notify(&space, p.id, ChangeKind::Update);
    }
    to_value(removed)
}

fn attachment_path(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: AttachmentParams = params(methods::ATTACHMENT_PATH, raw)?;
    let space = state.space(&p.space)?;
    let space = lock_ignoring_poison(&space);
    let Some(attachment) = space.attachment(&p.id, &p.name)? else {
        return Ok(Value::Null);
    };
    to_value(space.blob_path(&attachment.hash)?)
}

fn import_conversations(state: &State, raw: Value) -> Result<Value, RpcError> {
    let p: ConversationsImportParams = params(methods::CONVERSATIONS_IMPORT, raw)?;
    if !p.path.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "path must be an absolute path: {}",
            p.path.display()
        )));
    }
    // Tell what the export is before the space is locked: reading a large zip takes a while.
    knode_ingest::detect_export(&p.path).map_err(import_error)?;
    let space = state.space(&p.space)?;
    let mut space = lock_ignoring_poison(&space);
    let mark = last_change(&space)?;
    let report = knode_ingest::import_export(&mut space, &p.path, &p.by);
    // What was written before a failure is announced too.
    state.notify_since(&space, mark)?;
    to_value(ConversationImport::of(&report.map_err(import_error)?))
}

fn import_error(error: knode_ingest::ImportError) -> RpcError {
    match error {
        knode_ingest::ImportError::Space(error) => error.into(),
        knode_ingest::ImportError::UnknownFormat(_) => invalid(error.to_string()),
        other => {
            RpcError::new(crate::protocol::codes::FOLDER, other.to_string()).with_kind("folder")
        }
    }
}

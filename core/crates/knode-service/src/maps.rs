//! The shapes of the methods for link kinds, labelled links, views, canvases and attachments
//! (schema version 4), and of `conversations.import`.
//!
//! The methods mirror [`knode_format::Space`]; `PROTOCOL.md` describes each one.

use std::path::PathBuf;

use knode_format::{
    Attachment, GroupSpec, LinkKindSpec, NodeId, PositionSpec, Provenance, ViewDisplay, Viewport,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::methods;

/// The largest attachment `attachment.add` takes and `attachment.read` returns, in bytes: 10 MiB.
/// The content travels as base64 on one line of the protocol. Larger files are attached by a
/// person with `attachment.add_file` and read from `attachment.path`.
pub const MAX_INLINE_ATTACHMENT: u64 = 10 * 1024 * 1024;

/// The methods an agent may call with `read` in a space.
pub(crate) const READS: &[&str] = &[
    methods::LINK_KINDS_LIST,
    methods::LINKS_OF,
    methods::LINKS_BACK_LABELED,
    methods::VIEWS_LIST,
    methods::VIEW_GET,
    methods::GRAPH,
    methods::CANVAS_BOARD,
    methods::POSITIONS_LIST,
    methods::GROUPS_LIST,
    methods::ATTACHMENTS_LIST,
    methods::ATTACHMENT_READ,
];

/// The methods an agent may call with `write` in a space. `attachment.add_file`,
/// `attachment.path` and `conversations.import` name files on this machine: people only.
pub(crate) const WRITES: &[&str] = &[
    methods::LINK_KIND_SET,
    methods::LINK_KIND_REMOVE,
    methods::VIEW_SET,
    methods::VIEW_REMOVE,
    methods::VIEW_VIEWPORT_SET,
    methods::CANVAS_CREATE,
    methods::POSITIONS_SET,
    methods::POSITIONS_REMOVE,
    methods::GROUP_SET,
    methods::GROUP_REMOVE,
    methods::ATTACHMENT_ADD,
    methods::ATTACHMENT_REMOVE,
];

/// Where to put a node in a view, as `positions.set` takes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionInput {
    /// The node.
    pub node_id: NodeId,
    /// The centre, in points; `x` grows to the right.
    pub x: f64,
    /// The centre, in points; `y` grows down.
    pub y: f64,
    /// The width of the node's box; set with `height`, or neither for the default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// The height of the node's box; set with `width`, or neither for the default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Stacking order on a canvas: higher is drawn in front.
    #[serde(default)]
    pub z: i64,
    /// `open` (the default) or `collapsed`.
    #[serde(default)]
    pub display: ViewDisplay,
    /// The group to put it in, on a canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

impl PositionInput {
    /// A node at a point, with the default size, open and outside any group.
    pub fn at(node_id: NodeId, x: f64, y: f64) -> Self {
        PositionInput::from(PositionSpec::at(node_id, x, y))
    }

    pub(crate) fn to_spec(&self) -> knode_format::Result<PositionSpec> {
        let size = match (self.width, self.height) {
            (Some(w), Some(h)) => Some((w, h)),
            (None, None) => None,
            _ => {
                return Err(knode_format::Error::Invalid(
                    "a node's box has both a width and a height, or neither".into(),
                ));
            }
        };
        Ok(PositionSpec {
            node_id: self.node_id,
            x: self.x,
            y: self.y,
            size,
            z: self.z,
            display: self.display,
            group_id: self.group_id.clone(),
        })
    }
}

impl From<PositionSpec> for PositionInput {
    fn from(spec: PositionSpec) -> Self {
        PositionInput {
            node_id: spec.node_id,
            x: spec.x,
            y: spec.y,
            width: spec.size.map(|s| s.0),
            height: spec.size.map(|s| s.1),
            z: spec.z,
            display: spec.display,
            group_id: spec.group_id,
        }
    }
}

/// An attachment with its content: the result of `attachment.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentContent {
    /// The attachment.
    pub attachment: Attachment,
    /// Its bytes, in base64 (RFC 4648, with padding).
    pub content: String,
}

/// Something in a data export that was not imported, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    /// What it was: an id, or the start of a prompt.
    pub item: String,
    /// Why it was skipped.
    pub reason: String,
}

/// What `conversations.import` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationImport {
    /// The kind of export, for people, such as `Claude export`.
    pub format: String,
    /// The agent whose conversations it holds: `claude-ai`, `chatgpt` or `gemini-web`.
    pub agent: String,
    /// Conversations written as new nodes.
    pub created: usize,
    /// Conversations whose node changed.
    pub updated: usize,
    /// Projects written or changed (Claude projects).
    pub projects: usize,
    /// Conversations and projects whose node was already up to date.
    pub unchanged: usize,
    /// Conversations with no messages; no node is written for them.
    pub empty: usize,
    /// What could not be imported; the rest was.
    pub skipped: Vec<SkippedItem>,
}

impl From<&knode_ingest::ImportReport> for ConversationImport {
    fn from(report: &knode_ingest::ImportReport) -> Self {
        ConversationImport {
            format: report.format.to_string(),
            agent: report.format.agent().id().to_string(),
            created: report.created().count(),
            updated: report.updated().count(),
            projects: report.projects.len(),
            unchanged: report.unchanged,
            empty: report.empty,
            skipped: report
                .skipped
                .iter()
                .map(|s| SkippedItem {
                    item: s.item.clone(),
                    reason: s.reason.clone(),
                })
                .collect(),
        }
    }
}

// Params, shared by the client and the server.

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LinkKindSetParams {
    pub space: PathBuf,
    #[serde(flatten)]
    pub spec: LinkKindSpec,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LinkKindRemoveParams {
    pub space: PathBuf,
    pub kind: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ViewParams {
    pub space: PathBuf,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ViewSetParams {
    pub space: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ViewRemoveParams {
    pub space: PathBuf,
    pub id: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ViewportSetParams {
    pub space: PathBuf,
    pub id: String,
    #[serde(flatten)]
    pub viewport: Viewport,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GraphParams {
    pub space: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CanvasCreateParams {
    pub space: PathBuf,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct InViewParams {
    pub space: PathBuf,
    pub view: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PositionsSetParams {
    pub space: PathBuf,
    pub view: String,
    pub positions: Vec<PositionInput>,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PositionsRemoveParams {
    pub space: PathBuf,
    pub view: String,
    pub nodes: Vec<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GroupSetParams {
    pub space: PathBuf,
    pub view: String,
    #[serde(flatten)]
    pub group: GroupSpec,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GroupRemoveParams {
    pub space: PathBuf,
    pub view: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Provenance>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AttachmentParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AttachmentAddParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub name: String,
    pub content: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AttachmentAddFileParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub path: PathBuf,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AttachmentRemoveParams {
    pub space: PathBuf,
    pub id: NodeId,
    pub name: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ConversationsImportParams {
    pub space: PathBuf,
    pub path: PathBuf,
    pub by: Provenance,
}

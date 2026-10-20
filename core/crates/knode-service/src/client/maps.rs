//! [`Client`] methods for link kinds, labelled links, views, canvases and attachments, and
//! `conversations.import`.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use knode_format::{
    Attachment, CanvasBoard, Graph, GraphFilter, GroupSpec, LinkKind, LinkKindSpec, NodeId,
    NodeLink, Position, Provenance, View, ViewGroup, Viewport,
};
use serde_json::Value;

use super::{Client, absolute, space_path};
use crate::maps::*;
use crate::protocol::{SpaceParams, methods};
use crate::{Error, Result};

impl Client {
    /// Every kind of link the space knows: the built-in kinds, then those the space defines.
    pub fn link_kinds(&self, space: impl AsRef<Path>) -> Result<Vec<LinkKind>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::LINK_KINDS_LIST, params)
    }

    /// Defines a kind of link, or changes its labels and description, and returns it.
    pub fn set_link_kind(
        &self,
        space: impl AsRef<Path>,
        spec: &LinkKindSpec,
        by: &Provenance,
    ) -> Result<LinkKind> {
        let params = LinkKindSetParams {
            space: space_path(space.as_ref())?,
            spec: spec.clone(),
            by: by.clone(),
        };
        self.request(methods::LINK_KIND_SET, params)
    }

    /// Removes a kind of link the space defined, while no link has it. Returns whether there
    /// was one.
    pub fn remove_link_kind(
        &self,
        space: impl AsRef<Path>,
        kind: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let params = LinkKindRemoveParams {
            space: space_path(space.as_ref())?,
            kind: kind.to_string(),
            by: by.clone(),
        };
        self.request(methods::LINK_KIND_REMOVE, params)
    }

    /// The links of a node, going out then coming in, each with the label it reads with from
    /// the node.
    pub fn node_links(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<NodeLink>> {
        self.request(methods::LINKS_OF, self.node_params(space.as_ref(), id)?)
    }

    /// The backlinks of a node, of every kind, each with its inverse label.
    pub fn labeled_backlinks(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<NodeLink>> {
        self.request(
            methods::LINKS_BACK_LABELED,
            self.node_params(space.as_ref(), id)?,
        )
    }

    /// The graph views and canvases of the space, oldest first. A canvas is named by its
    /// node's title.
    pub fn views(&self, space: impl AsRef<Path>) -> Result<Vec<View>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::VIEWS_LIST, params)
    }

    /// A view by id, or `None`.
    pub fn view(&self, space: impl AsRef<Path>, id: &str) -> Result<Option<View>> {
        let params = ViewParams {
            space: space_path(space.as_ref())?,
            id: id.to_string(),
        };
        self.request(methods::VIEW_GET, params)
    }

    /// Creates a graph view (`id` `None`), or renames or filters it, and returns it. `filter`
    /// is the JSON object a graph view stores (see [`GraphFilter::from_json`]).
    pub fn set_graph_view(
        &self,
        space: impl AsRef<Path>,
        id: Option<&str>,
        name: &str,
        filter: Option<Value>,
        by: &Provenance,
    ) -> Result<View> {
        let params = ViewSetParams {
            space: space_path(space.as_ref())?,
            id: id.map(str::to_string),
            name: name.to_string(),
            filter,
            by: by.clone(),
        };
        self.request(methods::VIEW_SET, params)
    }

    /// Removes a graph view with its positions. Returns whether there was one.
    pub fn remove_view(&self, space: impl AsRef<Path>, id: &str, by: &Provenance) -> Result<bool> {
        let params = ViewRemoveParams {
            space: space_path(space.as_ref())?,
            id: id.to_string(),
            by: by.clone(),
        };
        self.request(methods::VIEW_REMOVE, params)
    }

    /// Saves the part of a view's plane on screen.
    pub fn set_viewport(
        &self,
        space: impl AsRef<Path>,
        id: &str,
        viewport: Viewport,
        by: &Provenance,
    ) -> Result<()> {
        let params = ViewportSetParams {
            space: space_path(space.as_ref())?,
            id: id.to_string(),
            viewport,
            by: by.clone(),
        };
        let _: Value = self.request(methods::VIEW_VIEWPORT_SET, params)?;
        Ok(())
    }

    /// The nodes a filter keeps and the links between them, with the positions saved in
    /// `view`. Without a filter the view's own applies; with neither, the whole space.
    pub fn graph(
        &self,
        space: impl AsRef<Path>,
        view: Option<&str>,
        filter: Option<&GraphFilter>,
    ) -> Result<Graph> {
        let params = GraphParams {
            space: space_path(space.as_ref())?,
            view: view.map(str::to_string),
            filter: filter.map(GraphFilter::to_json),
        };
        self.request(methods::GRAPH, params)
    }

    /// Creates a canvas, a node of kind `canvas` with this title and body, and returns its
    /// view; its `node_id` is the node.
    pub fn create_canvas(
        &self,
        space: impl AsRef<Path>,
        title: &str,
        body: &str,
        by: &Provenance,
    ) -> Result<View> {
        let params = CanvasCreateParams {
            space: space_path(space.as_ref())?,
            title: title.to_string(),
            body: body.to_string(),
            tags: Vec::new(),
            by: by.clone(),
        };
        self.request(methods::CANVAS_CREATE, params)
    }

    /// The canvas whose node is `node`, with its nodes, groups and links, or `None` when the
    /// node is not a canvas.
    pub fn canvas_board(
        &self,
        space: impl AsRef<Path>,
        node: &NodeId,
    ) -> Result<Option<CanvasBoard>> {
        self.request(
            methods::CANVAS_BOARD,
            self.node_params(space.as_ref(), node)?,
        )
    }

    /// The positions in a view of the nodes that exist, back to front.
    pub fn positions(&self, space: impl AsRef<Path>, view: &str) -> Result<Vec<Position>> {
        self.request(methods::POSITIONS_LIST, in_view(space.as_ref(), view)?)
    }

    /// Places nodes in a view in one write, adding or replacing their positions.
    pub fn set_positions(
        &self,
        space: impl AsRef<Path>,
        view: &str,
        positions: &[PositionInput],
        by: &Provenance,
    ) -> Result<()> {
        let params = PositionsSetParams {
            space: space_path(space.as_ref())?,
            view: view.to_string(),
            positions: positions.to_vec(),
            by: by.clone(),
        };
        let _: Value = self.request(methods::POSITIONS_SET, params)?;
        Ok(())
    }

    /// Takes nodes out of a view; the nodes are not deleted. Returns how many were in it.
    pub fn remove_positions(
        &self,
        space: impl AsRef<Path>,
        view: &str,
        nodes: &[NodeId],
        by: &Provenance,
    ) -> Result<usize> {
        let params = PositionsRemoveParams {
            space: space_path(space.as_ref())?,
            view: view.to_string(),
            nodes: nodes.to_vec(),
            by: Some(by.clone()),
        };
        self.request(methods::POSITIONS_REMOVE, params)
    }

    /// The groups of a canvas.
    pub fn groups(&self, space: impl AsRef<Path>, view: &str) -> Result<Vec<ViewGroup>> {
        self.request(methods::GROUPS_LIST, in_view(space.as_ref(), view)?)
    }

    /// Adds a group to a canvas, or changes the one with `group.id`, and returns it.
    pub fn set_group(
        &self,
        space: impl AsRef<Path>,
        view: &str,
        group: &GroupSpec,
        by: &Provenance,
    ) -> Result<ViewGroup> {
        let params = GroupSetParams {
            space: space_path(space.as_ref())?,
            view: view.to_string(),
            group: group.clone(),
            by: by.clone(),
        };
        self.request(methods::GROUP_SET, params)
    }

    /// Removes a group of a canvas; its nodes stay on the canvas. Returns whether there was
    /// one.
    pub fn remove_group(
        &self,
        space: impl AsRef<Path>,
        view: &str,
        id: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let params = GroupRemoveParams {
            space: space_path(space.as_ref())?,
            view: view.to_string(),
            id: id.to_string(),
            by: Some(by.clone()),
        };
        self.request(methods::GROUP_REMOVE, params)
    }

    /// The attachments of a node, oldest first.
    pub fn attachments(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<Attachment>> {
        self.request(
            methods::ATTACHMENTS_LIST,
            self.node_params(space.as_ref(), id)?,
        )
    }

    /// An attachment of a node with its bytes, or `None` when the node has none by that name.
    /// Fails for files larger than [`MAX_INLINE_ATTACHMENT`].
    pub fn read_attachment(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        name: &str,
    ) -> Result<Option<(Attachment, Vec<u8>)>> {
        let found: Option<AttachmentContent> = self.request(
            methods::ATTACHMENT_READ,
            attachment(space.as_ref(), id, name)?,
        )?;
        found
            .map(|found| {
                let bytes = BASE64
                    .decode(found.content)
                    .map_err(|e| Error::Protocol(format!("the content is not base64: {e}")))?;
                Ok((found.attachment, bytes))
            })
            .transpose()
    }

    /// Attaches bytes to a node under `name` and returns the attachment. At most
    /// [`MAX_INLINE_ATTACHMENT`] bytes; a person attaches larger files with
    /// [`attach_file`](Self::attach_file).
    pub fn attach(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        name: &str,
        bytes: &[u8],
        by: &Provenance,
    ) -> Result<Attachment> {
        let params = AttachmentAddParams {
            space: space_path(space.as_ref())?,
            id: *id,
            name: name.to_string(),
            content: BASE64.encode(bytes),
            by: by.clone(),
        };
        self.request(methods::ATTACHMENT_ADD, params)
    }

    /// Attaches a file on this machine to a node under its file name. People only.
    pub fn attach_file(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        path: impl AsRef<Path>,
        by: &Provenance,
    ) -> Result<Attachment> {
        let params = AttachmentAddFileParams {
            space: space_path(space.as_ref())?,
            id: *id,
            path: absolute(path.as_ref())?,
            by: by.clone(),
        };
        self.request(methods::ATTACHMENT_ADD_FILE, params)
    }

    /// Removes an attachment from a node. Returns whether there was one.
    pub fn detach(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        name: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let params = AttachmentRemoveParams {
            space: space_path(space.as_ref())?,
            id: *id,
            name: name.to_string(),
            by: by.clone(),
        };
        self.request(methods::ATTACHMENT_REMOVE, params)
    }

    /// Where the file of an attachment is on this machine, to open or show it, or `None`. The
    /// file never changes; never write to it. People only.
    pub fn attachment_path(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        name: &str,
    ) -> Result<Option<PathBuf>> {
        self.request(
            methods::ATTACHMENT_PATH,
            attachment(space.as_ref(), id, name)?,
        )
    }

    /// Imports a Claude, ChatGPT or Gemini (Google Takeout) data export, the `.zip` or the
    /// folder it was extracted to. People only.
    pub fn import_conversations(
        &self,
        space: impl AsRef<Path>,
        path: impl AsRef<Path>,
        by: &Provenance,
    ) -> Result<ConversationImport> {
        let params = ConversationsImportParams {
            space: space_path(space.as_ref())?,
            path: absolute(path.as_ref())?,
            by: by.clone(),
        };
        self.request(methods::CONVERSATIONS_IMPORT, params)
    }
}

fn in_view(space: &Path, view: &str) -> Result<InViewParams> {
    Ok(InViewParams {
        space: space_path(space)?,
        view: view.to_string(),
    })
}

fn attachment(space: &Path, id: &NodeId, name: &str) -> Result<AttachmentParams> {
    Ok(AttachmentParams {
        space: space_path(space)?,
        id: *id,
        name: name.to_string(),
    })
}

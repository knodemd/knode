//! Typed links, graph views and canvases (schema version 4, section 18 of the specification).
//!
//! Link kinds say how two nodes relate; [`KnodeSpace::node_links`] shows the links of a node with
//! the label each reads with from it. A graph view is a named filter with a viewport and the
//! positions of its nodes; a canvas is a node of kind `canvas` arranged by hand, with positions,
//! sizes and groups. Moving the viewport, positions and groups adds no change to the history.

use serde_json::{Map, Value};

use crate::{KnodeError, KnodeSpace, Link, Provenance, Result, format, parse_id};

/// How a kind of link reads from its two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LinkDirection {
    /// One way from its source (its label) and another from its target (its inverse label).
    Directed,
    /// The same from both nodes, such as *related to*.
    Symmetric,
}

impl From<format::LinkDirection> for LinkDirection {
    fn from(d: format::LinkDirection) -> Self {
        match d {
            format::LinkDirection::Directed => LinkDirection::Directed,
            format::LinkDirection::Symmetric => LinkDirection::Symmetric,
        }
    }
}

impl From<LinkDirection> for format::LinkDirection {
    fn from(d: LinkDirection) -> Self {
        match d {
            LinkDirection::Directed => format::LinkDirection::Directed,
            LinkDirection::Symmetric => format::LinkDirection::Symmetric,
        }
    }
}

/// A kind of link: built in, or defined by the space.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LinkKind {
    /// The name stored on each link, such as `part-of`.
    pub kind: String,
    /// How the link reads from its source, such as `part of`.
    pub label: String,
    /// How it reads from its target, such as `has part`. Empty for none.
    pub inverse_label: String,
    /// Whether it reads the same from both nodes.
    pub direction: LinkDirection,
    /// What the kind means. Empty for none.
    pub description: String,
    /// Whether the format defines it; a built-in kind cannot be changed or removed.
    pub built_in: bool,
}

impl From<format::LinkKind> for LinkKind {
    fn from(k: format::LinkKind) -> Self {
        LinkKind {
            kind: k.kind,
            label: k.label,
            inverse_label: k.inverse_label,
            direction: k.direction.into(),
            description: k.description,
            built_in: k.built_in,
        }
    }
}

/// A kind of link to define or change with [`KnodeSpace::set_link_kind`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LinkKindSpec {
    /// The name: lower-case letters, digits and `-`, starting with a letter, at most 32
    /// characters.
    pub kind: String,
    /// How the link reads from its source: 1 to 64 characters.
    pub label: String,
    /// How it reads from its target: at most 64 characters, empty for none.
    pub inverse_label: String,
    /// Whether it reads the same from both nodes. Cannot change while links have the kind.
    pub direction: LinkDirection,
    /// What the kind means. Empty for none.
    pub description: String,
}

/// A link as one of its nodes shows it: going out or coming in, with its label from that node.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NodeLink {
    /// The link's source.
    pub from: String,
    /// The link's target.
    pub to: String,
    /// The kind, such as `mentions` or `part-of`.
    pub kind: String,
    /// `true` for a link going out (or to the node itself), `false` for a backlink.
    pub outgoing: bool,
    /// The node at the other end.
    pub node: String,
    /// The title of the node at the other end.
    pub title: String,
    /// How the link reads from the node, such as `part of` or `has part`.
    pub label: String,
    /// The direction of the kind.
    pub direction: LinkDirection,
    /// When the link was made, in Unix milliseconds.
    pub created_at: i64,
    /// Who made the link.
    pub created_by: Provenance,
}

impl From<format::NodeLink> for NodeLink {
    fn from(l: format::NodeLink) -> Self {
        NodeLink {
            from: l.link.from.to_string(),
            to: l.link.to.to_string(),
            kind: l.link.kind,
            outgoing: l.outgoing,
            node: l.node.to_string(),
            title: l.title,
            label: l.label,
            direction: l.direction.into(),
            created_at: l.link.created_at.as_millis(),
            created_by: l.link.created_by.into(),
        }
    }
}

/// The part of a view's plane on screen: the point at the centre and the zoom.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct Viewport {
    /// The centre, `x` growing to the right.
    pub x: f64,
    /// The centre, `y` growing down.
    pub y: f64,
    /// The zoom: 1 is one point per point.
    pub zoom: f64,
}

/// Which nodes and links a graph view shows. A node is shown only when it passes every field
/// that is set; `None` leaves a field out.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct GraphFilter {
    /// Kinds of node, one of which the node must be.
    pub node_kinds: Option<Vec<String>>,
    /// Tags, one of which the node must have.
    pub tags: Option<Vec<String>>,
    /// People or agents, one of whom created or last changed the node.
    pub actors: Option<Vec<String>>,
    /// The node changed at or after this time, in Unix milliseconds.
    pub since: Option<i64>,
    /// The node changed before this time, in Unix milliseconds.
    pub until: Option<i64>,
    /// Kinds of link to show; the others are hidden.
    pub link_kinds: Option<Vec<String>>,
}

impl From<GraphFilter> for format::GraphFilter {
    fn from(f: GraphFilter) -> Self {
        format::GraphFilter {
            node_kinds: f.node_kinds,
            tags: f.tags,
            actors: f.actors,
            since: f.since.map(format::Timestamp),
            until: f.until.map(format::Timestamp),
            link_kinds: f.link_kinds,
        }
    }
}

impl From<format::GraphFilter> for GraphFilter {
    fn from(f: format::GraphFilter) -> Self {
        GraphFilter {
            node_kinds: f.node_kinds,
            tags: f.tags,
            actors: f.actors,
            since: f.since.map(|t| t.as_millis()),
            until: f.until.map(|t| t.as_millis()),
            link_kinds: f.link_kinds,
        }
    }
}

/// The keys of a stored filter this library reads and writes; others are kept as they are.
const FILTER_KEYS: [&str; 6] = [
    "node_kinds",
    "tags",
    "actors",
    "since",
    "until",
    "link_kinds",
];

/// A view: a graph view of the space, or a canvas.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct View {
    /// The view's id, a ULID.
    pub id: String,
    /// `graph` or `canvas`.
    pub kind: String,
    /// The name of a graph view; for a canvas, the title of its node.
    pub name: String,
    /// The node of a canvas; `None` for a graph view.
    pub node_id: Option<String>,
    /// The part of the plane last on screen, if it was saved.
    pub viewport: Option<Viewport>,
    /// The filter of a graph view, if it has one.
    pub filter: Option<GraphFilter>,
    /// When it was made, in Unix milliseconds.
    pub created_at: i64,
    /// When its name or settings last changed, in Unix milliseconds.
    pub updated_at: i64,
}

/// Where a node is in a view.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Position {
    /// The node.
    pub node_id: String,
    /// The centre, in points; `x` grows to the right.
    pub x: f64,
    /// The centre, in points; `y` grows down.
    pub y: f64,
    /// The width of the node's box, or `None` for the default size. Set with `height`.
    pub width: Option<f64>,
    /// The height of the node's box, or `None` for the default size. Set with `width`.
    pub height: Option<f64>,
    /// Stacking order on a canvas: higher is drawn in front.
    pub z: i64,
    /// Only the title is shown, not the content.
    pub collapsed: bool,
    /// The group it is in, on a canvas.
    pub group_id: Option<String>,
}

impl From<format::Position> for Position {
    fn from(p: format::Position) -> Self {
        Position {
            node_id: p.node_id.to_string(),
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            z: p.z,
            collapsed: p.display == format::ViewDisplay::Collapsed,
            group_id: p.group_id,
        }
    }
}

impl Position {
    fn to_spec(&self) -> Result<format::PositionSpec> {
        let size = match (self.width, self.height) {
            (Some(w), Some(h)) => Some((w, h)),
            (None, None) => None,
            _ => {
                return Err(KnodeError::Invalid {
                    message: "a node's box has both a width and a height, or neither".into(),
                });
            }
        };
        Ok(format::PositionSpec {
            node_id: parse_id(&self.node_id)?,
            x: self.x,
            y: self.y,
            size,
            z: self.z,
            display: display(self.collapsed),
            group_id: self.group_id.clone(),
        })
    }
}

fn display(collapsed: bool) -> format::ViewDisplay {
    if collapsed {
        format::ViewDisplay::Collapsed
    } else {
        format::ViewDisplay::Open
    }
}

/// A labelled box on a canvas that holds nodes.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ViewGroup {
    /// The group's id, a ULID.
    pub id: String,
    /// The label. Empty for none.
    pub label: String,
    /// The centre of the box.
    pub x: f64,
    /// The centre of the box.
    pub y: f64,
    /// The width of the box.
    pub width: f64,
    /// The height of the box.
    pub height: f64,
    /// Only the label is shown, and the nodes in it are hidden.
    pub collapsed: bool,
}

impl From<format::ViewGroup> for ViewGroup {
    fn from(g: format::ViewGroup) -> Self {
        ViewGroup {
            id: g.id,
            label: g.label,
            x: g.x,
            y: g.y,
            width: g.width,
            height: g.height,
            collapsed: g.display == format::ViewDisplay::Collapsed,
        }
    }
}

/// A group to add or change with [`KnodeSpace::set_group`].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct GroupSpec {
    /// The group to change, or `None` to add one.
    pub id: Option<String>,
    /// The label. Empty for none.
    pub label: String,
    /// The centre of the box.
    pub x: f64,
    /// The centre of the box.
    pub y: f64,
    /// The width, greater than 0.
    pub width: f64,
    /// The height, greater than 0.
    pub height: f64,
    /// Only the label is shown.
    pub collapsed: bool,
}

/// A node in a graph.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GraphNode {
    /// The node id.
    pub id: String,
    /// What the node is, such as `note` or `conversation`.
    pub kind: String,
    /// The title.
    pub title: String,
    /// Tags, lower case, sorted.
    pub tags: Vec<String>,
    /// When it was created, in Unix milliseconds.
    pub created_at: i64,
    /// When it last changed, in Unix milliseconds.
    pub updated_at: i64,
    /// The person or agent who created it.
    pub created_by: String,
    /// The person or agent who changed it last.
    pub updated_by: String,
}

/// A link between two nodes of a graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Record)]
pub struct GraphEdge {
    /// The source.
    pub from: String,
    /// The target.
    pub to: String,
    /// The kind.
    pub kind: String,
}

/// The nodes and links a graph view shows, with the positions saved for it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Graph {
    /// The nodes the filter keeps, newest first.
    pub nodes: Vec<GraphNode>,
    /// The links between them of the kinds the filter keeps.
    pub edges: Vec<GraphEdge>,
    /// Where nodes were put in the view; empty without a view.
    pub positions: Vec<Position>,
    /// Every kind of link the space knows, to label the edges.
    pub link_kinds: Vec<LinkKind>,
}

/// A node on a canvas, with what the board shows of it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct CanvasItem {
    /// Where it is.
    pub position: Position,
    /// What the node is.
    pub kind: String,
    /// The title.
    pub title: String,
    /// The first line of plain text in its body.
    pub excerpt: String,
}

/// A canvas as its board shows it: its nodes, groups and the links between its nodes.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct CanvasBoard {
    /// The canvas view.
    pub view: View,
    /// The nodes on it, back to front.
    pub items: Vec<CanvasItem>,
    /// Its groups.
    pub groups: Vec<ViewGroup>,
    /// The links between its nodes.
    pub edges: Vec<GraphEdge>,
    /// Every kind of link the space knows, to label the edges.
    pub link_kinds: Vec<LinkKind>,
}

#[uniffi::export]
impl KnodeSpace {
    /// Every kind of link a link can be made with: the built-in kinds, then the space's own.
    pub fn link_kinds(&self) -> Result<Vec<LinkKind>> {
        let kinds = self.lock().link_kinds()?;
        Ok(kinds.into_iter().map(LinkKind::from).collect())
    }

    /// Defines a kind of link, or changes its labels and description, and returns it.
    pub fn set_link_kind(&self, spec: LinkKindSpec, by: Provenance) -> Result<LinkKind> {
        let spec = format::LinkKindSpec {
            kind: spec.kind,
            label: spec.label,
            inverse_label: spec.inverse_label,
            direction: spec.direction.into(),
            description: spec.description,
        };
        Ok(self.lock().set_link_kind(&spec, &by.into())?.into())
    }

    /// Removes a kind the space defined, while no link has it. Returns whether there was one.
    pub fn remove_link_kind(&self, kind: String, by: Provenance) -> Result<bool> {
        Ok(self.lock().remove_link_kind(&kind, &by.into())?)
    }

    /// The links of a node, going out then coming in, each with the label it reads with from
    /// the node. A symmetric link between two nodes is shown once.
    pub fn node_links(&self, id: String) -> Result<Vec<NodeLink>> {
        let id = parse_id(&id)?;
        let links = self.lock().node_links(&id)?;
        Ok(links.into_iter().map(NodeLink::from).collect())
    }

    /// The backlinks of a node, of every kind, each with its inverse label.
    pub fn labeled_backlinks(&self, id: String) -> Result<Vec<NodeLink>> {
        let id = parse_id(&id)?;
        let links = self.lock().labeled_backlinks(&id)?;
        Ok(links.into_iter().map(NodeLink::from).collect())
    }

    /// Links two nodes with a kind (empty for `related`) and returns the link. Linking twice
    /// records nothing. `mentions` links come only from `[[links]]` in a body.
    pub fn link(&self, from: String, to: String, kind: String, by: Provenance) -> Result<Link> {
        let (from, to) = (parse_id(&from)?, parse_id(&to)?);
        Ok(self.lock().link(&from, &to, &kind, &by.into())?.into())
    }

    /// Removes a link made by hand. Returns whether there was one.
    pub fn unlink(&self, from: String, to: String, kind: String, by: Provenance) -> Result<bool> {
        let (from, to) = (parse_id(&from)?, parse_id(&to)?);
        Ok(self.lock().unlink_by(&from, &to, &kind, &by.into())?)
    }

    /// The views a reader shows, oldest first: the graph views and the canvases whose node
    /// exists. A canvas is named by its node's title.
    pub fn views(&self) -> Result<Vec<View>> {
        let space = self.lock();
        let views = space.views()?;
        views.into_iter().map(|v| view(&space, v)).collect()
    }

    /// A view by id, or `None`.
    pub fn view(&self, id: String) -> Result<Option<View>> {
        let space = self.lock();
        space.view(&id)?.map(|v| view(&space, v)).transpose()
    }

    /// Creates a graph view (`id` `None`), or renames it or changes its filter, and returns
    /// it. Keys of a stored filter that this version does not know are kept.
    pub fn set_graph_view(
        &self,
        id: Option<String>,
        name: String,
        filter: Option<GraphFilter>,
        by: Provenance,
    ) -> Result<View> {
        let mut space = self.lock();
        let kept: Map<String, Value> = match &id {
            Some(id) => space
                .view(id)?
                .and_then(|v| v.filter().and_then(Value::as_object).cloned())
                .map(|mut object| {
                    object.retain(|key, _| !FILTER_KEYS.contains(&key.as_str()));
                    object
                })
                .unwrap_or_default(),
            None => Map::new(),
        };
        let mut object = match format::GraphFilter::from(filter.unwrap_or_default()).to_json() {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        object.extend(kept);
        let spec = format::GraphViewSpec {
            id,
            name,
            filter: (!object.is_empty()).then_some(Value::Object(object)),
        };
        let found = space.set_graph_view(&spec, &by.into())?;
        view(&space, found)
    }

    /// Removes a graph view with its positions. A canvas is removed by deleting its node.
    pub fn remove_view(&self, id: String, by: Provenance) -> Result<bool> {
        Ok(self.lock().remove_view(&id, &by.into())?)
    }

    /// Saves the part of a view's plane on screen. Adds no change.
    pub fn set_viewport(&self, id: String, viewport: Viewport, by: Provenance) -> Result<()> {
        let viewport = format::Viewport {
            x: viewport.x,
            y: viewport.y,
            zoom: viewport.zoom,
        };
        Ok(self.lock().set_viewport(&id, &viewport, &by.into())?)
    }

    /// The nodes a filter keeps and the links between them, with the positions saved in
    /// `view` (if given). Without a filter, the view's own filter applies; with neither, the
    /// whole space.
    pub fn graph(&self, view: Option<String>, filter: Option<GraphFilter>) -> Result<Graph> {
        let filter = filter.map(format::GraphFilter::from);
        let graph = self.lock().graph(view.as_deref(), filter.as_ref())?;
        Ok(graph.into())
    }

    /// Creates a canvas: a node of kind `canvas` with this title and body, and its view.
    pub fn create_canvas(&self, title: String, body: String, by: Provenance) -> Result<View> {
        let mut space = self.lock();
        let found = space.create_canvas(format::NewNode::note(title, body), &by.into())?;
        view(&space, found)
    }

    /// The canvas whose node is `node`, as its board shows it, or `None` when the node is not
    /// a canvas.
    pub fn canvas_board(&self, node: String) -> Result<Option<CanvasBoard>> {
        let node = parse_id(&node)?;
        let space = self.lock();
        let Some(board) = space.canvas_board(&node)? else {
            return Ok(None);
        };
        let view = View::from_format(board.view, board.title);
        Ok(Some(CanvasBoard {
            view,
            items: board.items.into_iter().map(CanvasItem::from).collect(),
            groups: board.groups.into_iter().map(ViewGroup::from).collect(),
            edges: board.edges.into_iter().map(GraphEdge::from).collect(),
            link_kinds: board.link_kinds.into_iter().map(LinkKind::from).collect(),
        }))
    }

    /// The positions in a view of the nodes that exist, back to front.
    pub fn positions(&self, view: String) -> Result<Vec<Position>> {
        let positions = self.lock().positions(&view)?;
        Ok(positions.into_iter().map(Position::from).collect())
    }

    /// Places nodes in a view in one write, adding or replacing their positions. Adds no
    /// change.
    pub fn set_positions(
        &self,
        view: String,
        positions: Vec<Position>,
        by: Provenance,
    ) -> Result<()> {
        let specs = positions
            .iter()
            .map(Position::to_spec)
            .collect::<Result<Vec<_>>>()?;
        Ok(self.lock().set_positions(&view, &specs, &by.into())?)
    }

    /// Takes nodes out of a view; the nodes are not deleted. Returns how many were in it.
    pub fn remove_positions(&self, view: String, nodes: Vec<String>) -> Result<u32> {
        let ids = nodes
            .iter()
            .map(|id| parse_id(id))
            .collect::<Result<Vec<_>>>()?;
        let removed = self.lock().remove_positions(&view, &ids)?;
        Ok(u32::try_from(removed).unwrap_or(u32::MAX))
    }

    /// The groups of a canvas.
    pub fn groups(&self, view: String) -> Result<Vec<ViewGroup>> {
        let groups = self.lock().groups(&view)?;
        Ok(groups.into_iter().map(ViewGroup::from).collect())
    }

    /// Adds a group to a canvas, or changes one, and returns it. Adds no change.
    pub fn set_group(&self, view: String, spec: GroupSpec, by: Provenance) -> Result<ViewGroup> {
        let spec = format::GroupSpec {
            id: spec.id,
            label: spec.label,
            x: spec.x,
            y: spec.y,
            width: spec.width,
            height: spec.height,
            display: display(spec.collapsed),
        };
        Ok(self.lock().set_group(&view, &spec, &by.into())?.into())
    }

    /// Removes a group; its nodes stay on the canvas, outside any group. Adds no change.
    pub fn remove_group(&self, id: String) -> Result<bool> {
        Ok(self.lock().remove_group(&id)?)
    }
}

/// A view as the apps see it: a canvas is named by its node's title.
fn view(space: &format::Space, v: format::View) -> Result<View> {
    let name = match (&v.node_id, v.kind.as_str()) {
        (Some(node), format::view_kinds::CANVAS) => {
            space.get_node(node)?.map(|n| n.title).unwrap_or_default()
        }
        _ => v.name.clone(),
    };
    Ok(View::from_format(v, name))
}

impl View {
    /// Converts a view, named `name`. A stored filter that cannot be read is shown as none.
    fn from_format(v: format::View, name: String) -> View {
        View {
            viewport: v.viewport().map(|p| Viewport {
                x: p.x,
                y: p.y,
                zoom: p.zoom,
            }),
            filter: v
                .filter()
                .and_then(|f| format::GraphFilter::from_json(f).ok())
                .map(GraphFilter::from),
            id: v.id,
            kind: v.kind,
            name,
            node_id: v.node_id.map(|n| n.to_string()),
            created_at: v.created_at.as_millis(),
            updated_at: v.updated_at.as_millis(),
        }
    }
}

impl From<format::GraphNode> for GraphNode {
    fn from(n: format::GraphNode) -> Self {
        GraphNode {
            id: n.id.to_string(),
            kind: n.kind,
            title: n.title,
            tags: n.tags,
            created_at: n.created_at.as_millis(),
            updated_at: n.updated_at.as_millis(),
            created_by: n.created_by.actor,
            updated_by: n.updated_by.actor,
        }
    }
}

impl From<format::GraphEdge> for GraphEdge {
    fn from(e: format::GraphEdge) -> Self {
        GraphEdge {
            from: e.from.to_string(),
            to: e.to.to_string(),
            kind: e.kind,
        }
    }
}

impl From<format::Graph> for Graph {
    fn from(g: format::Graph) -> Self {
        Graph {
            nodes: g.nodes.into_iter().map(GraphNode::from).collect(),
            edges: g.edges.into_iter().map(GraphEdge::from).collect(),
            positions: g.positions.into_iter().map(Position::from).collect(),
            link_kinds: g.link_kinds.into_iter().map(LinkKind::from).collect(),
        }
    }
}

impl From<format::CanvasItem> for CanvasItem {
    fn from(i: format::CanvasItem) -> Self {
        CanvasItem {
            position: i.position.into(),
            kind: i.kind,
            title: i.title,
            excerpt: i.excerpt,
        }
    }
}

#[cfg(test)]
#[path = "maps_tests.rs"]
mod tests;

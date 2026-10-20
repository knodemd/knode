//! Views: graph views and canvases, with the positions and groups of their nodes (schema
//! version 4).

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::space::{
    Content, add_change, add_change_with_detail, check_title, clean_tags, insert_node, next_ulid,
    node_id, provenance, provenance_id, record,
};
use crate::types::ChangeOp;
use crate::{
    Error, NewNode, NodeId, Provenance, Result, Space, Timestamp, actions, kinds, view_kinds,
};

/// How a node or a group is shown in a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewDisplay {
    /// With its content: on a canvas, a node's body; a group with its nodes.
    #[default]
    Open,
    /// Only its title, or a group's label.
    Collapsed,
}

impl ViewDisplay {
    /// The value stored in `display`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewDisplay::Open => "open",
            ViewDisplay::Collapsed => "collapsed",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "collapsed" => ViewDisplay::Collapsed,
            _ => ViewDisplay::Open,
        }
    }
}

/// The part of a view's plane on screen: the point at the centre and the zoom (1 is one point
/// per point).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// The centre, `x` growing to the right.
    pub x: f64,
    /// The centre, `y` growing down.
    pub y: f64,
    /// The zoom, greater than 0.
    pub zoom: f64,
}

/// A view: a graph of the space, or a canvas arranged by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// The view's id, a ULID.
    pub id: String,
    /// `graph` or `canvas`. See [`view_kinds`](crate::view_kinds).
    pub kind: String,
    /// The name of a graph view. Empty for a canvas, whose name is the title of its node.
    pub name: String,
    /// The node of a canvas; `None` for a graph view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    /// The settings of the view, a JSON object: `viewport`, and `filter` for a graph view. Keys
    /// this library does not know are kept.
    pub settings: Value,
    /// When it was made.
    pub created_at: Timestamp,
    /// Who made it.
    pub created_by: Provenance,
    /// When its name or settings last changed.
    pub updated_at: Timestamp,
    /// Who changed them last.
    pub updated_by: Provenance,
}

impl View {
    /// The part of the plane last on screen, if it was saved.
    pub fn viewport(&self) -> Option<Viewport> {
        serde_json::from_value(self.settings.get("viewport")?.clone()).ok()
    }

    /// The filter of a graph view, a JSON object, if it has one.
    pub fn filter(&self) -> Option<&Value> {
        self.settings.get("filter").filter(|f| f.is_object())
    }
}

/// A graph view to create or change with [`Space::set_graph_view`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphViewSpec {
    /// The id of the view to change, or `None` to create one.
    #[serde(default)]
    pub id: Option<String>,
    /// The name: 1 to 128 characters.
    pub name: String,
    /// Which nodes and links it shows, a JSON object (keys `node_kinds`, `tags`, `actors`,
    /// `since`, `until` and `link_kinds`), or `None` for the whole space.
    #[serde(default)]
    pub filter: Option<Value>,
}

/// Where a node is in a view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// The view.
    pub view_id: String,
    /// The node.
    pub node_id: NodeId,
    /// The centre of the node, in points; `x` grows to the right.
    pub x: f64,
    /// The centre of the node, in points; `y` grows down.
    pub y: f64,
    /// The width of the node's box, or `None` for the reader's default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// The height of the node's box, or `None` for the reader's default size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Stacking order on a canvas: higher is drawn in front.
    pub z: i64,
    /// Whether the node shows its content or only its title.
    pub display: ViewDisplay,
    /// The group it is in, on a canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// When it was last placed.
    pub updated_at: Timestamp,
    /// Who placed it last.
    pub updated_by: Provenance,
}

/// Where to put a node with [`Space::set_positions`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionSpec {
    /// The node.
    pub node_id: NodeId,
    /// The centre, in points.
    pub x: f64,
    /// The centre, in points.
    pub y: f64,
    /// The width and height of the node's box, or `None` for the reader's default size. Both
    /// greater than 0.
    #[serde(default)]
    pub size: Option<(f64, f64)>,
    /// Stacking order on a canvas.
    #[serde(default)]
    pub z: i64,
    /// Whether the node shows its content or only its title.
    #[serde(default)]
    pub display: ViewDisplay,
    /// The group to put it in, on a canvas.
    #[serde(default)]
    pub group_id: Option<String>,
}

impl PositionSpec {
    /// A node at a point, with the default size, open and outside any group.
    pub fn at(node_id: NodeId, x: f64, y: f64) -> Self {
        PositionSpec {
            node_id,
            x,
            y,
            size: None,
            z: 0,
            display: ViewDisplay::Open,
            group_id: None,
        }
    }
}

/// A labelled box on a canvas that holds nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewGroup {
    /// The group's id, a ULID.
    pub id: String,
    /// The canvas view.
    pub view_id: String,
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
    /// Whether the group shows its nodes or only its label.
    pub display: ViewDisplay,
    /// When it last changed.
    pub updated_at: Timestamp,
    /// Who changed it last.
    pub updated_by: Provenance,
}

/// A group to add or change with [`Space::set_group`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupSpec {
    /// The id of the group to change, or `None` to add one.
    #[serde(default)]
    pub id: Option<String>,
    /// The label. Empty for none.
    #[serde(default)]
    pub label: String,
    /// The centre of the box.
    pub x: f64,
    /// The centre of the box.
    pub y: f64,
    /// The width of the box, greater than 0.
    pub width: f64,
    /// The height of the box, greater than 0.
    pub height: f64,
    /// Whether the group shows its nodes or only its label.
    #[serde(default)]
    pub display: ViewDisplay,
}

const VIEW_COLUMNS: &str = "v.id, v.kind, v.name, v.node_id, v.settings, \
     v.created_at, c.actor_kind, c.actor, c.session, c.source, \
     v.updated_at, u.actor_kind, u.actor, u.session, u.source";

fn view_row(row: &Row<'_>) -> rusqlite::Result<View> {
    let node: Option<String> = row.get(3)?;
    let settings: String = row.get(4)?;
    Ok(View {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        node_id: match node {
            Some(_) => Some(node_id(row, 3)?),
            None => None,
        },
        settings: serde_json::from_str(&settings)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Default::default())),
        created_at: Timestamp(row.get(5)?),
        created_by: provenance(row, 6)?,
        updated_at: Timestamp(row.get(10)?),
        updated_by: provenance(row, 11)?,
    })
}

fn query_views<P: rusqlite::Params>(conn: &Connection, tail: &str, params: P) -> Result<Vec<View>> {
    let sql = format!(
        "SELECT {VIEW_COLUMNS} FROM views v
         JOIN provenance c ON c.id = v.created_by
         JOIN provenance u ON u.id = v.updated_by
         {tail}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, view_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn read_view(conn: &Connection, id: &str) -> Result<Option<View>> {
    Ok(query_views(conn, "WHERE v.id = ?1", [id])?.pop())
}

fn require_view(conn: &Connection, id: &str) -> Result<View> {
    read_view(conn, id)?.ok_or_else(|| Error::Invalid(format!("no view has the id {id:?}")))
}

fn group_row(row: &Row<'_>) -> rusqlite::Result<ViewGroup> {
    Ok(ViewGroup {
        id: row.get(0)?,
        view_id: row.get(1)?,
        label: row.get(2)?,
        x: row.get(3)?,
        y: row.get(4)?,
        width: row.get(5)?,
        height: row.get(6)?,
        display: ViewDisplay::parse(&row.get::<_, String>(7)?),
        updated_at: Timestamp(row.get(8)?),
        updated_by: provenance(row, 9)?,
    })
}

fn query_groups<P: rusqlite::Params>(
    conn: &Connection,
    filter: &str,
    params: P,
) -> Result<Vec<ViewGroup>> {
    let sql = format!(
        "SELECT g.id, g.view_id, g.label, g.x, g.y, g.width, g.height, g.display, g.updated_at,
                u.actor_kind, u.actor, u.session, u.source
         FROM view_groups g JOIN provenance u ON u.id = g.updated_by
         WHERE {filter} ORDER BY g.id"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, group_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn group_in_view(conn: &Connection, group: &str, view: &str) -> Result<bool> {
    Ok(!query_groups(conn, "g.id = ?1 AND g.view_id = ?2", params![group, view])?.is_empty())
}

fn finite(what: &str, values: &[f64]) -> Result<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("the {what} must be finite numbers")))
    }
}

fn check_size(what: &str, width: f64, height: f64) -> Result<()> {
    finite("size", &[width, height])?;
    if width > 0.0 && height > 0.0 {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "the size of {what} must be greater than 0"
        )))
    }
}

fn check_view_name(name: &str) -> Result<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = name.chars().count();
    if count == 0 || count > 128 {
        return Err(Error::Invalid(
            "the name of a graph view is 1 to 128 characters".into(),
        ));
    }
    Ok(name)
}

fn node_exists(conn: &Connection, id: &NodeId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM nodes WHERE id = ?1",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Updates the settings of a view and when and by whom they changed.
fn write_settings(
    tx: &Transaction<'_>,
    id: &str,
    settings: &Value,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    tx.execute(
        "UPDATE views SET settings = ?2, updated_at = ?3, updated_by = ?4 WHERE id = ?1",
        params![id, settings.to_string(), now.0, prov],
    )?;
    Ok(())
}

impl Space {
    /// The views a reader shows, oldest first: the graph views, and the canvases whose node
    /// exists. Views of kinds this library does not know are kept and not listed.
    pub fn views(&self) -> Result<Vec<View>> {
        query_views(
            &self.conn,
            "WHERE v.kind = ?1
                OR (v.kind = ?2 AND EXISTS (SELECT 1 FROM nodes n WHERE n.id = v.node_id))
             ORDER BY v.created_at, v.id",
            params![view_kinds::GRAPH, view_kinds::CANVAS],
        )
    }

    /// A view by id, of any kind, or `None`.
    pub fn view(&self, id: &str) -> Result<Option<View>> {
        read_view(&self.conn, id)
    }

    /// The canvas whose node is `node`, or `None`. Found even while the node is deleted, so a
    /// restored canvas comes back as it was.
    pub fn canvas(&self, node: &NodeId) -> Result<Option<View>> {
        Ok(query_views(
            &self.conn,
            "WHERE v.node_id = ?1 AND v.kind = ?2",
            params![node.to_string(), view_kinds::CANVAS],
        )?
        .pop())
    }

    /// Creates a graph view, or renames or filters the one with `spec.id`, records the change
    /// and returns the view. Changing nothing writes nothing.
    ///
    /// Recorded as a `view.set` change, which is not undone through [`undo`](Self::undo):
    /// change the view again instead.
    pub fn set_graph_view(&mut self, spec: &GraphViewSpec, by: &Provenance) -> Result<View> {
        let name = check_view_name(&spec.name)?;
        if spec.filter.as_ref().is_some_and(|f| !f.is_object()) {
            return Err(Error::Invalid(
                "the filter of a graph view is a JSON object".into(),
            ));
        }
        let current = match &spec.id {
            Some(id) => {
                let view = require_view(&self.conn, id)?;
                if view.kind != view_kinds::GRAPH {
                    return Err(Error::Invalid(format!(
                        "view {id} is a {}, not a graph view",
                        view.kind
                    )));
                }
                Some(view)
            }
            None => None,
        };
        if let Some(current) = &current {
            if current.name == name && current.filter() == spec.filter.as_ref() {
                return Ok(current.clone());
            }
        }
        let now = Timestamp::now();
        let id = match &current {
            Some(view) => view.id.clone(),
            None => next_ulid(&mut self.ids).to_string(),
        };
        let mut settings = current
            .as_ref()
            .map(|v| v.settings.clone())
            .unwrap_or_else(|| Value::Object(Default::default()));
        if let Some(object) = settings.as_object_mut() {
            match &spec.filter {
                Some(filter) => object.insert("filter".into(), filter.clone()),
                None => object.remove("filter"),
            };
        }
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        if current.is_some() {
            tx.execute(
                "UPDATE views SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            write_settings(&tx, &id, &settings, prov, now)?;
        } else {
            tx.execute(
                "INSERT INTO views (id, kind, name, node_id, settings, created_at, created_by,
                                    updated_at, updated_by)
                 VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?5, ?6)",
                params![
                    id,
                    view_kinds::GRAPH,
                    name,
                    settings.to_string(),
                    now.0,
                    prov
                ],
            )?;
        }
        let detail = serde_json::json!({ "name": name, "filter": spec.filter }).to_string();
        add_change_with_detail(&tx, now, prov, actions::VIEW_SET, &id, None, &detail)?;
        tx.commit()?;
        require_view(&self.conn, &id)
    }

    /// Removes a graph view with its positions, records the change and returns whether there
    /// was one. A canvas is not removed on its own: delete its node instead.
    pub fn remove_view(&mut self, id: &str, by: &Provenance) -> Result<bool> {
        let Some(view) = read_view(&self.conn, id)? else {
            return Ok(false);
        };
        if view.kind != view_kinds::GRAPH {
            return Err(Error::Invalid(format!(
                "view {id} is a {}; delete its node to remove a canvas",
                view.kind
            )));
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute("DELETE FROM views WHERE id = ?1", [id])?;
        let detail = serde_json::json!({ "name": view.name, "filter": view.filter() }).to_string();
        add_change_with_detail(&tx, now, prov, actions::VIEW_REMOVE, id, None, &detail)?;
        tx.commit()?;
        Ok(true)
    }

    /// Saves the part of a view's plane on screen. Adds no change: like positions, it carries
    /// its own provenance.
    pub fn set_viewport(&mut self, id: &str, viewport: &Viewport, by: &Provenance) -> Result<()> {
        finite("viewport", &[viewport.x, viewport.y, viewport.zoom])?;
        if viewport.zoom <= 0.0 {
            return Err(Error::Invalid("the zoom must be greater than 0".into()));
        }
        let view = require_view(&self.conn, id)?;
        let mut settings = view.settings;
        if let Some(object) = settings.as_object_mut() {
            object.insert(
                "viewport".into(),
                serde_json::json!({ "x": viewport.x, "y": viewport.y, "zoom": viewport.zoom }),
            );
        }
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        write_settings(&tx, id, &settings, prov, Timestamp::now())?;
        tx.commit()?;
        Ok(())
    }

    /// Creates a canvas: a node of kind `canvas`, whose title names the canvas and whose body
    /// says what it is for, and its view, in one write recorded as a `node.create` change.
    /// Returns the view; its `node_id` is the node. The kind of `new` is ignored.
    ///
    /// Deleting the canvas is deleting its node with [`delete_node`](Self::delete_node): the view,
    /// its positions and groups are kept, so undoing the delete or restoring the node brings the
    /// canvas back as it was.
    pub fn create_canvas(&mut self, new: NewNode, by: &Provenance) -> Result<View> {
        let title = check_title(&new.title)?;
        let tags = clean_tags(&new.tags)?;
        let node = NodeId::from_ulid(next_ulid(&mut self.ids));
        let view = next_ulid(&mut self.ids).to_string();
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(
            &tx,
            now,
            prov,
            actions::NODE_CREATE,
            &node.to_string(),
            None,
        )?;
        let content = Content {
            kind: kinds::CANVAS.to_string(),
            title,
            body: new.body,
            tags,
        };
        insert_node(&tx, &node, &content, (now, prov), prov, now)?;
        record(&tx, &node, ChangeOp::Create, prov, now, change, None)?;
        tx.execute(
            "INSERT INTO views (id, kind, name, node_id, settings, created_at, created_by,
                                updated_at, updated_by)
             VALUES (?1, ?2, '', ?3, '{}', ?4, ?5, ?4, ?5)",
            params![view, view_kinds::CANVAS, node.to_string(), now.0, prov],
        )?;
        tx.commit()?;
        require_view(&self.conn, &view)
    }

    /// The positions in a view of the nodes that exist, back to front. Positions of deleted
    /// nodes are kept, so restoring a node puts it back where it was.
    pub fn positions(&self, view: &str) -> Result<Vec<Position>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT p.view_id, p.node_id, p.x, p.y, p.width, p.height, p.z, p.display,
                    p.group_id, p.updated_at, u.actor_kind, u.actor, u.session, u.source
             FROM positions p
             JOIN nodes n ON n.id = p.node_id
             JOIN provenance u ON u.id = p.updated_by
             WHERE p.view_id = ?1
             ORDER BY p.z, p.node_id",
        )?;
        let rows = stmt.query_map([view], |row| {
            Ok(Position {
                view_id: row.get(0)?,
                node_id: node_id(row, 1)?,
                x: row.get(2)?,
                y: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
                z: row.get(6)?,
                display: ViewDisplay::parse(&row.get::<_, String>(7)?),
                group_id: row.get(8)?,
                updated_at: Timestamp(row.get(9)?),
                updated_by: provenance(row, 10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Places nodes in a view, adding or replacing their positions, in one write so the view is
    /// never seen half moved. Adds no change: positions carry their own provenance.
    ///
    /// Every node must exist. Groups are only on a canvas, and must be groups of the view.
    pub fn set_positions(
        &mut self,
        view: &str,
        positions: &[PositionSpec],
        by: &Provenance,
    ) -> Result<()> {
        let found = require_view(&self.conn, view)?;
        let canvas = found.kind == view_kinds::CANVAS;
        if !canvas && found.kind != view_kinds::GRAPH {
            return Err(Error::Invalid(format!(
                "view {view} is of a kind this version does not know: {}",
                found.kind
            )));
        }
        for spec in positions {
            finite("position", &[spec.x, spec.y])?;
            if let Some((width, height)) = spec.size {
                check_size("a node", width, height)?;
            }
            if !node_exists(&self.conn, &spec.node_id)? {
                return Err(Error::NotFound(spec.node_id));
            }
            if let Some(group) = &spec.group_id {
                if !canvas {
                    return Err(Error::Invalid("groups are only on a canvas".into()));
                }
                if !group_in_view(&self.conn, group, view)? {
                    return Err(Error::Invalid(format!("no group {group:?} on this canvas")));
                }
            }
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO positions (view_id, node_id, x, y, width, height, z, display,
                                        group_id, updated_at, updated_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (view_id, node_id) DO UPDATE SET
                     x = excluded.x, y = excluded.y, width = excluded.width,
                     height = excluded.height, z = excluded.z, display = excluded.display,
                     group_id = excluded.group_id, updated_at = excluded.updated_at,
                     updated_by = excluded.updated_by",
            )?;
            for spec in positions {
                stmt.execute(params![
                    view,
                    spec.node_id.to_string(),
                    spec.x,
                    spec.y,
                    spec.size.map(|s| s.0),
                    spec.size.map(|s| s.1),
                    spec.z,
                    spec.display.as_str(),
                    spec.group_id,
                    now.0,
                    prov
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Takes nodes out of a view, deleting their positions; the nodes are not deleted. Returns
    /// how many positions there were. Adds no change.
    pub fn remove_positions(&mut self, view: &str, nodes: &[NodeId]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for node in nodes {
            removed += tx.execute(
                "DELETE FROM positions WHERE view_id = ?1 AND node_id = ?2",
                params![view, node.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// The groups of a canvas. Empty for a view that is not a canvas.
    pub fn groups(&self, view: &str) -> Result<Vec<ViewGroup>> {
        query_groups(
            &self.conn,
            "g.view_id = ?1
             AND EXISTS (SELECT 1 FROM views v WHERE v.id = g.view_id AND v.kind = ?2)",
            params![view, view_kinds::CANVAS],
        )
    }

    /// Adds a group to a canvas, or changes the one with `spec.id`, and returns it. Adds no
    /// change: groups carry their own provenance.
    pub fn set_group(
        &mut self,
        view: &str,
        spec: &GroupSpec,
        by: &Provenance,
    ) -> Result<ViewGroup> {
        let found = require_view(&self.conn, view)?;
        if found.kind != view_kinds::CANVAS {
            return Err(Error::Invalid("groups are only on a canvas".into()));
        }
        finite("position", &[spec.x, spec.y])?;
        check_size("a group", spec.width, spec.height)?;
        let label = spec.label.trim().to_string();
        if label.chars().any(char::is_control) {
            return Err(Error::Invalid("the label of a group is one line".into()));
        }
        let id = match &spec.id {
            Some(id) if group_in_view(&self.conn, id, view)? => id.clone(),
            Some(id) => return Err(Error::Invalid(format!("no group {id:?} on this canvas"))),
            None => next_ulid(&mut self.ids).to_string(),
        };
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT INTO view_groups (id, view_id, label, x, y, width, height, display,
                                      updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
                 label = excluded.label, x = excluded.x, y = excluded.y,
                 width = excluded.width, height = excluded.height, display = excluded.display,
                 updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            params![
                id,
                view,
                label,
                spec.x,
                spec.y,
                spec.width,
                spec.height,
                spec.display.as_str(),
                now.0,
                prov
            ],
        )?;
        tx.commit()?;
        query_groups(&self.conn, "g.id = ?1", [&id])?
            .pop()
            .ok_or_else(|| Error::Invalid("the group could not be read back".into()))
    }

    /// Removes a group. Its nodes stay on the canvas, outside any group. Returns whether there
    /// was one. Adds no change.
    pub fn remove_group(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM view_groups WHERE id = ?1", [id])?
            > 0)
    }
}

/// Removes what a purged node leaves in views: its canvas, with the canvas's positions and
/// groups, and its positions in every view.
pub(crate) fn purge_node_views(tx: &Transaction<'_>, id: &NodeId) -> Result<()> {
    tx.execute(
        "DELETE FROM views WHERE node_id = ?1 AND kind = ?2",
        params![id.to_string(), view_kinds::CANVAS],
    )?;
    tx.execute("DELETE FROM positions WHERE node_id = ?1", [id.to_string()])?;
    Ok(())
}

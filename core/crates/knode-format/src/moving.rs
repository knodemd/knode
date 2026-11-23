//! Moving a project to another space: the project, the projects inside it and the nodes that
//! are part of them, with their ids, bodies, tags, properties, attachments, Loro documents,
//! history, the links among them and the project folders that hold them.
//!
//! A move is one `project.move` change in each space. The space it goes to gets every version of
//! every node, with who wrote it and when; the space it leaves keeps the nodes as deleted, so the
//! move can be undone in both spaces at once with [`Space::undo_move`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::attachments::{blob_file, purge_node_attachments, remove_blob_files};
use crate::history::{active_undo, put_version, read_change};
use crate::projects::PROJECT_RULE;
use crate::space::{
    Content, Write, add_change, add_change_with_detail, delete_in, insert_link, link_exists,
    provenance, provenance_id, query_history, read_node, resolve_waiting_links,
    resolve_waiting_paths,
};
use crate::types::{ActorKind, ChangeOp};
use crate::views::purge_node_views;
use crate::{
    Change, ChangeId, Error, Folder, NodeId, NodeSummary, Provenance, Result, Space, Timestamp,
    actions, folder_kinds, kinds, link_kinds, property_keys,
};

/// A space, by id and name, as a move records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRef {
    /// The id of the space, from its manifest.
    pub id: String,
    /// Its name.
    pub name: String,
}

impl SpaceRef {
    fn of(space: &Space) -> Self {
        SpaceRef {
            id: space.manifest().id.clone(),
            name: space.manifest().name.clone(),
        }
    }
}

/// A link between a node that moves and one that stays. Links do not cross spaces: a typed link
/// is removed (and comes back if the move is undone), and a `[[link]]` in a body stays as the
/// text the author wrote, waiting for a node of that title in its new space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossingLink {
    /// The node the link starts from.
    pub from: NodeId,
    /// Its title.
    pub from_title: String,
    /// The kind of link.
    pub kind: String,
    /// The node it points at.
    pub to: NodeId,
    /// Its title.
    pub to_title: String,
    /// Whether the node the link starts from is the one that moves.
    pub from_moves: bool,
}

/// Something of the project that stays in the space it is in, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staying {
    /// What it is: a node title, a folder path or a file name.
    pub what: String,
    /// The node, when it is a node or a node's file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    /// Why it stays.
    pub reason: String,
}

/// What moving a project would do, worked out without writing anything. See
/// [`Space::preview_move`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePreview {
    /// The project that moves.
    pub project: NodeSummary,
    /// The space it leaves.
    pub from: SpaceRef,
    /// The space it goes to.
    pub to: SpaceRef,
    /// The projects that move: the project and every project inside it, at any depth.
    pub projects: Vec<NodeSummary>,
    /// How many nodes that are not projects move with them.
    pub notes: u64,
    /// How many versions of those nodes move with them.
    pub versions: u64,
    /// How many files are attached to them.
    pub attachments: u64,
    /// How many bytes those files take (each file counted once).
    pub attachment_bytes: u64,
    /// How many links among the nodes that move are kept.
    pub links_kept: u64,
    /// The links between a node that moves and one that stays, which do not cross spaces.
    pub crossing: Vec<CrossingLink>,
    /// The project folders that move, so conversations held in them are captured into the new
    /// space.
    pub folders: Vec<Folder>,
    /// What stays where it is, and why.
    pub stays: Vec<Staying>,
    /// Why the project cannot move now; empty when it can.
    pub conflicts: Vec<String>,
}

/// A move made with [`Space::move_project`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedProject {
    /// What moved.
    pub preview: MovePreview,
    /// The `project.move` change in the space the project left.
    pub from_change: ChangeId,
    /// The `project.move` change in the space it went to.
    pub to_change: ChangeId,
}

/// A move undone with [`Space::undo_move`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoneMove {
    /// The project.
    pub project: NodeId,
    /// The space the project is back in.
    pub from: SpaceRef,
    /// The space it was moved to.
    pub to: SpaceRef,
    /// The `undo` change in the space it is back in.
    pub from_change: Change,
    /// The `undo` change in the space it was moved to.
    pub to_change: Change,
    /// The nodes that came back.
    pub nodes: Vec<NodeId>,
}

/// Everything a move writes, worked out before writing.
struct Plan {
    preview: MovePreview,
    /// Every node that moves, in the order the space it leaves created them.
    nodes: Vec<NodeId>,
    /// The links among them.
    kept: Vec<(NodeId, String, NodeId)>,
    /// The project each moving folder holds.
    folders: Vec<(Folder, NodeId)>,
    /// The attachments that move: node, name, hash.
    attachments: Vec<(NodeId, String, String)>,
    /// Nodes the space it goes to had before and gave away in an earlier move: their old history
    /// there is replaced by the history they bring.
    returning: Vec<NodeId>,
}

/// The `detail` of a `project.move` change.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MoveDetail {
    from: SpaceRef,
    to: SpaceRef,
    /// In the space the project left: the `project.move` change in the space it went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change: Option<ChangeId>,
    nodes: Vec<NodeId>,
    #[serde(default)]
    folders: Vec<String>,
    #[serde(default)]
    crossing: Vec<CrossingLink>,
}

impl MoveDetail {
    fn read(change: &Change) -> Result<MoveDetail> {
        serde_json::from_value(change.detail.clone()).map_err(|e| {
            Error::Invalid(format!("change {} does not record a move: {e}", change.id))
        })
    }
}

impl Space {
    /// What moving `project` to `target` would do, writing nothing: the projects and nodes that
    /// move, their versions, attachments and links, the links that would cross spaces (which
    /// do not), the project folders that move, what stays and why, and any conflict that stops
    /// the move.
    ///
    /// The project moves with every project `part-of` it, at any depth, and every node `part-of`
    /// any of them. Canvases stay: their views belong to the space.
    pub fn preview_move(&self, project: &NodeId, target: &Space) -> Result<MovePreview> {
        Ok(self.plan_move(project, target)?.preview)
    }

    /// Moves a project, the projects inside it and the nodes that are part of them to `target`,
    /// as [`preview_move`](Self::preview_move) describes, and returns what moved.
    ///
    /// Node ids, kinds, titles, bodies, tags, properties, attachments (with their files), Loro
    /// documents, every version in the history (with who made it and when) and the links among
    /// the moved nodes go to `target`, in one `project.move` change there. In this space the
    /// nodes are deleted in one `project.move` change, so their history stays here too and
    /// [`undo_move`](Self::undo_move) can bring them back. Links between a moved node and one
    /// that stays are removed (typed links) or stay as their text (`[[links]]`). Project folders
    /// that hold a moved project move with it, so conversations held in them are captured into
    /// `target`.
    ///
    /// `target` is written first: if writing this space then fails, what was written there is
    /// removed again, and nothing is lost either way. Only a person can move a project; it fails
    /// with [`Error::PersonOnly`] for anyone else, and with [`Error::Invalid`] when the preview
    /// has conflicts.
    pub fn move_project(
        &mut self,
        project: &NodeId,
        target: &mut Space,
        by: &Provenance,
    ) -> Result<MovedProject> {
        if by.kind != ActorKind::Person {
            return Err(Error::PersonOnly("move a project to another space".into()));
        }
        let plan = self.plan_move(project, target)?;
        if !plan.preview.conflicts.is_empty() {
            return Err(Error::Invalid(format!(
                "{} cannot move to {}: {}",
                plan.preview.project.title,
                plan.preview.to.name,
                plan.preview.conflicts.join("; ")
            )));
        }
        let now = Timestamp::now();
        let to_change = copy_into(self, target, &plan, by, now)?;
        match self.move_out(&plan, to_change, by, now) {
            Ok(from_change) => Ok(MovedProject {
                preview: plan.preview,
                from_change,
                to_change,
            }),
            Err(error) => {
                // Nothing may be in both spaces: take back what the target got.
                let _ = remove_moved(target, &plan.nodes, &plan.preview.folders, None, by, now);
                Err(error)
            }
        }
    }

    /// Undoes a move, given its `project.move` change in this space, which may be either the
    /// space the project left or the one it went to; `other` is the other space. Returns both
    /// `undo` changes.
    ///
    /// The nodes come back to the space they left, as they were, with their links, properties
    /// and folders, and are removed from the other space with their history there, as the move
    /// had never happened: content that goes back does not stay behind. Fails with
    /// [`Error::UndoConflict`] when the moved nodes changed since in either space (move the
    /// project back instead), with [`Error::AlreadyUndone`] when the move was undone, and with
    /// [`Error::CannotUndo`] when `change` is not a move between these two spaces. Only a person
    /// can undo a move.
    pub fn undo_move(
        &mut self,
        change: ChangeId,
        other: &mut Space,
        by: &Provenance,
    ) -> Result<UndoneMove> {
        if by.kind != ActorKind::Person {
            return Err(Error::PersonOnly("undo a move between spaces".into()));
        }
        let found = read_change(&self.conn, change)?.ok_or(Error::ChangeNotFound(change))?;
        let cannot = |reason: String| Error::CannotUndo { change, reason };
        if found.action != actions::PROJECT_MOVE {
            return Err(cannot(format!(
                "it is a {} change, not a move",
                found.action
            )));
        }
        let detail = MoveDetail::read(&found)?;
        let mine = &self.manifest().id;
        let theirs = &other.manifest().id;
        let (source, target, from_change) = if &detail.from.id == mine && &detail.to.id == theirs {
            let to_change = detail
                .change
                .ok_or_else(|| cannot("it does not name the change in the other space".into()))?;
            (self, other, (change, to_change))
        } else if &detail.to.id == mine && &detail.from.id == theirs {
            let from_change = find_move_out(other, change)?.ok_or_else(|| {
                cannot(format!(
                    "{} has no record of this move",
                    other.manifest().name
                ))
            })?;
            (other, self, (from_change, change))
        } else {
            return Err(cannot(format!(
                "it moved a project from {} to {}, not between these spaces",
                detail.from.name, detail.to.name
            )));
        };
        let (from_change, to_change) = from_change;
        undo_move_between(source, target, from_change, to_change, by)
    }

    /// Works out a move.
    fn plan_move(&self, project: &NodeId, target: &Space) -> Result<Plan> {
        let root = read_node(&self.conn, project)?.ok_or(Error::NotFound(*project))?;
        if root.kind != kinds::PROJECT {
            return Err(Error::Invalid(format!(
                "{} is a {}, not a project; only projects move between spaces",
                root.title, root.kind
            )));
        }
        let mut conflicts = Vec::new();
        if self.manifest().id == target.manifest().id {
            conflicts.push("it is already in that space".to_string());
        }

        // The projects inside it, at any depth, and the nodes part of any of them.
        let mut projects = vec![*project];
        let mut seen = BTreeSet::from([*project]);
        let mut stays = Vec::new();
        let mut next = 0;
        while next < projects.len() {
            let parent = projects[next];
            next += 1;
            let mut stmt = self.conn.prepare_cached(
                "SELECT n.id, n.kind, n.title FROM links l JOIN nodes n ON n.id = l.source_id
                 WHERE l.target_id = ?1 AND l.kind = ?2 ORDER BY n.seq",
            )?;
            let parts: Vec<(String, String, String)> = stmt
                .query_map(params![parent.to_string(), link_kinds::PART_OF], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, kind, title) in parts {
                let id = NodeId::parse(&id)?;
                if kind == kinds::CANVAS {
                    if !stays.iter().any(|s: &Staying| s.node == Some(id)) {
                        stays.push(Staying {
                            what: title,
                            node: Some(id),
                            reason: "a canvas stays: its board belongs to the space".into(),
                        });
                    }
                    continue;
                }
                if !seen.insert(id) {
                    continue;
                }
                if kind == kinds::PROJECT {
                    projects.push(id);
                }
            }
        }
        let mut nodes: Vec<(i64, NodeId)> = Vec::new();
        for id in &seen {
            let seq: i64 = self.conn.query_row(
                "SELECT seq FROM nodes WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            nodes.push((seq, *id));
        }
        nodes.sort();
        let nodes: Vec<NodeId> = nodes.into_iter().map(|(_, id)| id).collect();
        let moving: BTreeSet<NodeId> = nodes.iter().copied().collect();

        // Nodes the target has, or had.
        let mut returning = Vec::new();
        for id in &nodes {
            if read_node(&target.conn, id)?.is_some() {
                conflicts.push(format!(
                    "{} already has {} ({id})",
                    target.manifest().name,
                    title_of(&self.conn, id)?
                ));
                continue;
            }
            let last: Option<(String, Option<String>)> = target
                .conn
                .query_row(
                    "SELECT h.op, c.action FROM history h LEFT JOIN changes c ON c.id = h.change_id
                     WHERE h.node_id = ?1 ORDER BY h.seq DESC LIMIT 1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match last {
                None => {}
                Some((op, Some(action)))
                    if op == ChangeOp::Delete.as_str() && action == actions::PROJECT_MOVE =>
                {
                    returning.push(*id);
                }
                Some(_) => conflicts.push(format!(
                    "{} has a deleted node with the id of {} ({id}); recover or purge it there first",
                    target.manifest().name,
                    title_of(&self.conn, id)?
                )),
            }
        }

        // Links: kept among the nodes that move, or crossing to a node that stays.
        let mut kept = Vec::new();
        let mut crossing = Vec::new();
        let mut links_seen = BTreeSet::new();
        for id in &nodes {
            let mut links = self.links_from(id)?;
            links.extend(self.backlinks(id)?);
            for link in links {
                let key = (link.from, link.kind.clone(), link.to);
                if !links_seen.insert(key.clone()) {
                    continue;
                }
                let (from_moves, to_moves) =
                    (moving.contains(&link.from), moving.contains(&link.to));
                if from_moves && to_moves {
                    kept.push(key);
                } else {
                    crossing.push(CrossingLink {
                        from: link.from,
                        from_title: link.from_title,
                        kind: link.kind,
                        to: link.to,
                        to_title: link.to_title,
                        from_moves,
                    });
                }
            }
        }

        // Attachments, each file counted once.
        let mut attachments = Vec::new();
        let mut files = BTreeMap::new();
        for id in &nodes {
            for attachment in self.attachments(id)? {
                if !blob_file(self.path(), &attachment.hash).is_file() {
                    stays.push(Staying {
                        what: attachment.name,
                        node: Some(*id),
                        reason: "its file is not in this space on this device".into(),
                    });
                    continue;
                }
                files.insert(attachment.hash.clone(), attachment.size);
                attachments.push((*id, attachment.name, attachment.hash));
            }
        }

        // Project folders that hold a moved project.
        let mut folders = Vec::new();
        let target_folders = target.folders()?;
        for (folder, held) in self.folders_with_projects()? {
            let Some(held) = held.filter(|p| moving.contains(&p.id)) else {
                continue;
            };
            if folder.kind != folder_kinds::PROJECT {
                continue;
            }
            if target_folders
                .iter()
                .any(|f| f.id == folder.id || (f.kind == folder.kind && f.path == folder.path))
            {
                stays.push(Staying {
                    what: folder.path.clone(),
                    node: Some(held.id),
                    reason: format!(
                        "{} already has this folder; its conversations go to both spaces",
                        target.manifest().name
                    ),
                });
                continue;
            }
            folders.push((folder, held.id));
        }

        let versions: i64 = {
            let mut total = 0;
            for id in &nodes {
                total += self.conn.query_row(
                    "SELECT count(*) FROM history WHERE node_id = ?1",
                    [id.to_string()],
                    |row| row.get::<_, i64>(0),
                )?;
            }
            total
        };
        let summaries = |ids: &[NodeId]| -> Result<Vec<NodeSummary>> {
            let mut out = Vec::new();
            for id in ids {
                out.extend(self.query_summaries("WHERE n.id = ?1", [id.to_string()])?);
            }
            Ok(out)
        };
        let project_summary = summaries(&[*project])?
            .pop()
            .ok_or(Error::NotFound(*project))?;
        let preview = MovePreview {
            project: project_summary,
            from: SpaceRef::of(self),
            to: SpaceRef::of(target),
            projects: summaries(&projects)?,
            notes: (nodes.len() - projects.len()) as u64,
            versions: versions.try_into().unwrap_or(0),
            attachments: attachments.len() as u64,
            attachment_bytes: files.values().sum(),
            links_kept: kept.len() as u64,
            crossing,
            folders: folders.iter().map(|(f, _)| f.clone()).collect(),
            stays,
            conflicts,
        };
        Ok(Plan {
            preview,
            nodes,
            kept,
            folders,
            attachments,
            returning,
        })
    }

    /// Deletes the moved nodes and folders from this space, in one `project.move` change.
    fn move_out(
        &mut self,
        plan: &Plan,
        to_change: ChangeId,
        by: &Provenance,
        now: Timestamp,
    ) -> Result<ChangeId> {
        let detail = move_detail(plan, Some(to_change));
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change_with_detail(
            &tx,
            now,
            prov,
            actions::PROJECT_MOVE,
            &plan.preview.project.id.to_string(),
            None,
            &detail,
        )?;
        let write = Write { prov, now, change };
        for id in &plan.nodes {
            if let Some(node) = read_node(&tx, id)? {
                delete_in(&tx, id, &node.title, &write)?;
            }
        }
        for (folder, _) in &plan.folders {
            tx.execute("DELETE FROM folders WHERE id = ?1", [&folder.id])?;
        }
        tx.commit()?;
        Ok(change)
    }
}

fn move_detail(plan: &Plan, change: Option<ChangeId>) -> String {
    let detail = MoveDetail {
        from: plan.preview.from.clone(),
        to: plan.preview.to.clone(),
        change,
        nodes: plan.nodes.clone(),
        folders: plan.folders.iter().map(|(f, _)| f.id.clone()).collect(),
        crossing: plan.preview.crossing.clone(),
    };
    serde_json::to_string(&detail).unwrap_or_else(|_| "{}".into())
}

fn title_of(conn: &Connection, id: &NodeId) -> Result<String> {
    Ok(read_node(conn, id)?.map_or_else(|| id.to_string(), |n| n.title))
}

/// Provenance rows of one space, as rows of another.
struct ProvMap<'a> {
    from: &'a Connection,
    ids: HashMap<i64, i64>,
}

impl<'a> ProvMap<'a> {
    fn new(from: &'a Connection) -> Self {
        ProvMap {
            from,
            ids: HashMap::new(),
        }
    }

    fn get(&mut self, tx: &Transaction<'_>, id: i64) -> Result<i64> {
        if let Some(mapped) = self.ids.get(&id) {
            return Ok(*mapped);
        }
        let prov = self.from.query_row(
            "SELECT actor_kind, actor, session, source FROM provenance WHERE id = ?1",
            [id],
            |row| provenance(row, 0),
        )?;
        let mapped = provenance_id(tx, &prov)?;
        self.ids.insert(id, mapped);
        Ok(mapped)
    }
}

/// Writes everything that moves into `target`, in one `project.move` change, and returns it.
/// Files (attachments and Loro documents) are copied before the rows that name them.
#[allow(clippy::type_complexity)]
fn copy_into(
    source: &Space,
    target: &mut Space,
    plan: &Plan,
    by: &Provenance,
    now: Timestamp,
) -> Result<ChangeId> {
    for (_, _, hash) in &plan.attachments {
        copy_blob(source.path(), target.path(), hash)?;
    }
    for id in &plan.nodes {
        let from = source.loro_dir(id);
        if from.is_dir() {
            copy_dir(&from, &target.loro_dir(id))?;
        }
    }
    let src = &source.conn;
    let mut provs = ProvMap::new(src);
    let detail = move_detail(plan, None);
    let tx = target.conn.transaction()?;
    let prov = provenance_id(&tx, by)?;
    let change = add_change_with_detail(
        &tx,
        now,
        prov,
        actions::PROJECT_MOVE,
        &plan.preview.project.id.to_string(),
        None,
        &detail,
    )?;

    // The old history of nodes that come back is replaced by the history they bring, which has
    // every version they had here.
    for id in &plan.returning {
        let id = id.to_string();
        tx.execute(
            "DELETE FROM link_history WHERE source_id = ?1 OR target_id = ?1",
            [&id],
        )?;
        tx.execute("DELETE FROM history WHERE node_id = ?1", [&id])?;
        tx.execute("DELETE FROM attachments WHERE node_id = ?1", [&id])?;
    }

    // Nodes, with their tags.
    for id in &plan.nodes {
        let key = id.to_string();
        let row: (String, String, String, String, String, i64, i64, i64, i64) = src.query_row(
            "SELECT kind, title, title_key, body, plain, created_at, updated_at, created_by,
                    updated_by FROM nodes WHERE id = ?1",
            [&key],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )?;
        let created_by = provs.get(&tx, row.7)?;
        let updated_by = provs.get(&tx, row.8)?;
        tx.execute(
            "INSERT INTO nodes (id, kind, title, title_key, body, plain, created_at, updated_at,
                                created_by, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                key, row.0, row.1, row.2, row.3, row.4, row.5, row.6, created_by, updated_by
            ],
        )?;
        tx.execute(
            "INSERT INTO tags (node_id, tag) SELECT ?1, value FROM json_each(?2)",
            params![
                key,
                serde_json::to_string(&crate::space::tags_of(src, id)?)
                    .unwrap_or_else(|_| "[]".into())
            ],
        )?;
    }

    // Link kinds the kept links need, then the links.
    let mut needed = BTreeSet::new();
    for (_, kind, _) in &plan.kept {
        if !link_kinds::BUILT_IN.contains(&kind.as_str()) {
            needed.insert(kind.clone());
        }
    }
    for kind in needed {
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM link_kinds WHERE kind = ?1)",
            [&kind],
            |row| row.get(0),
        )?;
        if exists {
            continue;
        }
        let row: Option<(String, String, String, String, i64, i64, i64, i64)> = src
            .query_row(
                "SELECT label, inverse_label, direction, description, created_at, created_by,
                        updated_at, updated_by FROM link_kinds WHERE kind = ?1",
                [&kind],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;
        if let Some(row) = row {
            let created_by = provs.get(&tx, row.5)?;
            let updated_by = provs.get(&tx, row.7)?;
            tx.execute(
                "INSERT INTO link_kinds (kind, label, inverse_label, direction, description,
                                         created_at, created_by, updated_at, updated_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    kind, row.0, row.1, row.2, row.3, row.4, created_by, row.6, updated_by
                ],
            )?;
        }
    }
    for (from, kind, to) in &plan.kept {
        let (created_at, created_by): (i64, i64) = src.query_row(
            "SELECT created_at, created_by FROM links
             WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3",
            params![from.to_string(), kind, to.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let created_by = provs.get(&tx, created_by)?;
        tx.execute(
            "INSERT OR IGNORE INTO links (source_id, kind, target_id, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                from.to_string(),
                kind,
                to.to_string(),
                created_at,
                created_by
            ],
        )?;
    }

    // `[[links]]` waiting for a title: those that waited before, and those that pointed at a
    // node that stays, which wait for its title as the author wrote it.
    for id in &plan.nodes {
        let mut stmt = src.prepare_cached(
            "SELECT target_key, target FROM unresolved_links WHERE source_id = ?1",
        )?;
        let waiting: Vec<(String, String)> = stmt
            .query_map([id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (key, target) in waiting {
            tx.execute(
                "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
                 VALUES (?1, ?2, ?3)",
                params![id.to_string(), key, target],
            )?;
        }
    }
    for link in &plan.preview.crossing {
        if link.from_moves && link.kind == link_kinds::MENTIONS {
            tx.execute(
                "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
                 VALUES (?1, ?2, ?3)",
                params![
                    link.from.to_string(),
                    knode_md::title_key(&link.to_title),
                    link.to_title
                ],
            )?;
        }
    }

    // Properties.
    for id in &plan.nodes {
        let mut stmt = src.prepare_cached(
            "SELECT key, value, updated_at, updated_by FROM properties WHERE node_id = ?1",
        )?;
        let properties: Vec<(String, String, i64, i64)> = stmt
            .query_map([id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (key, value, updated_at, updated_by) in properties {
            let updated_by = provs.get(&tx, updated_by)?;
            tx.execute(
                "INSERT INTO properties (node_id, key, value, updated_at, updated_by)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.to_string(), key, value, updated_at, updated_by],
            )?;
            if key == property_keys::MARKDOWN_PATH {
                resolve_waiting_paths(&tx, id, &value, prov, now)?;
            }
        }
    }

    // Every version, in order, with who made it and when; all part of the move.
    let mut seqs: HashMap<i64, i64> = HashMap::new();
    for id in &plan.nodes {
        let mut stmt = src.prepare_cached(
            "SELECT seq, op, changed_at, changed_by, kind, title, body, tags, restored_from
             FROM history WHERE node_id = ?1 ORDER BY seq",
        )?;
        let rows: Vec<(
            i64,
            String,
            i64,
            i64,
            String,
            String,
            String,
            String,
            Option<i64>,
        )> = stmt
            .query_map([id.to_string()], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (seq, op, changed_at, changed_by, kind, title, body, tags, restored_from) in rows {
            let changed_by = provs.get(&tx, changed_by)?;
            let restored_from = restored_from.and_then(|r| seqs.get(&r).copied());
            tx.execute(
                "INSERT INTO history (node_id, op, changed_at, changed_by, kind, title, body,
                                      tags, change_id, restored_from)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id.to_string(),
                    op,
                    changed_at,
                    changed_by,
                    kind,
                    title,
                    body,
                    tags,
                    change,
                    restored_from
                ],
            )?;
            seqs.insert(seq, tx.last_insert_rowid());
        }
    }
    // The history of the links among them.
    let moving: BTreeSet<String> = plan.nodes.iter().map(ToString::to_string).collect();
    {
        let mut stmt =
            src.prepare("SELECT op, source_id, kind, target_id FROM link_history ORDER BY seq")?;
        let rows: Vec<(String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (op, from, kind, to) in rows {
            if moving.contains(&from) && moving.contains(&to) {
                tx.execute(
                    "INSERT INTO link_history (change_id, op, source_id, kind, target_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![change, op, from, kind, to],
                )?;
            }
        }
    }

    // Attachments, whose files were copied first.
    for (node, name, hash) in &plan.attachments {
        let (size, media_type, blob_at, blob_by): (i64, String, i64, i64) = src.query_row(
            "SELECT size, media_type, created_at, created_by FROM blobs WHERE hash = ?1",
            [hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let blob_by = provs.get(&tx, blob_by)?;
        tx.execute(
            "INSERT OR IGNORE INTO blobs (hash, size, media_type, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![hash, size, media_type, blob_at, blob_by],
        )?;
        let (created_at, created_by): (i64, i64) = src.query_row(
            "SELECT created_at, created_by FROM attachments WHERE node_id = ?1 AND name = ?2",
            params![node.to_string(), name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let created_by = provs.get(&tx, created_by)?;
        tx.execute(
            "INSERT OR REPLACE INTO attachments (node_id, name, hash, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node.to_string(), name, hash, created_at, created_by],
        )?;
    }

    // Loro documents: the version each body holds, and who their peers are.
    for id in &plan.nodes {
        let doc: Option<(String, i64)> = src
            .query_row(
                "SELECT version, updated_at FROM loro_docs WHERE node_id = ?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((version, updated_at)) = doc else {
            continue;
        };
        tx.execute(
            "INSERT OR REPLACE INTO loro_docs (node_id, version, updated_at) VALUES (?1, ?2, ?3)",
            params![id.to_string(), version, updated_at],
        )?;
        let peers: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&version).unwrap_or_default();
        for peer in peers.keys() {
            let row: Option<(i64, i64)> = src
                .query_row(
                    "SELECT provenance, created_at FROM loro_peers WHERE peer = ?1",
                    [peer],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((peer_prov, created_at)) = row {
                let peer_prov = provs.get(&tx, peer_prov)?;
                tx.execute(
                    "INSERT OR IGNORE INTO loro_peers (peer, provenance, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![peer, peer_prov, created_at],
                )?;
            }
        }
    }

    // Project folders, each recording the project it holds.
    for (folder, held) in &plan.folders {
        let mut rules = folder.rules.clone();
        if let Some(object) = rules.as_object_mut() {
            object.insert(PROJECT_RULE.into(), held.to_string().into());
        }
        let created_by = provenance_id(&tx, &folder.created_by)?;
        tx.execute(
            "INSERT INTO folders (id, kind, path, capture, rules, created_at, created_by,
                                  updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                folder.id,
                folder.kind,
                folder.path,
                if folder.capture { "on" } else { "off" },
                rules.to_string(),
                folder.created_at.0,
                created_by,
                now.0,
                prov
            ],
        )?;
    }

    // Links in this space waiting for the titles that arrived (7.3).
    for id in &plan.nodes {
        let title: String = tx.query_row(
            "SELECT title FROM nodes WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        resolve_waiting_links(&tx, id, &title, prov, now)?;
    }
    tx.commit()?;
    Ok(change)
}

/// Copies a blob's file into another package, unless it is there.
fn copy_blob(from: &Path, to: &Path, hash: &str) -> Result<()> {
    let place = blob_file(to, hash);
    if place.is_file() {
        return Ok(());
    }
    let dir = place
        .parent()
        .ok_or_else(|| Error::Invalid(format!("no folder for blob {hash}")))?;
    fs::create_dir_all(dir)?;
    let temp = to.join(crate::BLOBS_DIR).join(format!(".tmp-move-{hash}"));
    fs::copy(blob_file(from, hash), &temp)?;
    fs::File::open(&temp)?.sync_all()?;
    if let Err(e) = fs::rename(&temp, &place) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

/// Copies a folder of Loro files, keeping what the destination has.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &dest)?;
        } else if !entry.file_name().to_string_lossy().starts_with('.') {
            let temp = to.join(format!(".move-{}", entry.file_name().to_string_lossy()));
            fs::copy(&path, &temp)?;
            fs::rename(&temp, &dest)?;
        }
    }
    Ok(())
}

/// The `project.move` change in the space a project left whose other half is `to_change`.
fn find_move_out(source: &Space, to_change: ChangeId) -> Result<Option<ChangeId>> {
    let mut stmt = source.conn.prepare(
        "SELECT id FROM changes WHERE action = ?1 AND json_extract(detail, '$.change') = ?2
         ORDER BY id DESC LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![actions::PROJECT_MOVE, to_change], |row| row.get(0))
        .optional()?)
}

/// The changes after `after` that touched any of `nodes`.
fn later_changes(
    conn: &Connection,
    nodes: &[NodeId],
    after: ChangeId,
) -> Result<BTreeSet<ChangeId>> {
    let mut later = BTreeSet::new();
    for id in nodes {
        let id = id.to_string();
        for sql in [
            "SELECT change_id FROM history WHERE node_id = ?1 AND change_id > ?2",
            "SELECT change_id FROM link_history
             WHERE (source_id = ?1 OR target_id = ?1) AND change_id > ?2",
            "SELECT id FROM changes WHERE subject = ?1 AND id > ?2",
        ] {
            let mut stmt = conn.prepare_cached(sql)?;
            let rows = stmt.query_map(params![id, after], |row| row.get::<_, Option<i64>>(0))?;
            for row in rows {
                if let Some(change) = row? {
                    later.insert(change);
                }
            }
        }
    }
    Ok(later)
}

fn undo_move_between(
    source: &mut Space,
    target: &mut Space,
    from_change: ChangeId,
    to_change: ChangeId,
    by: &Provenance,
) -> Result<UndoneMove> {
    for (space, change) in [(&*source, from_change), (&*target, to_change)] {
        if let Some(undone_by) = active_undo(&space.conn, change)? {
            return Err(Error::AlreadyUndone { change, undone_by });
        }
    }
    let found =
        read_change(&source.conn, from_change)?.ok_or(Error::ChangeNotFound(from_change))?;
    let detail = MoveDetail::read(&found)?;
    let project = NodeId::parse(&found.subject)?;
    let nodes = detail.nodes.clone();

    // Later work on the moved nodes, in either space, would be lost.
    let mut later = later_changes(&target.conn, &nodes, to_change)?;
    later.extend(
        later_changes(&source.conn, &nodes, from_change)?
            .into_iter()
            .filter(|c| *c != from_change),
    );
    for id in &nodes {
        if read_node(&source.conn, id)?.is_some() {
            return Err(Error::CannotUndo {
                change: from_change,
                reason: format!("{id} is in {} again", source.manifest().name),
            });
        }
    }
    if !later.is_empty() {
        return Err(Error::UndoConflict {
            change: to_change,
            later: later.into_iter().collect(),
        });
    }
    let folders: Vec<Folder> = detail
        .folders
        .iter()
        .filter_map(|id| target.folder(id).transpose())
        .collect::<Result<_>>()?;

    // Back in the space the project left, from the versions its move deleted.
    let now = Timestamp::now();
    let from_undo = {
        let mut provs = ProvMap::new(&target.conn);
        let tx = source.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(
            &tx,
            now,
            prov,
            actions::UNDO,
            &project.to_string(),
            Some(from_change),
        )?;
        let write = Write { prov, now, change };
        for id in &nodes {
            let deleted = query_history(
                &tx,
                "h.node_id = ?1 AND h.change_id = ?2 AND h.op = 'delete'",
                params![id.to_string(), from_change],
            )?
            .pop();
            if let Some(deleted) = deleted {
                put_version(&tx, id, &Content::of(&deleted), deleted.seq, &write, false)?;
            }
        }
        let removed = crate::history::link_history_of(&tx, from_change)?;
        for entry in removed.iter().filter(|e| e.op == ChangeOp::Delete) {
            if !link_exists(&tx, &entry.from, &entry.kind, &entry.to)?
                && read_node(&tx, &entry.from)?.is_some()
                && read_node(&tx, &entry.to)?.is_some()
            {
                insert_link(&tx, &entry.from, &entry.kind, &entry.to, &write)?;
            }
        }
        for id in &nodes {
            let mut stmt = target.conn.prepare_cached(
                "SELECT key, value, updated_at, updated_by FROM properties WHERE node_id = ?1",
            )?;
            let properties: Vec<(String, String, i64, i64)> = stmt
                .query_map([id.to_string()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (key, value, updated_at, updated_by) in properties {
                let updated_by = provs.get(&tx, updated_by)?;
                tx.execute(
                    "INSERT OR REPLACE INTO properties (node_id, key, value, updated_at, updated_by)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id.to_string(), key, value, updated_at, updated_by],
                )?;
                if key == property_keys::MARKDOWN_PATH {
                    resolve_waiting_paths(&tx, id, &value, prov, now)?;
                }
            }
        }
        for folder in &folders {
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM folders WHERE id = ?1 OR (kind = ?2 AND path = ?3))",
                params![folder.id, folder.kind, folder.path],
                |row| row.get(0),
            )?;
            if taken {
                continue;
            }
            let created_by = provenance_id(&tx, &folder.created_by)?;
            tx.execute(
                "INSERT INTO folders (id, kind, path, capture, rules, created_at, created_by,
                                      updated_at, updated_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    folder.id,
                    folder.kind,
                    folder.path,
                    if folder.capture { "on" } else { "off" },
                    folder.rules.to_string(),
                    folder.created_at.0,
                    created_by,
                    now.0,
                    prov
                ],
            )?;
        }
        tx.commit()?;
        change
    };

    // Out of the space it went to, with what it brought there.
    let to_undo = remove_moved(
        target,
        &nodes,
        &folders,
        Some((to_change, project)),
        by,
        now,
    )?;
    Ok(UndoneMove {
        project,
        from: detail.from,
        to: detail.to,
        from_change: read_change(&source.conn, from_undo)?
            .ok_or(Error::ChangeNotFound(from_undo))?,
        to_change: read_change(&target.conn, to_undo)?.ok_or(Error::ChangeNotFound(to_undo))?,
        nodes,
    })
}

/// Removes moved nodes from the space they went to, with their history, links, attachments,
/// Loro documents and folders there, as a purge does. With `undoes`, records an `undo` of the
/// move; without, a failed move is being taken back and the move change is left without
/// content. Returns the change.
fn remove_moved(
    target: &mut Space,
    nodes: &[NodeId],
    folders: &[Folder],
    undoes: Option<(ChangeId, NodeId)>,
    by: &Provenance,
    now: Timestamp,
) -> Result<ChangeId> {
    let tx = target.conn.transaction()?;
    let prov = provenance_id(&tx, by)?;
    let change = match undoes {
        Some((undone, project)) => add_change(
            &tx,
            now,
            prov,
            actions::UNDO,
            &project.to_string(),
            Some(undone),
        )?,
        None => 0,
    };
    let mut blobs = Vec::new();
    for id in nodes {
        let key = id.to_string();
        if let Some(node) = read_node(&tx, id)? {
            tx.execute(
                "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
                 SELECT source_id, ?2, ?3 FROM links
                 WHERE target_id = ?1 AND kind = ?4 AND source_id <> ?1",
                params![
                    key,
                    knode_md::title_key(&node.title),
                    node.title,
                    link_kinds::MENTIONS
                ],
            )?;
        }
    }
    for id in nodes {
        let key = id.to_string();
        tx.execute("DELETE FROM nodes WHERE id = ?1", [&key])?;
        tx.execute(
            "DELETE FROM link_history WHERE source_id = ?1 OR target_id = ?1",
            [&key],
        )?;
        tx.execute("DELETE FROM history WHERE node_id = ?1", [&key])?;
        purge_node_views(&tx, id)?;
        blobs.extend(purge_node_attachments(&tx, id)?);
    }
    for folder in folders {
        tx.execute("DELETE FROM folders WHERE id = ?1", [&folder.id])?;
    }
    tx.commit()?;
    remove_blob_files(target.path(), &blobs);
    for id in nodes {
        let _ = fs::remove_dir_all(target.loro_dir(id));
    }
    Ok(change)
}

/// Whether a node was last deleted by a move, and to which space: such a node lives on there,
/// and is not restored here.
pub(crate) fn moved_away(conn: &Connection, id: &NodeId) -> Result<Option<String>> {
    let found: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT h.op, c.action, c.detail FROM history h JOIN changes c ON c.id = h.change_id
             WHERE h.node_id = ?1 ORDER BY h.seq DESC LIMIT 1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(match found {
        Some((op, Some(action), detail))
            if op == ChangeOp::Delete.as_str() && action == actions::PROJECT_MOVE =>
        {
            let detail: serde_json::Value = serde_json::from_str(&detail).unwrap_or_default();
            Some(
                detail
                    .pointer("/to/name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("another space")
                    .to_string(),
            )
        }
        _ => None,
    })
}

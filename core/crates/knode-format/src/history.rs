//! Change history: changes, undo, restore and purge (schema version 3).

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::space::{
    Content, Fields, Write, add_change, apply_fields, delete_in, finish_rename, insert_link,
    insert_node, limit_param, link_exists, node_id, provenance, provenance_id, query_history,
    read_node, record, remove_link,
};
use crate::types::{ActorKind, ChangeOp};
use crate::{
    Change, ChangeDetail, ChangeId, Error, HistoryEntry, LinkHistoryEntry, Node, NodeId,
    Provenance, Result, Space, Timestamp, actions, link_kinds,
};

/// The actions [`Space::undo`] can undo.
const UNDOABLE: &[&str] = &[
    actions::NODE_CREATE,
    actions::NODE_UPDATE,
    actions::NODE_DELETE,
    actions::NODE_RESTORE,
    actions::LINK_CREATE,
    actions::LINK_DELETE,
    actions::UNDO,
    actions::IMPORT,
];

/// Columns of a [`Change`], with `c` as `changes` and `p` as its provenance.
const CHANGE_COLUMNS: &str = "c.id, c.changed_at, p.actor_kind, p.actor, p.session, p.source, \
     c.action, c.subject, c.undoes, c.detail";

impl Space {
    /// The changes of the space, newest first: at most `limit`, and only those older than
    /// `before` when it is set, so a long history can be read page by page.
    pub fn changes(&self, limit: usize, before: Option<ChangeId>) -> Result<Vec<Change>> {
        query_changes(
            &self.conn,
            "c.id < ?1 ORDER BY c.id DESC LIMIT ?2",
            params![before.unwrap_or(i64::MAX), limit_param(limit)],
        )
    }

    /// The changes made to one subject, such as a node id, newest first, at most `limit`.
    /// Links made or removed by hand are changes of their source node.
    pub fn changes_of(&self, subject: &str, limit: usize) -> Result<Vec<Change>> {
        query_changes(
            &self.conn,
            "c.subject = ?1 ORDER BY c.id DESC LIMIT ?2",
            params![subject, limit_param(limit)],
        )
    }

    /// A change with the versions of nodes and the links it wrote, or `None` if no change has
    /// this id.
    pub fn change(&self, id: ChangeId) -> Result<Option<ChangeDetail>> {
        let Some(change) = read_change(&self.conn, id)? else {
            return Ok(None);
        };
        Ok(Some(ChangeDetail {
            change,
            versions: query_history(&self.conn, "h.change_id = ?1", [id])?,
            links: query_link_history(&self.conn, "change_id = ?1", [id])?,
        }))
    }

    /// The links made or removed by hand from or to a node, oldest first. Works for deleted
    /// nodes.
    pub fn link_history(&self, id: &NodeId) -> Result<Vec<LinkHistoryEntry>> {
        query_link_history(
            &self.conn,
            "source_id = ?1 OR target_id = ?1",
            [id.to_string()],
        )
    }

    /// Undoes a change and returns the new change that undoes it.
    ///
    /// Every node the change wrote goes back to its version just before it: a node it created
    /// is deleted, a node it deleted is created again with the same id, and any other node is
    /// updated. Links it made by hand are removed and links it removed are made again (when both
    /// nodes exist). A title put back does not rewrite the bodies that link to it, because those
    /// bodies are put back from their own versions. Undoing an undo redoes the change.
    ///
    /// Fails with [`Error::UndoConflict`] when later changes touched the same nodes or links,
    /// unless `force` is set: undoing would overwrite them. Confirm with the person before
    /// forcing; [`restore`](Self::restore) goes back past later changes on purpose. Fails with
    /// [`Error::AlreadyUndone`] for a change already undone, and with [`Error::CannotUndo`] for
    /// changes of settings, folders, agents and grants, or when the versions it needs were
    /// purged. Nothing is ever removed from the history.
    pub fn undo(&mut self, id: ChangeId, by: &Provenance, force: bool) -> Result<Change> {
        let plan = plan_undo(&self.conn, id, force)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::UNDO, &plan.subject, Some(id))?;
        let write = Write { prov, now, change };

        // Versions first, so links can be made again between nodes that come back.
        for step in &plan.nodes {
            if let Step::Put {
                node,
                content,
                from,
            } = step
            {
                put_version(&tx, node, content, *from, &write, false)?;
            }
        }
        for entry in plan.links.iter().rev() {
            let exists = link_exists(&tx, &entry.from, &entry.kind, &entry.to)?;
            match entry.op {
                ChangeOp::Create if exists => {
                    remove_link(&tx, &entry.from, &entry.kind, &entry.to, change)?
                }
                ChangeOp::Delete if !exists && both_exist(&tx, entry)? => {
                    insert_link(&tx, &entry.from, &entry.kind, &entry.to, &write)?
                }
                _ => {}
            }
        }
        for step in &plan.nodes {
            if let Step::Delete { node } = step {
                if let Some(current) = read_node(&tx, node)? {
                    delete_in(&tx, node, &current.title, &write)?;
                }
            }
        }
        tx.commit()?;
        read_change(&self.conn, change)?.ok_or(Error::ChangeNotFound(change))
    }

    /// Puts a node back to one of its versions (`seq` of a row of its
    /// [`history`](Self::history)) and returns it.
    ///
    /// Written as an ordinary update: a restored title renames the node and rewrites the bodies
    /// that link to it. A deleted node is recovered with the same id, so links by id and
    /// `[[links]]` waiting for its title come back, and so do the links other than `mentions`
    /// that its delete removed, when both nodes exist. Its properties are not part of the history
    /// and do not come back. Restoring the version the node already has writes nothing.
    pub fn restore(&mut self, id: &NodeId, seq: i64, by: &Provenance) -> Result<Node> {
        let version = query_history(
            &self.conn,
            "h.node_id = ?1 AND h.seq = ?2",
            params![id.to_string(), seq],
        )?
        .pop()
        .ok_or_else(|| Error::Invalid(format!("node {id} has no version {seq}")))?;
        let content = Content::of(&version);
        let current = self.get_node(id)?;
        if let Some(current) = &current {
            if Fields::to(current, &content).is_empty() {
                return Ok(current.clone());
            }
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::NODE_RESTORE, &id.to_string(), None)?;
        let write = Write { prov, now, change };
        let deleted_in: Option<ChangeId> = if current.is_none() {
            tx.query_row(
                "SELECT change_id FROM history WHERE node_id = ?1 AND op = 'delete'
                 ORDER BY seq DESC LIMIT 1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten()
        } else {
            None
        };
        put_version(&tx, id, &content, seq, &write, true)?;
        if let Some(deleted_in) = deleted_in {
            let removed = query_link_history(
                &tx,
                "change_id = ?1 AND op = 'delete' AND (source_id = ?2 OR target_id = ?2)",
                params![deleted_in, id.to_string()],
            )?;
            for entry in removed {
                if !link_exists(&tx, &entry.from, &entry.kind, &entry.to)?
                    && both_exist(&tx, &entry)?
                {
                    insert_link(&tx, &entry.from, &entry.kind, &entry.to, &write)?;
                }
            }
        }
        tx.commit()?;
        self.get_node(id)?.ok_or(Error::NotFound(*id))
    }

    /// Erases a node and every version of it for good, such as after pasting a secret into it,
    /// and returns the change that records it, which holds no content.
    ///
    /// Deletes the node if it exists, its history and the history of its links. The changes
    /// made to it stay, without content. A purged node cannot be restored, and undoing a change
    /// that needs its versions fails. Only a person can purge: any other provenance fails with
    /// [`Error::PersonOnly`].
    pub fn purge(&mut self, id: &NodeId, by: &Provenance) -> Result<Change> {
        if by.kind != ActorKind::Person {
            return Err(Error::PersonOnly("purge a node and its history".into()));
        }
        let node = self.get_node(id)?;
        let has_history: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM history WHERE node_id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if node.is_none() && !has_history {
            return Err(Error::NotFound(*id));
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::NODE_PURGE, &id.to_string(), None)?;
        if let Some(node) = node {
            tx.execute(
                "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
                 SELECT source_id, ?2, ?3 FROM links
                 WHERE target_id = ?1 AND kind = ?4 AND source_id <> ?1",
                params![
                    id.to_string(),
                    knode_md::title_key(&node.title),
                    node.title,
                    link_kinds::MENTIONS
                ],
            )?;
            tx.execute("DELETE FROM nodes WHERE id = ?1", [id.to_string()])?;
        }
        tx.execute("DELETE FROM history WHERE node_id = ?1", [id.to_string()])?;
        tx.execute(
            "DELETE FROM link_history WHERE source_id = ?1 OR target_id = ?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        read_change(&self.conn, change)?.ok_or(Error::ChangeNotFound(change))
    }
}

/// What an undo does to one node.
enum Step {
    /// Put a version back, creating the node again if it does not exist.
    Put {
        node: NodeId,
        content: Content,
        from: i64,
    },
    /// Delete the node, if it exists.
    Delete { node: NodeId },
}

/// Everything an undo will do, worked out before writing.
struct UndoPlan {
    subject: String,
    nodes: Vec<Step>,
    links: Vec<LinkHistoryEntry>,
}

fn plan_undo(conn: &Connection, id: ChangeId, force: bool) -> Result<UndoPlan> {
    let change = read_change(conn, id)?.ok_or(Error::ChangeNotFound(id))?;
    if !UNDOABLE.contains(&change.action.as_str()) {
        return Err(Error::CannotUndo {
            change: id,
            reason: format!(
                "a {} change is not undone; change it again instead",
                change.action
            ),
        });
    }
    if let Some(undone_by) = active_undo(conn, id)? {
        return Err(Error::AlreadyUndone {
            change: id,
            undone_by,
        });
    }
    let versions = query_history(conn, "h.change_id = ?1", [id])?;
    let links = query_link_history(conn, "change_id = ?1", [id])?;

    let mut touched: BTreeSet<String> = versions.iter().map(|v| v.node_id.to_string()).collect();
    for link in &links {
        touched.insert(link.from.to_string());
        touched.insert(link.to.to_string());
    }
    if NodeId::parse(&change.subject).is_ok() {
        touched.insert(change.subject.clone());
    }
    let mut purged = false;
    for node in &touched {
        purged |= conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM changes WHERE subject = ?1 AND action = ?2 AND id > ?3)",
            params![node, actions::NODE_PURGE, id],
            |row| row.get::<_, bool>(0),
        )?;
    }
    if purged || (versions.is_empty() && links.is_empty()) {
        let reason = if purged {
            "the versions it needs were purged"
        } else {
            "it wrote nothing that can be put back"
        };
        return Err(Error::CannotUndo {
            change: id,
            reason: reason.into(),
        });
    }

    // Changes that only undo or redo this one do not count as later work.
    let chain = undo_chain(conn, id)?;
    let mut later = BTreeSet::new();
    let mut nodes = Vec::new();
    let mut seen = BTreeSet::new();
    for version in &versions {
        let node = version.node_id;
        if !seen.insert(node) {
            continue;
        }
        let mine: Vec<&HistoryEntry> = versions.iter().filter(|v| v.node_id == node).collect();
        let (first, last) = (mine[0], mine[mine.len() - 1]);
        let before = query_history(
            conn,
            "h.node_id = ?1 AND h.seq < ?2",
            params![node.to_string(), first.seq],
        )?
        .pop();
        later.extend(later_changes(
            conn,
            "SELECT change_id FROM history WHERE node_id = ?1 AND seq > ?2",
            params![node.to_string(), last.seq],
        )?);
        let step = match before {
            _ if last.op == ChangeOp::Delete => Step::Put {
                node,
                content: Content::of(last),
                from: last.seq,
            },
            Some(before) if before.op != ChangeOp::Delete => Step::Put {
                node,
                content: Content::of(&before),
                from: before.seq,
            },
            _ => {
                // Undoing the create removes the node, with the links made to it later.
                later.extend(later_changes(
                    conn,
                    "SELECT change_id FROM link_history
                     WHERE (source_id = ?1 OR target_id = ?1) AND change_id > ?2",
                    params![node.to_string(), id],
                )?);
                Step::Delete { node }
            }
        };
        nodes.push(step);
    }
    for link in &links {
        later.extend(later_changes(
            conn,
            "SELECT change_id FROM link_history
             WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3 AND seq > ?4",
            params![
                link.from.to_string(),
                link.kind,
                link.to.to_string(),
                link.seq
            ],
        )?);
    }
    let later: Vec<ChangeId> = later.difference(&chain).copied().collect();
    if !later.is_empty() && !force {
        return Err(Error::UndoConflict { change: id, later });
    }
    Ok(UndoPlan {
        subject: change.subject,
        nodes,
        links,
    })
}

/// Writes a version of a node as part of `write`: an update if the node exists, or creates it
/// again with the same id. `rename` rewrites the bodies that link to a restored title.
fn put_version(
    tx: &Transaction<'_>,
    id: &NodeId,
    content: &Content,
    from: i64,
    write: &Write,
    rename: bool,
) -> Result<()> {
    match read_node(tx, id)? {
        Some(current) => {
            let fields = Fields::to(&current, content);
            if fields.is_empty() {
                return Ok(());
            }
            apply_fields(tx, id, &fields, write)?;
            record(
                tx,
                id,
                ChangeOp::Update,
                write.prov,
                write.now,
                write.change,
                Some(from),
            )?;
            finish_rename(tx, id, &current.title, &fields, write, rename)?;
        }
        None => {
            // The node keeps when and by whom it was first created.
            let created: (i64, i64) = tx
                .query_row(
                    "SELECT changed_at, changed_by FROM history WHERE node_id = ?1
                     ORDER BY seq LIMIT 1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .unwrap_or((write.now.0, write.prov));
            insert_node(
                tx,
                id,
                content,
                (Timestamp(created.0), created.1),
                write.prov,
                write.now,
            )?;
            record(
                tx,
                id,
                ChangeOp::Create,
                write.prov,
                write.now,
                write.change,
                Some(from),
            )?;
        }
    }
    Ok(())
}

fn both_exist(conn: &Connection, link: &LinkHistoryEntry) -> Result<bool> {
    Ok(read_node(conn, &link.from)?.is_some() && read_node(conn, &link.to)?.is_some())
}

/// The undo of `id` that is in effect: an undo of it that has not itself been undone.
fn active_undo(conn: &Connection, id: ChangeId) -> Result<Option<ChangeId>> {
    for undo in undos_of(conn, id)? {
        if active_undo(conn, undo)?.is_none() {
            return Ok(Some(undo));
        }
    }
    Ok(None)
}

/// A change with its undos, the undos of those, and so on.
fn undo_chain(conn: &Connection, id: ChangeId) -> Result<BTreeSet<ChangeId>> {
    let mut chain = BTreeSet::from([id]);
    let mut open = vec![id];
    while let Some(next) = open.pop() {
        for undo in undos_of(conn, next)? {
            if chain.insert(undo) {
                open.push(undo);
            }
        }
    }
    Ok(chain)
}

fn undos_of(conn: &Connection, id: ChangeId) -> Result<Vec<ChangeId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM changes WHERE undoes = ?1 AND action = 'undo' ORDER BY id",
    )?;
    let rows = stmt.query_map([id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn later_changes<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<ChangeId>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params, |row| row.get::<_, Option<ChangeId>>(0))?;
    let ids: Vec<Option<ChangeId>> = rows.collect::<rusqlite::Result<_>>()?;
    Ok(ids.into_iter().flatten().collect())
}

/// Reads a change, or `None` if no change has this id.
pub(crate) fn read_change(conn: &Connection, id: ChangeId) -> Result<Option<Change>> {
    Ok(query_changes(conn, "c.id = ?1", [id])?.pop())
}

fn query_changes<P: rusqlite::Params>(
    conn: &Connection,
    tail: &str,
    params: P,
) -> Result<Vec<Change>> {
    let sql = format!(
        "SELECT {CHANGE_COLUMNS} FROM changes c JOIN provenance p ON p.id = c.changed_by
         WHERE {tail}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, change_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn change_row(row: &Row<'_>) -> rusqlite::Result<Change> {
    let detail: String = row.get(9)?;
    Ok(Change {
        id: row.get(0)?,
        changed_at: Timestamp(row.get(1)?),
        changed_by: provenance(row, 2)?,
        action: row.get(6)?,
        subject: row.get(7)?,
        undoes: row.get(8)?,
        detail: serde_json::from_str(&detail)
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default())),
    })
}

fn query_link_history<P: rusqlite::Params>(
    conn: &Connection,
    filter: &str,
    params: P,
) -> Result<Vec<LinkHistoryEntry>> {
    let sql = format!(
        "SELECT seq, change_id, op, source_id, kind, target_id FROM link_history
         WHERE {filter} ORDER BY seq"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, |row| {
        Ok(LinkHistoryEntry {
            seq: row.get(0)?,
            change_id: row.get(1)?,
            op: ChangeOp::parse(&row.get::<_, String>(2)?),
            from: node_id(row, 3)?,
            kind: row.get(4)?,
            to: node_id(row, 5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

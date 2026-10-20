//! [`Space`]: an open `.knode` package.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use ulid::{Generator, Ulid};

use crate::types::{ActorKind, ChangeOp};
use crate::{
    BLOBS_DIR, ChangeId, DATABASE_FILE, EXTENSION, Error, FORMAT_VERSION, HistoryEntry,
    ImportedNode, Link, Manifest, NewNode, Node, NodeId, NodeSummary, NodeUpdate, Properties,
    Provenance, Result, SYNC_DIR, SearchHit, Timestamp, actions, kinds, link_kinds, property_keys,
    schema,
};
use crate::{LinkDirection, attachments, links};

/// The `settings` key holding the space id, which must match `manifest.json`.
const SPACE_ID_KEY: &str = "space.id";

/// Columns of a [`NodeSummary`], with `n` as `nodes` and `u` as the provenance of the last change.
pub(crate) const SUMMARY_COLUMNS: &str = "n.id, n.kind, n.title, n.created_at, n.updated_at, \
     u.actor_kind, u.actor, u.session, u.source";

/// An open `.knode` space.
///
/// Reads take `&self`; writes take `&mut self` and run in one transaction each, so a failed
/// write changes nothing. Every write takes the [`Provenance`] of the change.
#[derive(Debug)]
pub struct Space {
    path: PathBuf,
    manifest: Manifest,
    pub(crate) conn: Connection,
    pub(crate) ids: Generator,
}

impl Space {
    /// Creates a new, empty space at `path`, which must end in `.knode` and must not exist.
    ///
    /// Makes the package folder, `manifest.json`, `knode.db` in WAL mode with the full schema,
    /// and the reserved `blobs/` and `sync/` folders.
    pub fn create(path: impl AsRef<Path>, name: &str) -> Result<Space> {
        let path = path.as_ref().to_path_buf();
        if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            return Err(Error::Invalid(format!(
                "a space must end in .{EXTENSION}: {}",
                path.display()
            )));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("a space needs a name".into()));
        }
        if path.exists() {
            return Err(Error::AlreadyExists(path));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&path)?;
        match Self::init(&path, name) {
            Ok(space) => Ok(space),
            Err(e) => {
                let _ = fs::remove_dir_all(&path);
                Err(e)
            }
        }
    }

    fn init(path: &Path, name: &str) -> Result<Space> {
        fs::create_dir(path.join(BLOBS_DIR))?;
        fs::create_dir(path.join(SYNC_DIR))?;
        let mut ids = Generator::new();
        let id = next_ulid(&mut ids);
        let manifest = Manifest {
            format: FORMAT_VERSION,
            id: id.to_string(),
            name: name.to_string(),
            created_at: Timestamp(id.timestamp_ms() as i64).to_string(),
            generator: Some(generator()),
            extra: Default::default(),
        };
        let mut conn = Connection::open(path.join(DATABASE_FILE))?;
        schema::configure(&conn)?;
        schema::migrate(&mut conn)?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            params![SPACE_ID_KEY, manifest.id],
        )?;
        manifest.write(path)?;
        Ok(Space {
            path: path.to_path_buf(),
            manifest,
            conn,
            ids,
        })
    }

    /// Opens an existing space.
    ///
    /// Checks the format version in `manifest.json`, brings `knode.db` up to the current schema
    /// version if it is older, and checks that the database belongs to the manifest. Fails with
    /// [`Error::UnsupportedFormat`] or [`Error::NewerSchema`] for a space written by a newer
    /// version of Knode.
    pub fn open(path: impl AsRef<Path>) -> Result<Space> {
        let path = path.as_ref().to_path_buf();
        if !path.is_dir() {
            return Err(Error::NotASpace(path));
        }
        let manifest = Manifest::read(&path)?;
        let db = path.join(DATABASE_FILE);
        if !db.is_file() {
            return Err(Error::NotASpace(path));
        }
        let mut conn = Connection::open(db)?;
        schema::configure(&conn)?;
        schema::migrate(&mut conn)?;
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [SPACE_ID_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref() != Some(manifest.id.as_str()) {
            return Err(Error::MismatchedSpace {
                expected: manifest.id.clone(),
                found: stored.unwrap_or_default(),
            });
        }
        for dir in [BLOBS_DIR, SYNC_DIR] {
            fs::create_dir_all(path.join(dir))?;
        }
        Ok(Space {
            path,
            manifest,
            conn,
            ids: Generator::new(),
        })
    }

    /// The package folder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The contents of `manifest.json`.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Renames the space: changes `name` in `manifest.json`. The package folder keeps its name.
    pub fn rename(&mut self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("a space needs a name".into()));
        }
        let mut manifest = self.manifest.clone();
        manifest.name = name.to_string();
        manifest.generator = Some(generator());
        manifest.write(&self.path)?;
        self.manifest = manifest;
        Ok(())
    }

    /// The number of nodes in the space.
    pub fn node_count(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM nodes", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Creates a node and returns it.
    ///
    /// The body is read once: its plain text goes to the full-text index and each `[[link]]`
    /// becomes a `mentions` link to the node it names, or waits as an unresolved link until a
    /// node with that title exists. `[[links]]` in other nodes waiting for this title are
    /// resolved now.
    pub fn create_node(&mut self, new: NewNode, by: &Provenance) -> Result<Node> {
        self.create_node_with_properties(new, &Properties::new(), by)
    }

    /// Creates a node with its properties in one transaction, and returns it. See
    /// [`create_node`](Self::create_node) and [`set_properties`](Self::set_properties).
    pub fn create_node_with_properties(
        &mut self,
        new: NewNode,
        properties: &Properties,
        by: &Provenance,
    ) -> Result<Node> {
        check_properties(properties)?;
        let kind = if new.kind.trim().is_empty() {
            kinds::NOTE.to_string()
        } else {
            check_name("node kind", new.kind.trim())?
        };
        let title = check_title(&new.title)?;
        let tags = clean_tags(&new.tags)?;
        let id = NodeId::from_ulid(next_ulid(&mut self.ids));
        let now = Timestamp::now();

        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::NODE_CREATE, &id.to_string(), None)?;
        let content = Content {
            kind,
            title,
            body: new.body,
            tags,
        };
        insert_node(&tx, &id, &content, (now, prov), prov, now)?;
        write_properties(&tx, &id, properties, prov, now)?;
        record(&tx, &id, ChangeOp::Create, prov, now, change, None)?;
        tx.commit()?;
        self.require_node(&id)
    }

    /// Creates a node written elsewhere, keeping its id, `created_at`, `updated_at`,
    /// `created_by` and `updated_by`, with its properties, and returns it. For imports and
    /// sync only: everything else creates nodes with [`create_node`](Self::create_node).
    ///
    /// The write itself is made by `by`, now: the `node.create` change and the `create` row of
    /// the history are those of `by`, so the history says who brought the node into this
    /// space and when, while the node says who wrote it and when. The body is read like any
    /// other (see [`create_node`](Self::create_node)).
    ///
    /// Fails with [`Error::Invalid`] when a node with this id exists (update it instead) or
    /// `updated_at` is earlier than `created_at`. An id that only the history still has, such
    /// as a node deleted here, is created again as a recovered node would be.
    pub fn import_node(
        &mut self,
        imported: ImportedNode,
        properties: &Properties,
        by: &Provenance,
    ) -> Result<Node> {
        check_properties(properties)?;
        let ImportedNode {
            id,
            node: new,
            created_at,
            updated_at,
            created_by,
            updated_by,
        } = imported;
        if updated_at < created_at {
            return Err(Error::Invalid(format!(
                "node {id} cannot be updated ({updated_at}) before it was created ({created_at})"
            )));
        }
        let kind = if new.kind.trim().is_empty() {
            kinds::NOTE.to_string()
        } else {
            check_name("node kind", new.kind.trim())?
        };
        let title = check_title(&new.title)?;
        let tags = clean_tags(&new.tags)?;
        if read_node(&self.conn, &id)?.is_some() {
            return Err(Error::Invalid(format!(
                "node {id} already exists; update it instead"
            )));
        }
        let now = Timestamp::now();

        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let created_prov = provenance_id(&tx, &created_by)?;
        let updated_prov = provenance_id(&tx, &updated_by)?;
        let change = add_change(&tx, now, prov, actions::NODE_CREATE, &id.to_string(), None)?;
        let content = Content {
            kind,
            title,
            body: new.body,
            tags,
        };
        insert_node(
            &tx,
            &id,
            &content,
            (created_at, created_prov),
            updated_prov,
            updated_at,
        )?;
        write_properties(&tx, &id, properties, prov, now)?;
        record(&tx, &id, ChangeOp::Create, prov, now, change, None)?;
        tx.commit()?;
        self.require_node(&id)
    }

    /// Returns a node, or `None` if no node has this id.
    pub fn get_node(&self, id: &NodeId) -> Result<Option<Node>> {
        read_node(&self.conn, id)
    }

    /// Changes a node and returns it as it is now.
    ///
    /// Only the fields set in `update` change; when nothing changes, nothing is written. A new
    /// body is read again for its plain text and `[[links]]`. A new title rewrites every
    /// `[[Old title]]` in the nodes that link here, so links keep showing the current title;
    /// each rewritten node gets its own entry in the history.
    pub fn update_node(
        &mut self,
        id: &NodeId,
        update: NodeUpdate,
        by: &Provenance,
    ) -> Result<Node> {
        self.write_update(id, update, by, false)
    }

    /// Changes a node like [`update_node`](Self::update_node), keeping one history row for a run
    /// of updates by the same agent session, such as a conversation that capture keeps current
    /// while it grows.
    ///
    /// When `by` is an agent with a session and the newest history row of the node is an
    /// `update` with exactly the same provenance, that row is replaced by one with the node as
    /// it is now, instead of a new row being added. A row written by anyone else (a person,
    /// another agent or session, a rename) ends the run, so it and the version before it stay.
    /// `create` and `delete` rows are never replaced. Any other provenance behaves exactly like
    /// [`update_node`](Self::update_node).
    pub fn update_node_coalescing(
        &mut self,
        id: &NodeId,
        update: NodeUpdate,
        by: &Provenance,
    ) -> Result<Node> {
        let coalesce = by.kind == ActorKind::Agent
            && by.session.as_deref().is_some_and(|s| !s.trim().is_empty());
        self.write_update(id, update, by, coalesce)
    }

    fn write_update(
        &mut self,
        id: &NodeId,
        update: NodeUpdate,
        by: &Provenance,
        coalesce: bool,
    ) -> Result<Node> {
        let current = self.require_node(id)?;
        let fields = Fields::diff(&current, update)?;
        if fields.is_empty() {
            return Ok(current);
        }
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::NODE_UPDATE, &id.to_string(), None)?;
        let write = Write { prov, now, change };
        apply_fields(&tx, id, &fields, &write)?;
        if coalesce {
            drop_run_snapshot(&tx, id, prov)?;
        }
        record(&tx, id, ChangeOp::Update, prov, now, change, None)?;
        finish_rename(&tx, id, &current.title, &fields, &write, true)?;
        tx.commit()?;
        self.require_node(id)
    }

    /// Deletes a node, its tags and every link from or to it. Its last version stays in the
    /// history, and the links other than `mentions` it had are kept in the history of links, so
    /// the delete can be undone with them. `[[links]]` to it in other nodes wait as unresolved
    /// links, and come back if a node with the same title is created.
    pub fn delete_node(&mut self, id: &NodeId, by: &Provenance) -> Result<()> {
        let node = self.require_node(id)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        let change = add_change(&tx, now, prov, actions::NODE_DELETE, &id.to_string(), None)?;
        delete_in(&tx, id, &node.title, &Write { prov, now, change })?;
        tx.commit()?;
        Ok(())
    }

    /// Links two nodes with a link of the given kind, such as `related` or `part-of`, and returns
    /// the link. Linking twice returns the existing link and records nothing.
    ///
    /// The kind must be built in (see [`link_kinds`]) or defined by the space with
    /// [`set_link_kind`](Self::set_link_kind); an empty kind means `related`. `mentions` links
    /// come only from `[[links]]` in a body and cannot be made by hand. For a symmetric kind,
    /// such as `related`, linking B to A when A is already linked to B returns that link.
    pub fn link(
        &mut self,
        from: &NodeId,
        to: &NodeId,
        kind: &str,
        by: &Provenance,
    ) -> Result<Link> {
        let kind = check_link_kind(kind)?;
        self.require_exists(from)?;
        self.require_exists(to)?;
        let direction = links::direction_of(&self.conn, &kind)?.ok_or_else(|| {
            Error::Invalid(format!(
                "unknown link kind {kind:?}: define it first with a label (see set_link_kind)"
            ))
        })?;
        let (from, to) = if direction == LinkDirection::Symmetric
            && !link_exists(&self.conn, from, &kind, to)?
            && link_exists(&self.conn, to, &kind, from)?
        {
            (to, from)
        } else {
            (from, to)
        };
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        if !link_exists(&tx, from, &kind, to)? {
            let prov = provenance_id(&tx, by)?;
            let change = add_change(
                &tx,
                now,
                prov,
                actions::LINK_CREATE,
                &from.to_string(),
                None,
            )?;
            insert_link(&tx, from, &kind, to, &Write { prov, now, change })?;
        }
        tx.commit()?;
        self.query_links(
            "l.source_id = ?1 AND l.kind = ?2 AND l.target_id = ?3",
            params![from.to_string(), kind, to.to_string()],
        )?
        .pop()
        .ok_or_else(|| Error::Invalid("the link could not be read back".into()))
    }

    /// Removes a link made with [`link`](Self::link), recorded as a change by Knode itself.
    /// Returns whether there was one.
    ///
    /// Prefer [`unlink_by`](Self::unlink_by), which records who removed the link.
    pub fn unlink(&mut self, from: &NodeId, to: &NodeId, kind: &str) -> Result<bool> {
        self.unlink_by(from, to, kind, &Provenance::system())
    }

    /// Removes a link made with [`link`](Self::link), recorded as a change by `by`. Returns
    /// whether there was one; removing a link that is not there records nothing. For a symmetric
    /// kind, the link between the two nodes is removed whichever way it was made.
    pub fn unlink_by(
        &mut self,
        from: &NodeId,
        to: &NodeId,
        kind: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let kind = check_link_kind(kind)?;
        let symmetric = links::direction_of(&self.conn, &kind)? == Some(LinkDirection::Symmetric);
        let (from, to) = if symmetric
            && !link_exists(&self.conn, from, &kind, to)?
            && link_exists(&self.conn, to, &kind, from)?
        {
            (to, from)
        } else {
            (from, to)
        };
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        if !link_exists(&tx, from, &kind, to)? {
            return Ok(false);
        }
        let prov = provenance_id(&tx, by)?;
        let change = add_change(
            &tx,
            now,
            prov,
            actions::LINK_DELETE,
            &from.to_string(),
            None,
        )?;
        remove_link(&tx, from, &kind, to, change)?;
        tx.commit()?;
        Ok(true)
    }

    /// The links that start from a node, oldest first.
    pub fn links_from(&self, id: &NodeId) -> Result<Vec<Link>> {
        self.query_links("l.source_id = ?1", [id.to_string()])
    }

    /// The links that point at a node, oldest first.
    pub fn backlinks(&self, id: &NodeId) -> Result<Vec<Link>> {
        self.query_links("l.target_id = ?1", [id.to_string()])
    }

    /// The `[[links]]` in a node's body that name no node yet, as written.
    pub fn unresolved_links(&self, id: &NodeId) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT target FROM unresolved_links WHERE source_id = ?1 ORDER BY target_key",
        )?;
        let rows = stmt.query_map([id.to_string()], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The nodes with this title, compared without regard to case or extra spaces, oldest first.
    /// A `[[link]]` with this title points at the first one.
    pub fn find_by_title(&self, title: &str) -> Result<Vec<NodeSummary>> {
        self.query_summaries(
            "WHERE n.title_key = ?1 ORDER BY n.seq",
            [knode_md::title_key(title)],
        )
    }

    /// Sets properties of a node, adding new keys and replacing the values of existing ones;
    /// keys not named stay as they are. Returns whether anything changed.
    ///
    /// Properties are structured metadata, such as the agent and session of a conversation (see
    /// [`property_keys`](crate::property_keys)). A key is lower case letters, digits, `_`, `.`
    /// and `-`, starting with a letter, at most 64 characters. Properties are not part of the
    /// node's history, and setting them does not change its `updated_at`; each one keeps when
    /// and by whom it was last set.
    pub fn set_properties(
        &mut self,
        id: &NodeId,
        properties: &Properties,
        by: &Provenance,
    ) -> Result<bool> {
        check_properties(properties)?;
        self.require_exists(id)?;
        let current = self.properties(id)?;
        let changed: Properties = properties
            .iter()
            .filter(|(key, value)| current.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if changed.is_empty() {
            return Ok(false);
        }
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        write_properties(&tx, id, &changed, prov, Timestamp::now())?;
        tx.commit()?;
        Ok(true)
    }

    /// Sets one property of a node. Returns whether it changed. See
    /// [`set_properties`](Self::set_properties).
    pub fn set_property(
        &mut self,
        id: &NodeId,
        key: &str,
        value: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let properties = Properties::from([(key.to_string(), value.to_string())]);
        self.set_properties(id, &properties, by)
    }

    /// Removes a property of a node. Returns whether there was one.
    pub fn remove_property(&mut self, id: &NodeId, key: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM properties WHERE node_id = ?1 AND key = ?2",
            params![id.to_string(), key],
        )?;
        Ok(removed > 0)
    }

    /// The properties of a node, by key. Empty for a node without properties or with no such id.
    pub fn properties(&self, id: &NodeId) -> Result<Properties> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM properties WHERE node_id = ?1")?;
        let rows = stmt.query_map([id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One property of a node, or `None` if it is not set.
    pub fn property(&self, id: &NodeId, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM properties WHERE node_id = ?1 AND key = ?2",
                params![id.to_string(), key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The nodes whose property `key` is exactly `value`, oldest first. Uses an index, so it is
    /// fast in any space, such as finding the conversation of an agent session.
    pub fn find_by_property(&self, key: &str, value: &str) -> Result<Vec<NodeSummary>> {
        self.query_summaries(
            "JOIN properties p ON p.node_id = n.id WHERE p.key = ?1 AND p.value = ?2 \
             ORDER BY n.seq",
            params![key, value],
        )
    }

    /// The nodes changed most recently, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<NodeSummary>> {
        self.query_summaries(
            "ORDER BY n.updated_at DESC, n.seq DESC LIMIT ?1",
            [limit_param(limit)],
        )
    }

    /// Every version of a node, oldest first, including its deletion. Works for deleted nodes.
    ///
    /// Each version names the change it belongs to; see [`change`](Self::change).
    pub fn history(&self, id: &NodeId) -> Result<Vec<HistoryEntry>> {
        query_history(&self.conn, "h.node_id = ?1", [id.to_string()])
    }

    /// Searches titles and text by words, best match first.
    ///
    /// Every word must appear; the last one also matches as a prefix, so results come while
    /// typing. Case and accents are ignored: `cafe` finds `Café`. A match in the title counts
    /// more than one in the text. Returns at most `limit` hits; an empty query returns none.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        // FTS5 sorts by `rank` itself and runs snippet() only for the rows it returns, as long as
        // the query reads the FTS table alone; nodes are joined afterwards.
        let sql = format!(
            "WITH hits AS (
                 SELECT rowid, snippet(nodes_fts, -1, '**', '**', '…', 16) AS snippet, rank
                 FROM nodes_fts WHERE nodes_fts MATCH ?1 ORDER BY rank LIMIT ?2
             )
             SELECT {SUMMARY_COLUMNS}, hits.snippet, hits.rank
             FROM hits
             JOIN nodes n ON n.seq = hits.rowid
             JOIN provenance u ON u.id = n.updated_by
             ORDER BY hits.rank"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![fts_query, limit_param(limit)], |row| {
            Ok(SearchHit {
                node: summary(row)?,
                snippet: row.get(9)?,
                score: -row.get::<_, f64>(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn require_node(&self, id: &NodeId) -> Result<Node> {
        self.get_node(id)?.ok_or(Error::NotFound(*id))
    }

    fn require_exists(&self, id: &NodeId) -> Result<()> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM nodes WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        found.map(|_| ()).ok_or(Error::NotFound(*id))
    }

    fn query_summaries<P: rusqlite::Params>(
        &self,
        tail: &str,
        params: P,
    ) -> Result<Vec<NodeSummary>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM nodes n JOIN provenance u ON u.id = n.updated_by {tail}"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params, summary)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn query_links<P: rusqlite::Params>(
        &self,
        filter: &str,
        params: P,
    ) -> Result<Vec<Link>> {
        let sql = format!(
            "SELECT l.source_id, s.title, l.target_id, t.title, l.kind, l.created_at,
                    p.actor_kind, p.actor, p.session, p.source
             FROM links l
             JOIN nodes s ON s.id = l.source_id
             JOIN nodes t ON t.id = l.target_id
             JOIN provenance p ON p.id = l.created_by
             WHERE {filter}
             ORDER BY l.created_at, l.kind, l.target_id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params, |row| {
            Ok(Link {
                from: node_id(row, 0)?,
                from_title: row.get(1)?,
                to: node_id(row, 2)?,
                to_title: row.get(3)?,
                kind: row.get(4)?,
                created_at: Timestamp(row.get(5)?),
                created_by: provenance(row, 6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The program name written into `manifest.json`.
fn generator() -> String {
    format!("knode-format {}", env!("CARGO_PKG_VERSION"))
}

pub(crate) fn next_ulid(ids: &mut Generator) -> Ulid {
    ids.generate().unwrap_or_else(|_| Ulid::generate())
}

pub(crate) fn limit_param(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Turns words into an FTS5 query: every word quoted, all required, the last one as a prefix.
fn fts_query(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();
    let (last, rest) = words.split_last()?;
    let mut out: Vec<String> = rest.iter().map(|w| format!("\"{w}\"")).collect();
    let prefix = !query.ends_with(char::is_whitespace);
    out.push(if prefix {
        format!("\"{last}\"*")
    } else {
        format!("\"{last}\"")
    });
    Some(out.join(" "))
}

pub(crate) fn check_title(title: &str) -> Result<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(Error::Invalid("a node needs a title".into()));
    }
    if title.contains(['[', ']', '|']) {
        return Err(Error::Invalid(format!(
            "a title cannot contain [, ] or |, which [[links]] use: {title:?}"
        )));
    }
    Ok(title)
}

/// Checks a kind name: `[a-z][a-z0-9-]*`, at most 32 characters.
pub(crate) fn check_name(what: &str, name: &str) -> Result<String> {
    let ok = name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(name.to_string())
    } else {
        Err(Error::Invalid(format!(
            "a {what} is lower case letters, digits and -, starting with a letter: {name:?}"
        )))
    }
}

/// Checks a property key: `[a-z][a-z0-9_.-]*`, at most 64 characters.
fn check_property_key(key: &str) -> Result<()> {
    let ok = key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "a property key is lower case letters, digits, _, . and -, starting with a letter: \
             {key:?}"
        )))
    }
}

fn check_properties(properties: &Properties) -> Result<()> {
    properties
        .keys()
        .try_for_each(|key| check_property_key(key))
}

/// Adds or replaces properties of a node.
fn write_properties(
    tx: &Transaction<'_>,
    id: &NodeId,
    properties: &Properties,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO properties (node_id, key, value, updated_at, updated_by)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (node_id, key) DO UPDATE SET
             value = excluded.value, updated_at = excluded.updated_at,
             updated_by = excluded.updated_by",
    )?;
    for (key, value) in properties {
        stmt.execute(params![id.to_string(), key, value, now.0, prov])?;
        if key == property_keys::MARKDOWN_PATH {
            resolve_waiting_paths(tx, id, value, prov, now)?;
        }
    }
    Ok(())
}

pub(crate) fn check_link_kind(kind: &str) -> Result<String> {
    let kind = kind.trim();
    let kind = if kind.is_empty() {
        link_kinds::RELATED
    } else {
        kind
    };
    if kind == link_kinds::MENTIONS {
        return Err(Error::Invalid(
            "mentions links come from [[links]] in a body; write one there".into(),
        ));
    }
    check_name("link kind", kind)
}

pub(crate) fn clean_tags(tags: &[String]) -> Result<Vec<String>> {
    let mut out = BTreeSet::new();
    for tag in tags {
        let tag = tag.trim().trim_start_matches('#').trim().to_lowercase();
        if tag.is_empty() {
            continue;
        }
        if tag.chars().any(char::is_whitespace) || tag.chars().count() > 64 {
            return Err(Error::Invalid(format!(
                "a tag is one word of at most 64 characters: {tag:?}"
            )));
        }
        out.insert(tag);
    }
    Ok(out.into_iter().collect())
}

pub(crate) fn node_id(row: &Row<'_>, index: usize) -> rusqlite::Result<NodeId> {
    let text: String = row.get(index)?;
    NodeId::parse(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Reads a provenance from four columns: actor_kind, actor, session, source.
pub(crate) fn provenance(row: &Row<'_>, start: usize) -> rusqlite::Result<Provenance> {
    let session: String = row.get(start + 2)?;
    let source: String = row.get(start + 3)?;
    Ok(Provenance {
        kind: ActorKind::parse(&row.get::<_, String>(start)?),
        actor: row.get(start + 1)?,
        session: Some(session).filter(|s| !s.is_empty()),
        source: Some(source).filter(|s| !s.is_empty()),
    })
}

pub(crate) fn summary(row: &Row<'_>) -> rusqlite::Result<NodeSummary> {
    Ok(NodeSummary {
        id: node_id(row, 0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        created_at: Timestamp(row.get(3)?),
        updated_at: Timestamp(row.get(4)?),
        updated_by: provenance(row, 5)?,
    })
}

fn tags_of(conn: &Connection, id: &NodeId) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT tag FROM tags WHERE node_id = ?1 ORDER BY tag")?;
    let rows = stmt.query_map([id.to_string()], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The row id of a provenance, added if it is new.
pub(crate) fn provenance_id(tx: &Transaction<'_>, by: &Provenance) -> Result<i64> {
    let actor = by.actor.trim();
    if actor.is_empty() {
        return Err(Error::Invalid("provenance needs an actor".into()));
    }
    let session = by.session.as_deref().unwrap_or("").trim();
    let source = by.source.as_deref().unwrap_or("").trim();
    tx.execute(
        "INSERT OR IGNORE INTO provenance (actor_kind, actor, session, source)
         VALUES (?1, ?2, ?3, ?4)",
        params![by.kind.as_str(), actor, session, source],
    )?;
    Ok(tx.query_row(
        "SELECT id FROM provenance
         WHERE actor_kind = ?1 AND actor = ?2 AND session = ?3 AND source = ?4",
        params![by.kind.as_str(), actor, session, source],
        |row| row.get(0),
    )?)
}

fn set_tags(tx: &Transaction<'_>, id: &NodeId, tags: &[String]) -> Result<()> {
    tx.execute("DELETE FROM tags WHERE node_id = ?1", [id.to_string()])?;
    for tag in tags {
        tx.execute(
            "INSERT INTO tags (node_id, tag) VALUES (?1, ?2)",
            params![id.to_string(), tag],
        )?;
    }
    Ok(())
}

/// The node a `[[link]]` target names: a node id, or else the oldest node with that title.
pub(crate) fn resolve(tx: &Connection, target: &str) -> Result<Option<String>> {
    if let Ok(id) = NodeId::parse(target) {
        let found: Option<String> = tx
            .query_row(
                "SELECT id FROM nodes WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(tx
        .query_row(
            "SELECT id FROM nodes WHERE title_key = ?1 ORDER BY seq LIMIT 1",
            [knode_md::title_key(target)],
            |row| row.get(0),
        )
        .optional()?)
}

/// The key a file path is compared by when a `[[folder/note]]` link is resolved: its title key,
/// without a leading `/` or `./` and without a trailing `.md`. `"./Projects/Plan.md"` and
/// `"projects/plan"` have the same key.
fn path_key(path: &str) -> String {
    let key = knode_md::title_key(path);
    let key = key.trim_start_matches("./").trim_start_matches('/');
    key.strip_suffix(".md").unwrap_or(key).to_string()
}

/// The node a `[[folder/note]]` link names by the file it was imported from, as Obsidian does:
/// the oldest node whose `markdown.path` is that path, or ends with it after a `/`. Only targets
/// with a `/` are paths; tried after ids and titles, so a node titled `folder/note` still wins.
pub(crate) fn resolve_path(tx: &Connection, target: &str) -> Result<Option<String>> {
    let wanted = path_key(target);
    if !wanted.contains('/') {
        return Ok(None);
    }
    let suffix = format!("/{wanted}");
    let mut stmt = tx.prepare_cached(
        "SELECT p.node_id, p.value FROM properties p JOIN nodes n ON n.id = p.node_id
         WHERE p.key = ?1 ORDER BY n.seq",
    )?;
    let mut rows = stmt.query([property_keys::MARKDOWN_PATH])?;
    while let Some(row) = rows.next()? {
        let value: String = row.get(1)?;
        let key = path_key(&value);
        if key == wanted || key.ends_with(&suffix) {
            return Ok(Some(row.get(0)?));
        }
    }
    Ok(None)
}

/// Turns the unresolved links waiting for the file `path` into `mentions` links to `id`:
/// those written as the path or as any of its endings that still has a `/`, with or without
/// `.md`. `Work/Projects/Plan.md` takes `[[Work/Projects/Plan]]` and `[[Projects/Plan.md]]`.
fn resolve_waiting_paths(
    tx: &Transaction<'_>,
    id: &NodeId,
    path: &str,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let key = path_key(path);
    let mut keys = Vec::new();
    let mut rest = key.as_str();
    while rest.contains('/') {
        keys.push(rest.to_string());
        keys.push(format!("{rest}.md"));
        rest = rest.split_once('/').map_or("", |(_, r)| r);
    }
    for key in keys {
        tx.execute(
            "INSERT OR IGNORE INTO links (source_id, kind, target_id, created_at, created_by)
             SELECT source_id, ?2, ?3, ?4, ?5 FROM unresolved_links WHERE target_key = ?1",
            params![key, link_kinds::MENTIONS, id.to_string(), now.0, prov],
        )?;
        tx.execute("DELETE FROM unresolved_links WHERE target_key = ?1", [key])?;
    }
    Ok(())
}

/// Makes the `mentions` links and unresolved links of a node match the `[[links]]` in its body.
/// Links that stay keep their creation time and provenance.
fn apply_body_links(
    tx: &Transaction<'_>,
    id: &NodeId,
    links: &[knode_md::WikiLink],
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let source = id.to_string();
    let mut targets = BTreeSet::new();
    let mut waiting = Vec::new();
    // From schema version 4, a destination that is the name of one of the node's attachments
    // points at the file: no link and no unresolved link.
    let attached = attachments::attachment_names(tx, id)?;
    for link in links {
        if attached.contains(link.destination.as_str()) {
            continue;
        }
        let mut found = resolve(tx, &link.destination)?;
        if found.is_none() && link.heading.is_some() {
            found = resolve(tx, &link.target)?;
        }
        if found.is_none() {
            found = resolve_path(tx, &link.target)?;
        }
        match found {
            Some(target) => {
                targets.insert(target);
            }
            // A link waits for the title before `#`: `[[Roadmap#Plan]]` waits for `Roadmap`.
            None => waiting.push(link.target.clone()),
        }
    }
    let existing: Vec<String> = {
        let mut stmt =
            tx.prepare_cached("SELECT target_id FROM links WHERE source_id = ?1 AND kind = ?2")?;
        let rows = stmt.query_map(params![source, link_kinds::MENTIONS], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for old in existing.iter().filter(|t| !targets.contains(*t)) {
        tx.execute(
            "DELETE FROM links WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3",
            params![source, link_kinds::MENTIONS, old],
        )?;
    }
    for target in &targets {
        tx.execute(
            "INSERT OR IGNORE INTO links (source_id, kind, target_id, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source, link_kinds::MENTIONS, target, now.0, prov],
        )?;
    }
    tx.execute(
        "DELETE FROM unresolved_links WHERE source_id = ?1",
        [&source],
    )?;
    for target in waiting {
        tx.execute(
            "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
             VALUES (?1, ?2, ?3)",
            params![source, knode_md::title_key(&target), target],
        )?;
    }
    Ok(())
}

/// Reads a node's body again for its `[[links]]`, after its attachments changed.
pub(crate) fn refresh_body_links(
    tx: &Transaction<'_>,
    id: &NodeId,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM nodes WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(body) = body {
        apply_body_links(tx, id, &knode_md::wiki_links(&body), prov, now)?;
    }
    Ok(())
}

/// Turns the unresolved links waiting for `title` into `mentions` links to `id`.
fn resolve_waiting_links(
    tx: &Transaction<'_>,
    id: &NodeId,
    title: &str,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let key = knode_md::title_key(title);
    tx.execute(
        "INSERT OR IGNORE INTO links (source_id, kind, target_id, created_at, created_by)
         SELECT source_id, ?2, ?3, ?4, ?5 FROM unresolved_links WHERE target_key = ?1",
        params![key, link_kinds::MENTIONS, id.to_string(), now.0, prov],
    )?;
    tx.execute("DELETE FROM unresolved_links WHERE target_key = ?1", [key])?;
    Ok(())
}

/// The content of a node that its history keeps: kind, title, body and tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Content {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
}

impl Content {
    pub(crate) fn of(entry: &HistoryEntry) -> Content {
        Content {
            kind: entry.kind.clone(),
            title: entry.title.clone(),
            body: entry.body.clone(),
            tags: entry.tags.clone(),
        }
    }
}

/// The fields an update changes. `None` stays as it is.
#[derive(Debug, Default)]
pub(crate) struct Fields {
    kind: Option<String>,
    title: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
}

impl Fields {
    /// Checks an update and keeps only the fields that differ from the node.
    pub(crate) fn diff(current: &Node, update: NodeUpdate) -> Result<Fields> {
        let kind = match update.kind.as_deref().map(str::trim) {
            Some(kind) if kind != current.kind => Some(check_name("node kind", kind)?),
            _ => None,
        };
        let title = match update.title.as_deref() {
            Some(title) => Some(check_title(title)?).filter(|t| *t != current.title),
            None => None,
        };
        let body = update.body.filter(|b| *b != current.body);
        let tags = match update.tags {
            Some(tags) => Some(clean_tags(&tags)?).filter(|t| *t != current.tags),
            None => None,
        };
        Ok(Fields {
            kind,
            title,
            body,
            tags,
        })
    }

    /// The fields that take a node to a version of its history, which was checked when written.
    pub(crate) fn to(current: &Node, content: &Content) -> Fields {
        Fields {
            kind: Some(content.kind.clone()).filter(|k| *k != current.kind),
            title: Some(content.title.clone()).filter(|t| *t != current.title),
            body: Some(content.body.clone()).filter(|b| *b != current.body),
            tags: Some(content.tags.clone()).filter(|t| *t != current.tags),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.kind.is_none() && self.title.is_none() && self.body.is_none() && self.tags.is_none()
    }
}

/// A write in progress: who makes it, when, and the change it belongs to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Write {
    pub prov: i64,
    pub now: Timestamp,
    pub change: ChangeId,
}

/// Reads a node, or `None` if no node has this id.
pub(crate) fn read_node(conn: &Connection, id: &NodeId) -> Result<Option<Node>> {
    let node = conn
        .query_row(
            "SELECT n.id, n.kind, n.title, n.body, n.created_at, n.updated_at,
                    c.actor_kind, c.actor, c.session, c.source,
                    u.actor_kind, u.actor, u.session, u.source
             FROM nodes n
             JOIN provenance c ON c.id = n.created_by
             JOIN provenance u ON u.id = n.updated_by
             WHERE n.id = ?1",
            [id.to_string()],
            |row| {
                Ok(Node {
                    id: node_id(row, 0)?,
                    kind: row.get(1)?,
                    title: row.get(2)?,
                    body: row.get(3)?,
                    tags: Vec::new(),
                    created_at: Timestamp(row.get(4)?),
                    updated_at: Timestamp(row.get(5)?),
                    created_by: provenance(row, 6)?,
                    updated_by: provenance(row, 10)?,
                })
            },
        )
        .optional()?;
    let Some(mut node) = node else {
        return Ok(None);
    };
    node.tags = tags_of(conn, id)?;
    Ok(Some(node))
}

/// Inserts a node with its tags, applies the links of its body and resolves the links waiting
/// for its title. `created` is when and by whom it was first created.
pub(crate) fn insert_node(
    tx: &Transaction<'_>,
    id: &NodeId,
    content: &Content,
    created: (Timestamp, i64),
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let extracted = knode_md::extract(&content.body);
    tx.execute(
        "INSERT INTO nodes (id, kind, title, title_key, body, plain, created_at, updated_at,
                            created_by, updated_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id.to_string(),
            content.kind,
            content.title,
            knode_md::title_key(&content.title),
            content.body,
            extracted.plain,
            created.0.0,
            now.0,
            created.1,
            prov
        ],
    )?;
    set_tags(tx, id, &content.tags)?;
    apply_body_links(tx, id, &extracted.links, prov, now)?;
    resolve_waiting_links(tx, id, &content.title, prov, now)?;
    Ok(())
}

/// Writes the changed fields of a node and its `updated_at` and `updated_by`.
pub(crate) fn apply_fields(
    tx: &Transaction<'_>,
    id: &NodeId,
    fields: &Fields,
    write: &Write,
) -> Result<()> {
    if let Some(kind) = &fields.kind {
        tx.execute(
            "UPDATE nodes SET kind = ?2 WHERE id = ?1",
            params![id.to_string(), kind],
        )?;
    }
    if let Some(title) = &fields.title {
        tx.execute(
            "UPDATE nodes SET title = ?2, title_key = ?3 WHERE id = ?1",
            params![id.to_string(), title, knode_md::title_key(title)],
        )?;
    }
    if let Some(body) = &fields.body {
        let extracted = knode_md::extract(body);
        tx.execute(
            "UPDATE nodes SET body = ?2, plain = ?3 WHERE id = ?1",
            params![id.to_string(), body, extracted.plain],
        )?;
        apply_body_links(tx, id, &extracted.links, write.prov, write.now)?;
    }
    if let Some(tags) = &fields.tags {
        set_tags(tx, id, tags)?;
    }
    tx.execute(
        "UPDATE nodes SET updated_at = ?2, updated_by = ?3 WHERE id = ?1",
        params![id.to_string(), write.now.0, write.prov],
    )?;
    Ok(())
}

/// After a new title: rewrites the `[[links]]` that point at the node when `rewrite` is set, and
/// resolves the links waiting for the title.
pub(crate) fn finish_rename(
    tx: &Transaction<'_>,
    id: &NodeId,
    old_title: &str,
    fields: &Fields,
    write: &Write,
    rewrite: bool,
) -> Result<()> {
    if let Some(title) = &fields.title {
        if rewrite {
            rewrite_backlinks(tx, id, old_title, title, write)?;
        }
        resolve_waiting_links(tx, id, title, write.prov, write.now)?;
    }
    Ok(())
}

/// Deletes a node: its mentions wait as unresolved links, its links other than `mentions` go to
/// the history of links, and its last version to its history.
pub(crate) fn delete_in(
    tx: &Transaction<'_>,
    id: &NodeId,
    title: &str,
    write: &Write,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO unresolved_links (source_id, target_key, target)
         SELECT source_id, ?2, ?3 FROM links
         WHERE target_id = ?1 AND kind = ?4 AND source_id <> ?1",
        params![
            id.to_string(),
            knode_md::title_key(title),
            title,
            link_kinds::MENTIONS
        ],
    )?;
    tx.execute(
        "INSERT INTO link_history (change_id, op, source_id, kind, target_id)
         SELECT ?2, 'delete', source_id, kind, target_id FROM links
         WHERE (source_id = ?1 OR target_id = ?1) AND kind <> ?3
         ORDER BY created_at, source_id, kind, target_id",
        params![id.to_string(), write.change, link_kinds::MENTIONS],
    )?;
    record(
        tx,
        id,
        ChangeOp::Delete,
        write.prov,
        write.now,
        write.change,
        None,
    )?;
    tx.execute("DELETE FROM nodes WHERE id = ?1", [id.to_string()])?;
    Ok(())
}

/// Whether a link exists.
pub(crate) fn link_exists(
    conn: &Connection,
    from: &NodeId,
    kind: &str,
    to: &NodeId,
) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM links WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3",
            params![from.to_string(), kind, to.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Makes a link by hand and records it in the history of links.
pub(crate) fn insert_link(
    tx: &Transaction<'_>,
    from: &NodeId,
    kind: &str,
    to: &NodeId,
    write: &Write,
) -> Result<()> {
    tx.execute(
        "INSERT INTO links (source_id, kind, target_id, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            from.to_string(),
            kind,
            to.to_string(),
            write.now.0,
            write.prov
        ],
    )?;
    record_link(tx, write.change, ChangeOp::Create, from, kind, to)
}

/// Removes a link made by hand and records it in the history of links.
pub(crate) fn remove_link(
    tx: &Transaction<'_>,
    from: &NodeId,
    kind: &str,
    to: &NodeId,
    change: ChangeId,
) -> Result<()> {
    tx.execute(
        "DELETE FROM links WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3",
        params![from.to_string(), kind, to.to_string()],
    )?;
    record_link(tx, change, ChangeOp::Delete, from, kind, to)
}

fn record_link(
    tx: &Transaction<'_>,
    change: ChangeId,
    op: ChangeOp,
    from: &NodeId,
    kind: &str,
    to: &NodeId,
) -> Result<()> {
    tx.execute(
        "INSERT INTO link_history (change_id, op, source_id, kind, target_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![change, op.as_str(), from.to_string(), kind, to.to_string()],
    )?;
    Ok(())
}

/// Adds a row to `changes` and returns its id.
pub(crate) fn add_change(
    tx: &Transaction<'_>,
    now: Timestamp,
    prov: i64,
    action: &str,
    subject: &str,
    undoes: Option<ChangeId>,
) -> Result<ChangeId> {
    add_change_with_detail(tx, now, prov, action, subject, undoes, "{}")
}

/// Adds a row to `changes` with details, a JSON object, and returns its id.
pub(crate) fn add_change_with_detail(
    tx: &Transaction<'_>,
    now: Timestamp,
    prov: i64,
    action: &str,
    subject: &str,
    undoes: Option<ChangeId>,
    detail: &str,
) -> Result<ChangeId> {
    tx.execute(
        "INSERT INTO changes (changed_at, changed_by, action, subject, undoes, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now.0, prov, action, subject, undoes, detail],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Columns of a [`HistoryEntry`], with `h` as `history` and `p` as its provenance.
const HISTORY_COLUMNS: &str = "h.seq, h.node_id, h.op, h.changed_at, p.actor_kind, p.actor, \
     p.session, p.source, h.kind, h.title, h.body, h.tags, h.change_id, h.restored_from";

/// Rows of `history` that match `filter`, in order.
pub(crate) fn query_history<P: rusqlite::Params>(
    conn: &Connection,
    filter: &str,
    params: P,
) -> Result<Vec<HistoryEntry>> {
    let sql = format!(
        "SELECT {HISTORY_COLUMNS} FROM history h JOIN provenance p ON p.id = h.changed_by
         WHERE {filter} ORDER BY h.seq"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, |row| {
        let tags: String = row.get(11)?;
        Ok(HistoryEntry {
            seq: row.get(0)?,
            node_id: node_id(row, 1)?,
            op: ChangeOp::parse(&row.get::<_, String>(2)?),
            changed_at: Timestamp(row.get(3)?),
            changed_by: provenance(row, 4)?,
            kind: row.get(8)?,
            title: row.get(9)?,
            body: row.get(10)?,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
            change_id: row.get(12)?,
            restored_from: row.get(13)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Rewrites `[[old]]` to `[[new]]` in every node that mentions `id`, recording each node's new
/// version in the same change.
fn rewrite_backlinks(
    tx: &Transaction<'_>,
    id: &NodeId,
    old: &str,
    new: &str,
    write: &Write,
) -> Result<()> {
    let sources: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT n.id, n.body FROM links l JOIN nodes n ON n.id = l.source_id
             WHERE l.target_id = ?1 AND l.kind = ?2 AND l.source_id <> ?1",
        )?;
        let rows = stmt.query_map(params![id.to_string(), link_kinds::MENTIONS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (source, body) in sources {
        let rewritten = knode_md::rename_links(&body, old, new);
        if rewritten == body {
            continue;
        }
        tx.execute(
            "UPDATE nodes SET body = ?2, plain = ?3, updated_at = ?4, updated_by = ?5
             WHERE id = ?1",
            params![
                source,
                rewritten,
                knode_md::plain_text(&rewritten),
                write.now.0,
                write.prov
            ],
        )?;
        let source = NodeId::parse(&source)?;
        record(
            tx,
            &source,
            ChangeOp::Update,
            write.prov,
            write.now,
            write.change,
            None,
        )?;
    }
    Ok(())
}

/// Removes the newest history row of a node, and its change, when the row is a plain
/// `node.update` by `prov` that nothing else depends on, so the row that follows replaces it.
/// Being the newest row, no one else has changed the node since.
///
/// The row is kept when its change wrote anything else (a rename's rewritten nodes) or was
/// undone, so the run ends there.
fn drop_run_snapshot(tx: &Transaction<'_>, id: &NodeId, prov: i64) -> Result<()> {
    let newest: Option<(i64, String, i64, Option<ChangeId>)> = tx
        .query_row(
            "SELECT seq, op, changed_by, change_id FROM history
             WHERE node_id = ?1 ORDER BY seq DESC LIMIT 1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((seq, op, changed_by, change)) = newest else {
        return Ok(());
    };
    if op != ChangeOp::Update.as_str() || changed_by != prov {
        return Ok(());
    }
    if let Some(change) = change {
        let alone: bool = tx.query_row(
            "SELECT c.action = ?2
                 AND (SELECT count(*) FROM history WHERE change_id = ?1) = 1
                 AND NOT EXISTS (SELECT 1 FROM link_history WHERE change_id = ?1)
                 AND NOT EXISTS (SELECT 1 FROM changes WHERE undoes = ?1)
             FROM changes c WHERE c.id = ?1",
            params![change, actions::NODE_UPDATE],
            |row| row.get(0),
        )?;
        if !alone {
            return Ok(());
        }
        tx.execute("DELETE FROM history WHERE seq = ?1", [seq])?;
        tx.execute("DELETE FROM changes WHERE id = ?1", [change])?;
    } else {
        tx.execute("DELETE FROM history WHERE seq = ?1", [seq])?;
    }
    Ok(())
}

/// Adds the current state of a node to its history, as part of `change`, and returns the row's
/// `seq`.
pub(crate) fn record(
    tx: &Transaction<'_>,
    id: &NodeId,
    op: ChangeOp,
    prov: i64,
    now: Timestamp,
    change: ChangeId,
    restored_from: Option<i64>,
) -> Result<i64> {
    let tags: Vec<String> = {
        let mut stmt = tx.prepare_cached("SELECT tag FROM tags WHERE node_id = ?1 ORDER BY tag")?;
        let rows = stmt.query_map([id.to_string()], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let tags = serde_json::to_string(&tags).map_err(|e| Error::Invalid(e.to_string()))?;
    tx.execute(
        "INSERT INTO history (node_id, op, changed_at, changed_by, kind, title, body, tags,
                              change_id, restored_from)
         SELECT id, ?2, ?3, ?4, kind, title, body, ?5, ?6, ?7 FROM nodes WHERE id = ?1",
        params![
            id.to_string(),
            op.as_str(),
            now.0,
            prov,
            tags,
            change,
            restored_from
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_fts_queries() {
        assert_eq!(fts_query("hello world").unwrap(), "\"hello\" \"world\"*");
        assert_eq!(fts_query("hello ").unwrap(), "\"hello\"");
        assert_eq!(fts_query("c++ \"x").unwrap(), "\"c++\" \"x\"*");
        assert_eq!(fts_query("   "), None);
        assert_eq!(fts_query("- * ("), None);
    }

    #[test]
    fn checks_titles_and_names() {
        assert_eq!(check_title("  A   title ").unwrap(), "A title");
        assert!(check_title("   ").is_err());
        assert!(check_title("a [[b]]").is_err());
        assert!(check_title("Issue #3").is_ok());
        assert!(check_name("kind", "part-of").is_ok());
        assert!(check_name("kind", "Note").is_err());
        assert!(check_name("kind", "1note").is_err());
        assert!(check_property_key("conversation.started_at").is_ok());
        assert!(check_property_key("a-b_c.d1").is_ok());
        assert!(check_property_key("Agent").is_err());
        assert!(check_property_key("1a").is_err());
        assert!(check_property_key("").is_err());
        assert!(check_property_key(&"a".repeat(65)).is_err());
        assert!(check_link_kind("mentions").is_err());
        assert_eq!(check_link_kind("").unwrap(), "related");
    }

    #[test]
    fn cleans_tags() {
        let tags =
            clean_tags(&["#Rust".into(), "rust".into(), " ".into(), "Knode".into()]).unwrap();
        assert_eq!(tags, ["knode", "rust"]);
        assert!(clean_tags(&["two words".into()]).is_err());
    }
}

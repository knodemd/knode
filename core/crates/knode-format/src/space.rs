//! [`Space`]: an open `.knode` package.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use ulid::{Generator, Ulid};

use crate::types::{ActorKind, ChangeOp};
use crate::{
    BLOBS_DIR, DATABASE_FILE, EXTENSION, Error, FORMAT_VERSION, HistoryEntry, Link, Manifest,
    NewNode, Node, NodeId, NodeSummary, NodeUpdate, Provenance, Result, SYNC_DIR, SearchHit,
    Timestamp, kinds, link_kinds, schema,
};

/// The `settings` key holding the space id, which must match `manifest.json`.
const SPACE_ID_KEY: &str = "space.id";

/// Columns of a [`NodeSummary`], with `n` as `nodes` and `u` as the provenance of the last change.
const SUMMARY_COLUMNS: &str = "n.id, n.kind, n.title, n.created_at, n.updated_at, \
     u.actor_kind, u.actor, u.session, u.source";

/// An open `.knode` space.
///
/// Reads take `&self`; writes take `&mut self` and run in one transaction each, so a failed
/// write changes nothing. Every write takes the [`Provenance`] of the change.
#[derive(Debug)]
pub struct Space {
    path: PathBuf,
    manifest: Manifest,
    conn: Connection,
    ids: Generator,
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
        let kind = if new.kind.trim().is_empty() {
            kinds::NOTE.to_string()
        } else {
            check_name("node kind", new.kind.trim())?
        };
        let title = check_title(&new.title)?;
        let tags = clean_tags(&new.tags)?;
        let id = NodeId::from_ulid(next_ulid(&mut self.ids));
        let now = Timestamp::now();
        let extracted = knode_md::extract(&new.body);

        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT INTO nodes (id, kind, title, title_key, body, plain, created_at, updated_at,
                                created_by, updated_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?8)",
            params![
                id.to_string(),
                kind,
                title,
                knode_md::title_key(&title),
                new.body,
                extracted.plain,
                now.0,
                prov
            ],
        )?;
        set_tags(&tx, &id, &tags)?;
        apply_body_links(&tx, &id, &extracted.links, prov, now)?;
        resolve_waiting_links(&tx, &id, &title, prov, now)?;
        record(&tx, &id, ChangeOp::Create, prov, now)?;
        tx.commit()?;
        self.require_node(&id)
    }

    /// Returns a node, or `None` if no node has this id.
    pub fn get_node(&self, id: &NodeId) -> Result<Option<Node>> {
        let node = self
            .conn
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
        node.tags = tags_of(&self.conn, id)?;
        Ok(Some(node))
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
        let current = self.require_node(id)?;
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
        if kind.is_none() && title.is_none() && body.is_none() && tags.is_none() {
            return Ok(current);
        }

        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        if let Some(kind) = &kind {
            tx.execute(
                "UPDATE nodes SET kind = ?2 WHERE id = ?1",
                params![id.to_string(), kind],
            )?;
        }
        if let Some(title) = &title {
            tx.execute(
                "UPDATE nodes SET title = ?2, title_key = ?3 WHERE id = ?1",
                params![id.to_string(), title, knode_md::title_key(title)],
            )?;
        }
        if let Some(body) = &body {
            let extracted = knode_md::extract(body);
            tx.execute(
                "UPDATE nodes SET body = ?2, plain = ?3 WHERE id = ?1",
                params![id.to_string(), body, extracted.plain],
            )?;
            apply_body_links(&tx, id, &extracted.links, prov, now)?;
        }
        if let Some(tags) = &tags {
            set_tags(&tx, id, tags)?;
        }
        tx.execute(
            "UPDATE nodes SET updated_at = ?2, updated_by = ?3 WHERE id = ?1",
            params![id.to_string(), now.0, prov],
        )?;
        record(&tx, id, ChangeOp::Update, prov, now)?;
        if let Some(title) = &title {
            rewrite_backlinks(&tx, id, &current.title, title, prov, now)?;
            resolve_waiting_links(&tx, id, title, prov, now)?;
        }
        tx.commit()?;
        self.require_node(id)
    }

    /// Deletes a node, its tags and every link from or to it. Its last version stays in the
    /// history. `[[links]]` to it in other nodes wait as unresolved links, and come back if a
    /// node with the same title is created.
    pub fn delete_node(&mut self, id: &NodeId, by: &Provenance) -> Result<()> {
        let node = self.require_node(id)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
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
        record(&tx, id, ChangeOp::Delete, prov, now)?;
        tx.execute("DELETE FROM nodes WHERE id = ?1", [id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    /// Links two nodes with a link of the given kind, such as `related` or `part-of`, and returns
    /// the link. Linking twice returns the existing link.
    ///
    /// `mentions` links come only from `[[links]]` in a body and cannot be made by hand.
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
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT OR IGNORE INTO links (source_id, kind, target_id, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                from.to_string(),
                kind,
                to.to_string(),
                Timestamp::now().0,
                prov
            ],
        )?;
        tx.commit()?;
        self.query_links(
            "l.source_id = ?1 AND l.kind = ?2 AND l.target_id = ?3",
            params![from.to_string(), kind, to.to_string()],
        )?
        .pop()
        .ok_or_else(|| Error::Invalid("the link could not be read back".into()))
    }

    /// Removes a link made with [`link`](Self::link). Returns whether there was one.
    pub fn unlink(&mut self, from: &NodeId, to: &NodeId, kind: &str) -> Result<bool> {
        let kind = check_link_kind(kind)?;
        let removed = self.conn.execute(
            "DELETE FROM links WHERE source_id = ?1 AND kind = ?2 AND target_id = ?3",
            params![from.to_string(), kind, to.to_string()],
        )?;
        Ok(removed > 0)
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

    /// The nodes changed most recently, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<NodeSummary>> {
        self.query_summaries(
            "ORDER BY n.updated_at DESC, n.seq DESC LIMIT ?1",
            [limit_param(limit)],
        )
    }

    /// Every version of a node, oldest first, including its deletion. Works for deleted nodes.
    pub fn history(&self, id: &NodeId) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT h.seq, h.node_id, h.op, h.changed_at, p.actor_kind, p.actor, p.session,
                    p.source, h.kind, h.title, h.body, h.tags
             FROM history h JOIN provenance p ON p.id = h.changed_by
             WHERE h.node_id = ?1 ORDER BY h.seq",
        )?;
        let rows = stmt.query_map([id.to_string()], |row| {
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

    fn query_links<P: rusqlite::Params>(&self, filter: &str, params: P) -> Result<Vec<Link>> {
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

fn next_ulid(ids: &mut Generator) -> Ulid {
    ids.generate().unwrap_or_else(|_| Ulid::generate())
}

fn limit_param(limit: usize) -> i64 {
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

fn check_title(title: &str) -> Result<String> {
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
fn check_name(what: &str, name: &str) -> Result<String> {
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

fn check_link_kind(kind: &str) -> Result<String> {
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

fn clean_tags(tags: &[String]) -> Result<Vec<String>> {
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

fn node_id(row: &Row<'_>, index: usize) -> rusqlite::Result<NodeId> {
    let text: String = row.get(index)?;
    NodeId::parse(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Reads a provenance from four columns: actor_kind, actor, session, source.
fn provenance(row: &Row<'_>, start: usize) -> rusqlite::Result<Provenance> {
    let session: String = row.get(start + 2)?;
    let source: String = row.get(start + 3)?;
    Ok(Provenance {
        kind: ActorKind::parse(&row.get::<_, String>(start)?),
        actor: row.get(start + 1)?,
        session: Some(session).filter(|s| !s.is_empty()),
        source: Some(source).filter(|s| !s.is_empty()),
    })
}

fn summary(row: &Row<'_>) -> rusqlite::Result<NodeSummary> {
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
fn provenance_id(tx: &Transaction<'_>, by: &Provenance) -> Result<i64> {
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
fn resolve(tx: &Transaction<'_>, target: &str) -> Result<Option<String>> {
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
    for link in links {
        let mut found = resolve(tx, &link.destination)?;
        if found.is_none() && link.heading.is_some() {
            found = resolve(tx, &link.target)?;
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

/// Rewrites `[[old]]` to `[[new]]` in every node that mentions `id`, recording each change.
fn rewrite_backlinks(
    tx: &Transaction<'_>,
    id: &NodeId,
    old: &str,
    new: &str,
    prov: i64,
    now: Timestamp,
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
                now.0,
                prov
            ],
        )?;
        let source = NodeId::parse(&source)?;
        record(tx, &source, ChangeOp::Update, prov, now)?;
    }
    Ok(())
}

/// Adds the current state of a node to its history.
fn record(
    tx: &Transaction<'_>,
    id: &NodeId,
    op: ChangeOp,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    let tags: Vec<String> = {
        let mut stmt = tx.prepare_cached("SELECT tag FROM tags WHERE node_id = ?1 ORDER BY tag")?;
        let rows = stmt.query_map([id.to_string()], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let tags = serde_json::to_string(&tags).map_err(|e| Error::Invalid(e.to_string()))?;
    tx.execute(
        "INSERT INTO history (node_id, op, changed_at, changed_by, kind, title, body, tags)
         SELECT id, ?2, ?3, ?4, kind, title, body, ?5 FROM nodes WHERE id = ?1",
        params![id.to_string(), op.as_str(), now.0, prov, tags],
    )?;
    Ok(())
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

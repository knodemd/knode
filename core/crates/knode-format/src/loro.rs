//! The SQLite side of concurrent editing with Loro (section 21 of the specification): the
//! `loro_docs` and `loro_peers` tables, and writing a body together with the version of the Loro
//! document it holds.
//!
//! The Loro documents themselves, in `sync/nodes/`, are read and written by `knode-sync`.

use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::space::{
    Fields, Write, add_change, apply_fields, provenance, provenance_id, read_node, record,
};
use crate::types::ChangeOp;
use crate::{
    DATABASE_FILE, Error, Node, NodeId, NodeUpdate, Provenance, Result, SYNC_DIR, Space, Timestamp,
    actions,
};

/// The folder of the Loro documents of nodes, inside [`SYNC_DIR`].
pub const SYNC_NODES_DIR: &str = "nodes";

/// The write lock of a space's database, held until dropped. See [`Space::loro_lock`].
#[derive(Debug)]
pub struct LoroLock {
    conn: Connection,
}

impl Drop for LoroLock {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

impl Space {
    /// The folder of a node's Loro document: `sync/nodes/<id>/`. It may not exist.
    pub fn loro_dir(&self, id: &NodeId) -> PathBuf {
        self.path()
            .join(SYNC_DIR)
            .join(SYNC_NODES_DIR)
            .join(id.to_string())
    }

    /// Takes the write lock of the space's database, waiting for other writers as any write does,
    /// and holds it until the [`LoroLock`] is dropped, so no other program writes the space
    /// meanwhile.
    ///
    /// A writer that uses Loro holds it while it makes a node's first document, brings a
    /// document in step with a body written without Loro, or compacts a document, so two
    /// programs never do it at once. While it is held, writes through this `Space` wait for it
    /// and fail; reads work.
    pub fn loro_lock(&self) -> Result<LoroLock> {
        let conn = Connection::open(self.path().join(DATABASE_FILE))?;
        crate::schema::configure(&conn)?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(LoroLock { conn })
    }

    /// The version of a node's Loro document that its body holds, from `loro_docs`: a JSON object
    /// from each peer id, in decimal, to the number of its operations included. `None` when the
    /// node has no row, because it has no document or its body was written without Loro.
    pub fn loro_version(&self, id: &NodeId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT version FROM loro_docs WHERE node_id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Records that a node's body holds `version` of its Loro document, without writing the
    /// node. Used when the document was brought in step with the body (21.5).
    pub fn set_loro_version(&mut self, id: &NodeId, version: &str) -> Result<()> {
        check_version(version)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: Option<i64> = tx
            .query_row("SELECT 1 FROM nodes WHERE id = ?1", [id.to_string()], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(Error::NotFound(*id));
        }
        put_version(&tx, id, version, Timestamp::now())?;
        tx.commit()?;
        Ok(())
    }

    /// Writes a new body merged through the node's Loro document, and the document's `version`
    /// it holds, in one transaction (21.4): the body, its plain text and links, the history and
    /// the change as [`update_node`](Self::update_node) does, then the `loro_docs` row.
    ///
    /// When the body is unchanged only the row is written, and nothing is recorded.
    pub fn write_loro_body(
        &mut self,
        id: &NodeId,
        body: &str,
        version: &str,
        by: &Provenance,
    ) -> Result<Node> {
        check_version(version)?;
        // Immediate: the body is compared and written with no other writer in between.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_node(&tx, id)?.ok_or(Error::NotFound(*id))?;
        let update = NodeUpdate {
            body: Some(body.to_string()),
            ..Default::default()
        };
        let fields = Fields::diff(&current, update)?;
        let now = Timestamp::now();
        if !fields.is_empty() {
            let prov = provenance_id(&tx, by)?;
            let change = add_change(&tx, now, prov, actions::NODE_UPDATE, &id.to_string(), None)?;
            let write = Write { prov, now, change };
            apply_fields(&tx, id, &fields, &write)?;
            record(&tx, id, ChangeOp::Update, prov, now, change, None)?;
        }
        // After the body: the trigger `loro_docs_body` has removed the row.
        put_version(&tx, id, version, now)?;
        tx.commit()?;
        self.get_node(id)?.ok_or(Error::NotFound(*id))
    }

    /// Records who a Loro peer is, before an update made as it is written (21.3). Recording the
    /// same peer again with the same provenance does nothing; with another provenance it fails,
    /// since a peer belongs to one provenance.
    pub fn add_loro_peer(&mut self, peer: u64, by: &Provenance) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prov = provenance_id(&tx, by)?;
        tx.execute(
            "INSERT OR IGNORE INTO loro_peers (peer, provenance, created_at) VALUES (?1, ?2, ?3)",
            params![peer.to_string(), prov, Timestamp::now().0],
        )?;
        let stored: i64 = tx.query_row(
            "SELECT provenance FROM loro_peers WHERE peer = ?1",
            [peer.to_string()],
            |row| row.get(0),
        )?;
        if stored != prov {
            return Err(Error::Invalid(format!(
                "Loro peer {peer} belongs to another provenance"
            )));
        }
        tx.commit()?;
        Ok(())
    }

    /// The provenance of the edits made as a Loro peer, or `None` if the peer is not recorded.
    pub fn loro_peer(&self, peer: u64) -> Result<Option<Provenance>> {
        Ok(self
            .conn
            .query_row(
                "SELECT p.actor_kind, p.actor, p.session, p.source
                 FROM loro_peers l JOIN provenance p ON p.id = l.provenance
                 WHERE l.peer = ?1",
                [peer.to_string()],
                |row| provenance(row, 0),
            )
            .optional()?)
    }
}

fn put_version(
    tx: &rusqlite::Transaction<'_>,
    id: &NodeId,
    version: &str,
    now: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO loro_docs (node_id, version, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (node_id) DO UPDATE SET version = excluded.version,
                                             updated_at = excluded.updated_at",
        params![id.to_string(), version, now.0],
    )?;
    Ok(())
}

/// Checks a version vector as `loro_docs` keeps it: a JSON object from peer ids in decimal to
/// non-negative counts.
fn check_version(version: &str) -> Result<()> {
    let invalid = || Error::Invalid(format!("not a Loro version vector: {version}"));
    let value: serde_json::Value = serde_json::from_str(version).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    for (peer, count) in object {
        if peer.parse::<u64>().is_err() || !peer.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        if count.as_u64().is_none() {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_versions() {
        assert!(check_version("{}").is_ok());
        assert!(check_version(r#"{"7318201936571142": 52}"#).is_ok());
        assert!(check_version(r#"{"-1": 52}"#).is_err());
        assert!(check_version(r#"{"x": 52}"#).is_err());
        assert!(check_version(r#"{"1": -2}"#).is_err());
        assert!(check_version("[]").is_err());
    }
}

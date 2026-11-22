//! What a space keeps on disk, for a view of how much room it takes and what fills it. The sums
//! are made by `knode_ingest::storage`; this module only lists the rows and the files.

use std::fs;
use std::path::Path;

use rusqlite::Row;
use serde::{Deserialize, Serialize};

use crate::space::{node_id, provenance};
use crate::{BLOBS_DIR, DATABASE_FILE, NodeId, Provenance, Result, SYNC_DIR, Space};

/// A file attached to a node, with the node it belongs to: [`Space::stored_files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    /// The node.
    pub node_id: NodeId,
    /// The kind of the node; `None` when the node is deleted (the file stays until it is purged).
    pub node_kind: Option<String>,
    /// The title of the node; `None` when it is deleted.
    pub node_title: Option<String>,
    /// Its name in the node.
    pub name: String,
    /// The blob's hash.
    pub hash: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its media type, such as `image/heic`.
    pub media_type: String,
    /// Who attached it.
    pub created_by: Provenance,
}

/// A file in `blobs/`, as its row says: [`Space::stored_blobs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBlob {
    /// The SHA-256 of its bytes.
    pub hash: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its media type.
    pub media_type: String,
}

impl Space {
    /// Every attachment of the space, of nodes deleted too, largest first.
    pub fn stored_files(&self) -> Result<Vec<StoredFile>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.node_id, n.kind, n.title, a.name, a.hash, b.size, b.media_type,
                    p.actor_kind, p.actor, p.session, p.source
             FROM attachments a
             JOIN blobs b ON b.hash = a.hash
             JOIN provenance p ON p.id = a.created_by
             LEFT JOIN nodes n ON n.id = a.node_id
             ORDER BY b.size DESC, a.node_id, a.name",
        )?;
        let rows = stmt.query_map([], stored_file_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every blob the space has a row for, used by a node or not.
    pub fn stored_blobs(&self) -> Result<Vec<StoredBlob>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT hash, size, media_type FROM blobs ORDER BY hash")?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredBlob {
                hash: row.get(0)?,
                size: row.get::<_, i64>(1)? as u64,
                media_type: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The bytes of every blob the space has a row for, summed from the rows: no file is read.
    pub fn blob_bytes(&self) -> Result<u64> {
        let sum: i64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(size), 0) FROM blobs", [], |row| {
                    row.get(0)
                })?;
        Ok(sum.max(0) as u64)
    }

    /// The bytes of the database with its journal files.
    pub fn database_bytes(&self) -> u64 {
        let root = self.path();
        ["", "-wal", "-shm"]
            .iter()
            .map(|suffix| {
                fs::metadata(root.join(format!("{DATABASE_FILE}{suffix}")))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum()
    }

    /// The bytes of the database (with its journal files), of `sync/` and of the whole package
    /// on disk. Walks the package: for a view a person opens, not for every add (see
    /// [`blob_bytes`](Self::blob_bytes) and [`database_bytes`](Self::database_bytes)).
    pub fn disk_usage(&self) -> Result<DiskUsage> {
        let root = self.path();
        Ok(DiskUsage {
            database: self.database_bytes(),
            sync: dir_size(&root.join(SYNC_DIR)),
            blobs: dir_size(&root.join(BLOBS_DIR)),
            total: dir_size(root),
        })
    }
}

/// What [`Space::disk_usage`] found, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskUsage {
    /// `knode.db` and its journal files: the notes, their history and the search index.
    pub database: u64,
    /// `sync/`: the documents of concurrent editing.
    pub sync: u64,
    /// `blobs/`: every attached file, once.
    pub blobs: u64,
    /// The whole package.
    pub total: u64,
}

fn stored_file_row(row: &Row<'_>) -> rusqlite::Result<StoredFile> {
    Ok(StoredFile {
        node_id: node_id(row, 0)?,
        node_kind: row.get(1)?,
        node_title: row.get(2)?,
        name: row.get(3)?,
        hash: row.get(4)?,
        size: row.get::<_, i64>(5)? as u64,
        media_type: row.get(6)?,
        created_by: provenance(row, 7)?,
    })
}

/// The bytes of the files under `path`, not following links. 0 when it cannot be read.
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| dir_size(&e.path()))
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::{NewNode, Provenance, Space};

    #[test]
    fn lists_files_of_live_and_deleted_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("S.knode"), "S").unwrap();
        let by = Provenance::person("Ada");
        let a = space.create_node(NewNode::note("A", ""), &by).unwrap();
        let b = space.create_node(NewNode::note("B", ""), &by).unwrap();
        space
            .attach(&a.id, "big.bin", &[1u8; 300][..], &by)
            .unwrap();
        space.attach(&b.id, "small.txt", &b"hi"[..], &by).unwrap();
        space.delete_node(&b.id, &by).unwrap();
        let files = space.stored_files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "big.bin");
        assert_eq!(files[0].size, 300);
        assert_eq!(files[0].node_title.as_deref(), Some("A"));
        assert_eq!(files[1].node_kind, None);
        assert_eq!(space.stored_blobs().unwrap().len(), 2);
        let usage = space.disk_usage().unwrap();
        assert!(usage.database > 0);
        assert!(usage.blobs >= 302);
        assert!(usage.total >= usage.database + usage.blobs);
        assert_eq!(space.blob_bytes().unwrap(), 302);
        assert_eq!(space.database_bytes(), usage.database);
    }
}

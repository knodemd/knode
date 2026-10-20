//! Attachments: files in `blobs/`, named by their hash, and the nodes that use them (schema
//! version 4).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::space::{
    add_change_with_detail, next_ulid, node_id, provenance, provenance_id, read_node,
    refresh_body_links, resolve,
};
use crate::types::ActorKind;
use crate::{BLOBS_DIR, Error, NodeId, NodeSummary, Provenance, Result, Space, Timestamp, actions};

/// The largest file this library attaches: 100 MB. Larger files are refused, so a space stays
/// quick to copy, back up and sync.
pub const MAX_ATTACHMENT_SIZE: u64 = 100 * 1024 * 1024;

/// Temporary files in `blobs/` start with this; a purge removes those older than a day, left by
/// a crash.
const TEMP_PREFIX: &str = ".tmp-";

/// A file in `blobs/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    /// The SHA-256 of its bytes, 64 lower case hexadecimal digits.
    pub hash: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its media type, such as `image/png`.
    pub media_type: String,
    /// When it was first added.
    pub created_at: Timestamp,
    /// Who first added it.
    pub created_by: Provenance,
}

/// A file attached to a node, under a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// The node.
    pub node_id: NodeId,
    /// Its name in the node, such as `beds.svg`; `![[beds.svg]]` in the body shows it.
    pub name: String,
    /// The blob's hash.
    pub hash: String,
    /// The size of the file in bytes.
    pub size: u64,
    /// The media type of the file, such as `image/png`.
    pub media_type: String,
    /// When it was attached.
    pub created_at: Timestamp,
    /// Who attached it.
    pub created_by: Provenance,
}

/// What a `[[link]]` in a node's body points at. See [`Space::resolve_link`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LinkTarget {
    /// An attachment of the node: its name is the whole destination.
    Attachment(Attachment),
    /// A node, by id or by title.
    Node(NodeSummary),
}

/// Checks that a hash is 64 lower case hexadecimal digits.
fn check_hash(hash: &str) -> Result<()> {
    if hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{hash:?} is not a blob hash (64 lower case hexadecimal digits)"
        )))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the file of a blob is: `blobs/ab/cdef...`.
fn blob_file(package: &Path, hash: &str) -> PathBuf {
    package.join(BLOBS_DIR).join(&hash[..2]).join(&hash[2..])
}

/// Makes a file name fit for an attachment: trimmed, the characters that file systems and
/// `[[links]]` use (`/ \ [ ] | #`) replaced by `-`, control characters dropped, and at most 255
/// characters, keeping the extension.
fn clean_name(name: &str) -> Result<String> {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | '[' | ']' | '|' | '#' => '-',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.' || c == '-') {
        return Err(Error::Invalid("an attachment needs a name".into()));
    }
    Ok(fit(cleaned, ""))
}

/// Splits a name into its stem and its extension (with the dot), such as `beds` and `.svg`.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// `name` with `suffix` before its extension, cut to 255 characters.
fn fit(name: &str, suffix: &str) -> String {
    let (stem, ext) = split_extension(name);
    let room = 255usize.saturating_sub(ext.chars().count() + suffix.chars().count());
    let stem: String = stem.chars().take(room).collect();
    let ext: String = ext.chars().take(255 - suffix.chars().count()).collect();
    format!("{}{suffix}{ext}", stem.trim_end())
}

/// The media type of a file, from its first bytes when they tell, else from its name.
fn media_type(head: &[u8], name: &str) -> String {
    let sniffed = if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        match &head[8..12] {
            b"heic" | b"heix" | b"mif1" | b"msf1" => Some("image/heic"),
            b"avif" => Some("image/avif"),
            b"qt  " => Some("video/quicktime"),
            _ => Some("video/mp4"),
        }
    } else {
        None
    };
    if let Some(found) = sniffed {
        return found.to_string();
    }
    let (_, ext) = split_extension(name);
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// A blob written to `blobs/`, before its row.
struct Stored {
    hash: String,
    size: u64,
    media_type: String,
}

/// Writes bytes to `blobs/`: to a temporary file, hashed while written and flushed to disk, then
/// renamed to its place, or deleted if the place is taken. Refuses more than
/// [`MAX_ATTACHMENT_SIZE`] bytes.
fn store(
    package: &Path,
    ids: &mut ulid::Generator,
    mut reader: impl Read,
    name: &str,
) -> Result<Stored> {
    let dir = package.join(BLOBS_DIR);
    fs::create_dir_all(&dir)?;
    let temp = dir.join(format!("{TEMP_PREFIX}{}", next_ulid(ids)));
    let result = (|| -> Result<Stored> {
        let mut file = File::create(&temp)?;
        let mut hasher = Sha256::new();
        let mut head = Vec::new();
        let mut size: u64 = 0;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            size += n as u64;
            if size > MAX_ATTACHMENT_SIZE {
                return Err(Error::Invalid(format!(
                    "{name} is larger than {} MB, the most a space attaches",
                    MAX_ATTACHMENT_SIZE / (1024 * 1024)
                )));
            }
            if head.len() < 64 {
                head.extend_from_slice(&buf[..n.min(64 - head.len())]);
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
        }
        file.sync_all()?;
        drop(file);
        let hash = hex(&hasher.finalize());
        let place = blob_file(package, &hash);
        if place.is_file() {
            fs::remove_file(&temp)?;
        } else {
            fs::create_dir_all(place.parent().unwrap_or(&dir))?;
            fs::rename(&temp, &place)?;
        }
        Ok(Stored {
            media_type: media_type(&head, name),
            hash,
            size,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Adds the row of a blob whose file was just written, if it is new.
fn insert_blob(tx: &Transaction<'_>, stored: &Stored, prov: i64, now: Timestamp) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO blobs (hash, size, media_type, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            stored.hash,
            stored.size as i64,
            stored.media_type,
            now.0,
            prov
        ],
    )?;
    Ok(())
}

const ATTACHMENT_COLUMNS: &str = "a.node_id, a.name, a.hash, b.size, b.media_type, a.created_at, \
     p.actor_kind, p.actor, p.session, p.source";

fn attachment_row(row: &Row<'_>) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        node_id: node_id(row, 0)?,
        name: row.get(1)?,
        hash: row.get(2)?,
        size: row.get::<_, i64>(3)? as u64,
        media_type: row.get(4)?,
        created_at: Timestamp(row.get(5)?),
        created_by: provenance(row, 6)?,
    })
}

fn query_attachments<P: rusqlite::Params>(
    conn: &Connection,
    filter: &str,
    params: P,
) -> Result<Vec<Attachment>> {
    let sql = format!(
        "SELECT {ATTACHMENT_COLUMNS} FROM attachments a
         JOIN blobs b ON b.hash = a.hash
         JOIN provenance p ON p.id = a.created_by
         WHERE {filter} ORDER BY a.created_at, a.name"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, attachment_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn read_attachment(conn: &Connection, node: &NodeId, name: &str) -> Result<Option<Attachment>> {
    Ok(query_attachments(
        conn,
        "a.node_id = ?1 AND a.name = ?2",
        params![node.to_string(), name],
    )?
    .pop())
}

fn name_taken(conn: &Connection, node: &NodeId, name: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT hash FROM attachments WHERE node_id = ?1 AND name = ?2",
            params![node.to_string(), name],
            |row| row.get(0),
        )
        .optional()?)
}

/// The names of a node's attachments: `[[links]]` whose destination is one of them point at the
/// file, not at a node (section 20.4).
pub(crate) fn attachment_names(conn: &Connection, node: &NodeId) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare_cached("SELECT name FROM attachments WHERE node_id = ?1")?;
    let rows = stmt.query_map([node.to_string()], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether a blob is used: an attachment of any node names it, or a body or a version names it
/// as `knode:blob/<hash>`.
fn blob_used(conn: &Connection, hash: &str) -> Result<bool> {
    let reference = format!("knode:blob/{hash}");
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM attachments WHERE hash = ?1)
             OR EXISTS (SELECT 1 FROM nodes WHERE instr(body, ?2) > 0)
             OR EXISTS (SELECT 1 FROM history WHERE instr(body, ?2) > 0)",
        params![hash, reference],
        |row| row.get(0),
    )?)
}

/// Deletes the rows of the blobs in `hashes` that are no longer used, and returns those hashes,
/// whose files are removed once the transaction commits.
fn drop_unused(tx: &Transaction<'_>, hashes: BTreeSet<String>) -> Result<Vec<String>> {
    let mut dropped = Vec::new();
    for hash in hashes {
        if !blob_used(tx, &hash)? {
            tx.execute("DELETE FROM blobs WHERE hash = ?1", [&hash])?;
            dropped.push(hash);
        }
    }
    Ok(dropped)
}

/// Removes the files of purged blobs, and the folders they leave empty.
pub(crate) fn remove_blob_files(package: &Path, hashes: &[String]) {
    for hash in hashes {
        let file = blob_file(package, hash);
        let _ = fs::remove_file(&file);
        if let Some(parent) = file.parent() {
            // Only removed when empty.
            let _ = fs::remove_dir(parent);
        }
    }
}

/// Deletes the attachments of a purged node and the rows of the blobs no longer used, as part of
/// its purge. Returns the hashes whose files go once the purge commits.
pub(crate) fn purge_node_attachments(tx: &Transaction<'_>, id: &NodeId) -> Result<Vec<String>> {
    let hashes: BTreeSet<String> = {
        let mut stmt = tx.prepare("SELECT DISTINCT hash FROM attachments WHERE node_id = ?1")?;
        let rows = stmt.query_map([id.to_string()], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    tx.execute(
        "DELETE FROM attachments WHERE node_id = ?1",
        [id.to_string()],
    )?;
    drop_unused(tx, hashes)
}

/// The blob files in `blobs/` that follow the layout, by hash, and the temporary files older
/// than a day.
fn scan_blobs(package: &Path) -> Result<(BTreeSet<String>, Vec<PathBuf>)> {
    let mut hashes = BTreeSet::new();
    let mut stale = Vec::new();
    let dir = package.join(BLOBS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((hashes, stale)),
        Err(e) => return Err(e.into()),
    };
    let day_ago = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if kind.is_file() && name.starts_with(TEMP_PREFIX) {
            let old = entry
                .metadata()?
                .modified()
                .map(|m| m < day_ago)
                .unwrap_or(false);
            if old {
                stale.push(entry.path());
            }
            continue;
        }
        let prefix_ok =
            name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !kind.is_dir() || !prefix_ok {
            continue;
        }
        for file in fs::read_dir(entry.path())? {
            let file = file?;
            let rest = file.file_name().to_string_lossy().into_owned();
            let hash = format!("{name}{rest}");
            if file.file_type()?.is_file() && check_hash(&hash).is_ok() {
                hashes.insert(hash);
            }
        }
    }
    Ok((hashes, stale))
}

impl Space {
    /// Adds a file to `blobs/` without attaching it to a node, and returns its blob. A file
    /// already there is not written again. `name` is used to find the media type when the
    /// content does not tell it.
    ///
    /// Prefer [`attach`](Self::attach): a blob no node uses is removed by the next purge.
    pub fn add_blob(&mut self, reader: impl Read, name: &str, by: &Provenance) -> Result<Blob> {
        let package = self.path().to_path_buf();
        let stored = store(&package, &mut self.ids, reader, name)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        insert_blob(&tx, &stored, prov, now)?;
        tx.commit()?;
        self.blob(&stored.hash)?
            .ok_or_else(|| Error::Invalid("the blob could not be read back".into()))
    }

    /// Attaches a file to a node under `name`, its original file name, and returns the
    /// attachment. Recorded as an `attachment.add` change, which can be undone.
    ///
    /// The bytes go to `blobs/` first, named by their SHA-256, and only then are the rows
    /// written, so a row never names a missing file; the same content is stored once. A name
    /// is trimmed, and `/ \ [ ] | #` in it become `-`. When the node already has an attachment
    /// with that name and other content, ` 2`, ` 3`... is added before the extension; attaching
    /// the same content under the same name again returns the attachment and records nothing.
    /// `![[name]]` and `[[name]]` in the node's body then point at the file. Files larger than
    /// [`MAX_ATTACHMENT_SIZE`] are refused.
    pub fn attach(
        &mut self,
        node: &NodeId,
        name: &str,
        reader: impl Read,
        by: &Provenance,
    ) -> Result<Attachment> {
        let name = clean_name(name)?;
        if read_node(&self.conn, node)?.is_none() {
            return Err(Error::NotFound(*node));
        }
        let package = self.path().to_path_buf();
        let stored = store(&package, &mut self.ids, reader, &name)?;
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        insert_blob(&tx, &stored, prov, now)?;
        let mut chosen = name.clone();
        let mut n = 2;
        while let Some(hash) = name_taken(&tx, node, &chosen)? {
            if hash == stored.hash {
                drop(tx);
                return read_attachment(&self.conn, node, &chosen)?
                    .ok_or_else(|| Error::Invalid("the attachment could not be read".into()));
            }
            chosen = fit(&name, &format!(" {n}"));
            n += 1;
        }
        add_attachment(&tx, node, &chosen, &stored.hash, prov, now)?;
        let detail = serde_json::json!({ "name": chosen, "hash": stored.hash }).to_string();
        add_change_with_detail(
            &tx,
            now,
            prov,
            actions::ATTACHMENT_ADD,
            &node.to_string(),
            None,
            &detail,
        )?;
        tx.commit()?;
        read_attachment(&self.conn, node, &chosen)?
            .ok_or_else(|| Error::Invalid("the attachment could not be read back".into()))
    }

    /// Attaches a file on disk to a node under its file name. See [`attach`](Self::attach).
    pub fn attach_file(
        &mut self,
        node: &NodeId,
        path: impl AsRef<Path>,
        by: &Provenance,
    ) -> Result<Attachment> {
        let path = path.as_ref();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Invalid(format!("{} is not a file", path.display())))?;
        if fs::metadata(path)?.len() > MAX_ATTACHMENT_SIZE {
            return Err(Error::Invalid(format!(
                "{name} is larger than {} MB, the most a space attaches",
                MAX_ATTACHMENT_SIZE / (1024 * 1024)
            )));
        }
        self.attach(node, &name, File::open(path)?, by)
    }

    /// Removes an attachment from a node and returns whether there was one. Recorded as an
    /// `attachment.remove` change, which can be undone. The blob stays in `blobs/` until a
    /// [`purge_blobs`](Self::purge_blobs); `[[links]]` in the body to the name wait again for a
    /// node with that title.
    pub fn detach(&mut self, node: &NodeId, name: &str, by: &Provenance) -> Result<bool> {
        let Some(hash) = name_taken(&self.conn, node, name)? else {
            return Ok(false);
        };
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let prov = provenance_id(&tx, by)?;
        remove_attachment(&tx, node, name, prov, now)?;
        let detail = serde_json::json!({ "name": name, "hash": hash }).to_string();
        add_change_with_detail(
            &tx,
            now,
            prov,
            actions::ATTACHMENT_REMOVE,
            &node.to_string(),
            None,
            &detail,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The attachments of a node, oldest first. Kept while the node is deleted, so restoring it
    /// brings them back.
    pub fn attachments(&self, node: &NodeId) -> Result<Vec<Attachment>> {
        query_attachments(&self.conn, "a.node_id = ?1", [node.to_string()])
    }

    /// One attachment of a node by its exact name, or `None`.
    pub fn attachment(&self, node: &NodeId, name: &str) -> Result<Option<Attachment>> {
        read_attachment(&self.conn, node, name)
    }

    /// A blob by hash, or `None` if the space has no row for it.
    pub fn blob(&self, hash: &str) -> Result<Option<Blob>> {
        check_hash(hash)?;
        Ok(self
            .conn
            .query_row(
                "SELECT b.hash, b.size, b.media_type, b.created_at,
                        p.actor_kind, p.actor, p.session, p.source
                 FROM blobs b JOIN provenance p ON p.id = b.created_by
                 WHERE b.hash = ?1",
                [hash],
                |row| {
                    Ok(Blob {
                        hash: row.get(0)?,
                        size: row.get::<_, i64>(1)? as u64,
                        media_type: row.get(2)?,
                        created_at: Timestamp(row.get(3)?),
                        created_by: provenance(row, 4)?,
                    })
                },
            )
            .optional()?)
    }

    /// The path of a blob's file, to open or show it, or `None` when the file is missing (for
    /// example, not synced yet). The file never changes; never write to it.
    pub fn blob_path(&self, hash: &str) -> Result<Option<PathBuf>> {
        check_hash(hash)?;
        let path = blob_file(self.path(), hash);
        Ok(path.is_file().then_some(path))
    }

    /// The bytes of a blob, checked against its hash, or `None` when the file is missing. Fails
    /// when the file's content does not match its name: such a file is never used.
    pub fn read_blob(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.blob_path(hash)? else {
            return Ok(None);
        };
        let bytes = fs::read(path)?;
        if hex(&Sha256::digest(&bytes)) != hash {
            return Err(Error::Invalid(format!(
                "the file of blob {hash} does not match its hash"
            )));
        }
        Ok(Some(bytes))
    }

    /// What the destination of a `[[link]]` in a node's body points at: an attachment of the
    /// node whose name is the whole destination, else the node it names by id or title (the
    /// title before `#` when the whole destination is not one), or `None`.
    pub fn resolve_link(&self, from: &NodeId, destination: &str) -> Result<Option<LinkTarget>> {
        let destination = destination.trim();
        if let Some(found) = read_attachment(&self.conn, from, destination)? {
            return Ok(Some(LinkTarget::Attachment(found)));
        }
        let mut found = resolve(&self.conn, destination)?;
        if found.is_none() {
            if let Some((target, _)) = destination.split_once('#') {
                found = resolve(&self.conn, target.trim())?;
            }
        }
        let Some(id) = found else {
            return Ok(None);
        };
        let node = read_node(&self.conn, &NodeId::parse(&id)?)?;
        Ok(node.map(|n| {
            LinkTarget::Node(NodeSummary {
                id: n.id,
                kind: n.kind,
                title: n.title,
                created_at: n.created_at,
                updated_at: n.updated_at,
                updated_by: n.updated_by,
            })
        }))
    }

    /// Removes every blob nothing uses, files and rows, and returns their hashes. Only a person
    /// can purge: any other provenance fails with [`Error::PersonOnly`].
    ///
    /// A blob is used while an attachment of any node, deleted ones included, names it, or a
    /// body or a version in the history has `knode:blob/<hash>`. Files in `blobs/` with no row
    /// and no such reference go too, as do temporary files a crash left more than a day ago.
    /// Recorded as a `blobs.purge` change; purging nothing records nothing. Rows are deleted in
    /// one transaction and the files after it commits.
    pub fn purge_blobs(&mut self, by: &Provenance) -> Result<Vec<String>> {
        if by.kind != ActorKind::Person {
            return Err(Error::PersonOnly("purge the files of a space".into()));
        }
        let (files, stale) = scan_blobs(self.path())?;
        for temp in stale {
            let _ = fs::remove_file(temp);
        }
        let rows: BTreeSet<String> = {
            let mut stmt = self.conn.prepare("SELECT hash FROM blobs")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let now = Timestamp::now();
        let tx = self.conn.transaction()?;
        let mut purged = drop_unused(&tx, rows.clone())?;
        for hash in files.difference(&rows) {
            if !blob_used(&tx, hash)? {
                purged.push(hash.clone());
            }
        }
        purged.sort();
        if purged.is_empty() {
            return Ok(purged);
        }
        let prov = provenance_id(&tx, by)?;
        let detail = serde_json::json!({ "hashes": purged }).to_string();
        add_change_with_detail(&tx, now, prov, actions::BLOBS_PURGE, "", None, &detail)?;
        tx.commit()?;
        remove_blob_files(self.path(), &purged);
        Ok(purged)
    }
}

/// Adds an attachment row and points the node's `[[links]]` to the name at it.
pub(crate) fn add_attachment(
    tx: &Transaction<'_>,
    node: &NodeId,
    name: &str,
    hash: &str,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    tx.execute(
        "INSERT INTO attachments (node_id, name, hash, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![node.to_string(), name, hash, now.0, prov],
    )?;
    refresh_body_links(tx, node, prov, now)
}

/// Deletes an attachment row and lets the node's `[[links]]` to the name wait for a node again.
pub(crate) fn remove_attachment(
    tx: &Transaction<'_>,
    node: &NodeId,
    name: &str,
    prov: i64,
    now: Timestamp,
) -> Result<()> {
    tx.execute(
        "DELETE FROM attachments WHERE node_id = ?1 AND name = ?2",
        params![node.to_string(), name],
    )?;
    refresh_body_links(tx, node, prov, now)
}

/// Whether a blob's file is in `blobs/`.
pub(crate) fn blob_file_exists(package: &Path, hash: &str) -> bool {
    check_hash(hash).is_ok() && blob_file(package, hash).is_file()
}

/// The hash in a `knode:blob/<hash>` URL, or `None` when the URL is not one.
pub fn blob_url_hash(url: &str) -> Option<&str> {
    let hash = url.trim().strip_prefix("knode:blob/")?;
    check_hash(hash).ok().map(|_| hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_and_numbers_names() {
        assert_eq!(clean_name("  beds.svg ").unwrap(), "beds.svg");
        assert_eq!(clean_name("a/b#c|d.png").unwrap(), "a-b-c-d.png");
        assert!(clean_name("   ").is_err());
        assert!(clean_name("//").is_err());
        assert_eq!(fit("beds.svg", " 2"), "beds 2.svg");
        assert_eq!(fit(".gitignore", " 2"), ".gitignore 2");
        assert_eq!(fit("notes", " 3"), "notes 3");
        let long = format!("{}.png", "a".repeat(300));
        let cleaned = clean_name(&long).unwrap();
        assert_eq!(cleaned.chars().count(), 255);
        assert!(cleaned.ends_with(".png"));
        assert_eq!(fit(&cleaned, " 2").chars().count(), 255);
    }

    #[test]
    fn finds_media_types() {
        assert_eq!(media_type(b"\x89PNG\r\n\x1a\n....", "x"), "image/png");
        assert_eq!(media_type(b"%PDF-1.7", "x.bin"), "application/pdf");
        assert_eq!(media_type(b"<svg", "beds.svg"), "image/svg+xml");
        assert_eq!(media_type(b"hello", "notes.TXT"), "text/plain");
        assert_eq!(media_type(b"hello", "data"), "application/octet-stream");
    }

    #[test]
    fn reads_blob_urls() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert_eq!(blob_url_hash(&format!("knode:blob/{hash}")), Some(hash));
        assert_eq!(blob_url_hash("knode:blob/xyz"), None);
        assert_eq!(blob_url_hash("https://example.com"), None);
    }
}

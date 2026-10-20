//! Files attached to nodes: images, PDFs and other files kept in the space's `blobs/` folder,
//! addressed by the SHA-256 of their content (schema version 4).
//!
//! `![[name]]` in a node's body shows an attachment of the node, and `[[name]]` links to it.

use crate::{KnodeSpace, Provenance, Result, format, parse_id};

/// A file attached to a node.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Attachment {
    /// The node it is attached to.
    pub node_id: String,
    /// Its name in the node, usually the original file name, such as `diagram.png`.
    pub name: String,
    /// The SHA-256 of its content, in lower-case hexadecimal.
    pub hash: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its media type, such as `image/png` or `application/pdf`.
    pub media_type: String,
    /// When it was attached, in Unix milliseconds.
    pub created_at: i64,
    /// Who attached it.
    pub created_by: Provenance,
}

impl From<format::Attachment> for Attachment {
    fn from(a: format::Attachment) -> Self {
        Attachment {
            node_id: a.node_id.to_string(),
            name: a.name,
            hash: a.hash,
            size: a.size,
            media_type: a.media_type,
            created_at: a.created_at.as_millis(),
            created_by: a.created_by.into(),
        }
    }
}

#[uniffi::export]
impl KnodeSpace {
    /// The files attached to a node, oldest first.
    pub fn attachments(&self, node: String) -> Result<Vec<Attachment>> {
        let node = parse_id(&node)?;
        let found = self.lock().attachments(&node)?;
        Ok(found.into_iter().map(Attachment::from).collect())
    }

    /// Attaches bytes to a node under `name` and returns the attachment. A name the node already
    /// uses for other content gets ` 2`, ` 3`...; the same content under the same name again
    /// changes nothing.
    pub fn attach(
        &self,
        node: String,
        name: String,
        content: Vec<u8>,
        by: Provenance,
    ) -> Result<Attachment> {
        let node = parse_id(&node)?;
        let found = self
            .lock()
            .attach(&node, &name, content.as_slice(), &by.into())?;
        Ok(found.into())
    }

    /// Attaches the file at `path` to a node under its file name, up to 100 MB.
    pub fn attach_file(&self, node: String, path: String, by: Provenance) -> Result<Attachment> {
        let node = parse_id(&node)?;
        Ok(self.lock().attach_file(&node, &path, &by.into())?.into())
    }

    /// Removes an attachment from a node. Returns whether there was one. The file stays in the
    /// space until unused files are purged.
    pub fn detach(&self, node: String, name: String, by: Provenance) -> Result<bool> {
        let node = parse_id(&node)?;
        Ok(self.lock().detach(&node, &name, &by.into())?)
    }

    /// Where an attachment's file is on this machine, or `None` when the node has no such
    /// attachment or its file is missing. Never write to it.
    pub fn attachment_path(&self, node: String, name: String) -> Result<Option<String>> {
        let node = parse_id(&node)?;
        let space = self.lock();
        let Some(found) = space.attachment(&node, &name)? else {
            return Ok(None);
        };
        Ok(space
            .blob_path(&found.hash)?
            .map(|p| p.display().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorKind, KnodeError};

    fn me() -> Provenance {
        Provenance {
            kind: ActorKind::Person,
            actor: "Ada".into(),
            session: None,
            source: None,
        }
    }

    #[test]
    fn attaches_reads_and_detaches_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Files.knode").display().to_string();
        let space = KnodeSpace::create(path, "Files".into()).unwrap();
        let node = space
            .create_node("Trip".into(), "![[map.png]]".into(), me())
            .unwrap()
            .id;

        let png = b"\x89PNG\r\n\x1a\n not really".to_vec();
        let added = space
            .attach(node.clone(), "map.png".into(), png.clone(), me())
            .unwrap();
        assert_eq!(added.name, "map.png");
        assert_eq!(added.size, png.len() as u64);
        assert_eq!(added.created_by.actor, "Ada");
        // The same content under the same name changes nothing; other content gets a number.
        let again = space
            .attach(node.clone(), "map.png".into(), png.clone(), me())
            .unwrap();
        assert_eq!(again, added);
        let other = space
            .attach(node.clone(), "map.png".into(), b"other".to_vec(), me())
            .unwrap();
        assert_eq!(other.name, "map 2.png");

        let file = dir.path().join("notes.pdf");
        std::fs::write(&file, b"%PDF-1.7 not really").unwrap();
        let pdf = space
            .attach_file(node.clone(), file.display().to_string(), me())
            .unwrap();
        assert_eq!(pdf.name, "notes.pdf");
        assert_eq!(pdf.media_type, "application/pdf");

        let names: Vec<String> = space
            .attachments(node.clone())
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, ["map.png", "map 2.png", "notes.pdf"]);

        let stored = space
            .attachment_path(node.clone(), "map.png".into())
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(stored).unwrap(), png);
        assert!(
            space
                .attachment_path(node.clone(), "nothing.png".into())
                .unwrap()
                .is_none()
        );

        assert!(space.detach(node.clone(), "map.png".into(), me()).unwrap());
        assert!(!space.detach(node.clone(), "map.png".into(), me()).unwrap());
        assert_eq!(space.attachments(node.clone()).unwrap().len(), 2);

        assert!(matches!(
            space.attachments("not an id".into()),
            Err(KnodeError::Invalid { .. })
        ));
    }
}

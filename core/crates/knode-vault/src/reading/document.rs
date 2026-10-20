//! Writing a whole document (a PDF, a web page) to its node.

use knode_format::{NewNode, NodeId, NodeUpdate, Properties, Provenance, Space, kinds};

use super::{Imported, Outcome, keys};
use crate::Result;

/// A document to write to a node.
pub(crate) struct Document {
    /// What the node is found by again: see [`keys::KEY`].
    pub key: String,
    /// Whether the key is the hash of the content: then a node with this key already has it, as
    /// it was first imported (under the name of the first file), and is left as it is.
    pub by_content: bool,
    /// See [`keys::SOURCE`].
    pub source: &'static str,
    pub title: String,
    pub body: String,
    /// Properties besides the key and the source.
    pub props: Properties,
    /// The original file, by name, to attach.
    pub file: Option<(String, Vec<u8>)>,
    /// Something worth knowing, for the report.
    pub note: Option<String>,
}

/// Creates the node of `doc`, or updates the node with its key when it has not been edited
/// since it was last written.
pub(crate) fn write(space: &mut Space, doc: Document, by: &Provenance) -> Result<Imported> {
    let title = super::clean_title(&doc.title);
    let title = if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    };
    let mut props = doc.props.clone();
    props.insert(keys::KEY.into(), doc.key.clone());
    let existing = space
        .find_by_property(keys::KEY, &doc.key)?
        .first()
        .map(|s| s.id);

    let Some(id) = existing else {
        props.insert(keys::SOURCE.into(), doc.source.into());
        let node = space.create_node_with_properties(
            NewNode {
                kind: kinds::SOURCE.into(),
                title: title.clone(),
                body: doc.body.clone(),
                tags: Vec::new(),
            },
            &props,
            by,
        )?;
        attach(space, &node.id, doc.file.as_ref(), by)?;
        mark_written(space, &node.id, by)?;
        return Ok(Imported {
            id: node.id,
            title: node.title,
            outcome: Outcome::Created,
            highlights: 0,
            note: doc.note,
        });
    };

    let node = space
        .get_node(&id)?
        .ok_or(knode_format::Error::NotFound(id))?;
    if doc.by_content {
        return Ok(Imported {
            id,
            title: node.title,
            outcome: Outcome::Unchanged,
            highlights: 0,
            note: doc.note,
        });
    }
    let untouched = space.property(&id, keys::WRITTEN)?.as_deref()
        == Some(node.updated_at.to_string().as_str());
    let same = node.body == doc.body && node.title == title;
    let outcome = if same {
        Outcome::Unchanged
    } else if untouched {
        space.update_node(
            &id,
            NodeUpdate {
                title: Some(title),
                body: Some(doc.body.clone()),
                ..Default::default()
            },
            by,
        )?;
        Outcome::Updated
    } else {
        Outcome::Edited
    };
    if outcome != Outcome::Edited {
        // Properties of the document as it is now; the source stays the first one.
        space.set_properties(&id, &props, by)?;
        attach(space, &id, doc.file.as_ref(), by)?;
    }
    if outcome == Outcome::Updated || (outcome == Outcome::Unchanged && untouched) {
        mark_written(space, &id, by)?;
    }
    let title = space.get_node(&id)?.map(|n| n.title).unwrap_or_default();
    Ok(Imported {
        id,
        title,
        outcome,
        highlights: 0,
        note: doc.note,
    })
}

/// Keeps the node's `updated_at` as written by the import.
fn mark_written(space: &mut Space, id: &NodeId, by: &Provenance) -> Result<()> {
    if let Some(node) = space.get_node(id)? {
        space.set_property(id, keys::WRITTEN, &node.updated_at.to_string(), by)?;
    }
    Ok(())
}

/// Attaches the original file under its name, replacing an attachment of that name with other
/// content.
fn attach(
    space: &mut Space,
    id: &NodeId,
    file: Option<&(String, Vec<u8>)>,
    by: &Provenance,
) -> Result<()> {
    let Some((name, bytes)) = file else {
        return Ok(());
    };
    if let Some(current) = space.attachment(id, name)? {
        if current.hash == super::sha256(bytes) {
            return Ok(());
        }
        space.detach(id, name, by)?;
    }
    space.attach(id, name, bytes.as_slice(), by)?;
    Ok(())
}

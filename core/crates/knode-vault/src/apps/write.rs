//! Writing the notes an importer read into the space: one node per note, found again by the
//! app and the note's id.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use knode_format::{
    ImportedNode, MAX_ATTACHMENT_SIZE, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space,
    Timestamp, link_kinds,
};

use super::source::Source;
use super::{ImportError, ImportedNote, NoteApp, NotesReport, keys};

/// Where the bytes of an attachment are.
#[derive(Debug, Clone)]
pub(crate) enum Blob {
    /// In memory.
    Bytes(Vec<u8>),
    /// A file of the batch's [`Source`].
    Entry(String),
    /// A file on disk.
    File(PathBuf),
}

/// One note, read from an app, ready to write.
#[derive(Debug, Clone, Default)]
pub(crate) struct Draft {
    /// The note's id in the app.
    pub source_id: String,
    /// A title the format accepts (see [`super::markdown::title`]).
    pub title: String,
    /// The body in Markdown: links to other notes as `[[Title]]`, attachments as `![[name]]`.
    pub body: String,
    /// The app's tags, not cleaned yet.
    pub tags: BTreeSet<String>,
    /// Where it was in the app, with `/` between names.
    pub folder: Option<String>,
    pub created: Option<Timestamp>,
    pub updated: Option<Timestamp>,
    /// Other properties.
    pub properties: Properties,
    /// Attachments by their name in the note (each name used once).
    pub attachments: Vec<(String, Blob)>,
    /// The id in the app of the note this one is part of.
    pub parent: Option<String>,
}

/// What an importer read: the notes, and the export their [`Blob::Entry`] files are in.
pub(crate) struct Batch {
    pub drafts: Vec<Draft>,
    pub source: Option<Source>,
}

/// The node of a note imported before from `app`.
fn find(space: &Space, app: NoteApp, source_id: &str) -> Result<Option<NodeId>, ImportError> {
    for summary in space.find_by_property(keys::ID, source_id)? {
        if space.property(&summary.id, keys::APP)?.as_deref() == Some(app.id()) {
            return Ok(Some(summary.id));
        }
    }
    Ok(None)
}

/// A hash of everything an import writes for a note.
fn hash(draft: &Draft, tags: &[String], props: &Properties) -> String {
    let mut text = String::new();
    for part in [
        draft.title.as_str(),
        draft.body.as_str(),
        &tags.join(","),
        draft.parent.as_deref().unwrap_or(""),
    ] {
        text.push_str(part);
        text.push('\u{0}');
    }
    for (key, value) in props {
        text.push_str(&format!("{key}={value}\u{0}"));
    }
    for (name, _) in &draft.attachments {
        text.push_str(name);
        text.push('\u{0}');
    }
    format!("{:x}", md5::compute(text.as_bytes()))
}

/// The id a new node gets: a ULID of its creation time, with the rest from the app and the
/// note's id, so the same note gets the same id in every space it is imported into.
fn node_id(app: NoteApp, source_id: &str, created: Timestamp) -> NodeId {
    let digest = md5::compute(format!("{}:{source_id}", app.id()).as_bytes());
    let random = u128::from_be_bytes(digest.0) >> 48;
    let ms = created.0.clamp(0, (1i64 << 48) - 1) as u64;
    NodeId::parse(&ulid::Ulid::from_parts(ms, random).to_string()).expect("a ULID is a node id")
}

/// Writes every draft of `batch`, then their `part-of` links.
pub(crate) fn write(
    space: &mut Space,
    app: NoteApp,
    mut batch: Batch,
    by: &Provenance,
    report: &mut NotesReport,
) -> Result<(), ImportError> {
    let mut nodes: BTreeMap<String, NodeId> = BTreeMap::new();
    let mut parents: Vec<(NodeId, String, String)> = Vec::new();
    for draft in std::mem::take(&mut batch.drafts) {
        let label = draft.title.clone();
        match write_one(space, app, &draft, batch.source.as_mut(), by, report) {
            Ok(id) => {
                nodes.insert(draft.source_id.clone(), id);
                if let Some(parent) = &draft.parent {
                    parents.push((id, parent.clone(), label));
                }
            }
            Err(ImportError::Space(e)) => report.skip(label, e.to_string()),
            Err(e) => return Err(e),
        }
    }
    for (child, parent, label) in parents {
        let Some(parent) = nodes.get(&parent) else {
            continue;
        };
        match space.link(&child, parent, link_kinds::PART_OF, by) {
            Ok(_) => report.links += 1,
            Err(e) => report.skip(label, format!("part-of link: {e}")),
        }
    }
    Ok(())
}

fn write_one(
    space: &mut Space,
    app: NoteApp,
    draft: &Draft,
    source: Option<&mut Source>,
    by: &Provenance,
    report: &mut NotesReport,
) -> Result<NodeId, ImportError> {
    let tags: Vec<String> = draft
        .tags
        .iter()
        .filter_map(|t| super::markdown::tag(t))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut props = draft.properties.clone();
    props.insert(keys::APP.into(), app.id().into());
    props.insert(keys::ID.into(), draft.source_id.clone());
    if let Some(folder) = draft.folder.as_ref().filter(|f| !f.is_empty()) {
        props.insert(keys::FOLDER.into(), folder.clone());
    }
    let digest = hash(draft, &tags, &props);
    props.insert(keys::HASH.into(), digest.clone());
    let imported = |id: NodeId| ImportedNote {
        node_id: id,
        source_id: draft.source_id.clone(),
        title: draft.title.clone(),
    };

    let id = match find(space, app, &draft.source_id)? {
        Some(id) => {
            if space.property(&id, keys::HASH)?.as_deref() == Some(digest.as_str()) {
                report.unchanged += 1;
                return Ok(id);
            }
            space.update_node(
                &id,
                NodeUpdate {
                    kind: None,
                    title: Some(draft.title.clone()),
                    body: Some(draft.body.clone()),
                    tags: Some(tags.clone()),
                },
                by,
            )?;
            space.set_properties(&id, &props, by)?;
            if !props.contains_key(keys::FOLDER) {
                space.remove_property(&id, keys::FOLDER)?;
            }
            report.updated.push(imported(id));
            id
        }
        None => {
            let now = Timestamp::now();
            let created = draft.created.unwrap_or(now);
            let updated = draft.updated.unwrap_or(created).max(created);
            let id = node_id(app, &draft.source_id, created);
            let node = NewNode {
                kind: String::new(),
                title: draft.title.clone(),
                body: draft.body.clone(),
                tags: tags.clone(),
            };
            let node = if space.get_node(&id)?.is_some() {
                // That id is taken by a node that is not this note's: a new id.
                space.create_node_with_properties(node, &props, by)?
            } else {
                space.import_node(
                    ImportedNode {
                        id,
                        node,
                        created_at: created,
                        updated_at: updated,
                        created_by: by.clone(),
                        updated_by: by.clone(),
                    },
                    &props,
                    by,
                )?
            };
            report.created.push(imported(node.id));
            node.id
        }
    };

    let mut source = source;
    for (name, blob) in &draft.attachments {
        let what = format!("{} ({name})", draft.title);
        let bytes = match blob {
            Blob::Bytes(bytes) => Ok(bytes.clone()),
            Blob::Entry(entry) => match source.as_deref_mut() {
                Some(source) => source.read(entry),
                None => Err(std::io::Error::other("no export to read it from")),
            },
            Blob::File(path) => std::fs::read(path),
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(e) => {
                report.skip(what, format!("the file could not be read: {e}"));
                continue;
            }
        };
        if bytes.len() as u64 > MAX_ATTACHMENT_SIZE {
            report.skip(
                what,
                format!(
                    "larger than {} MB, the most a space attaches",
                    MAX_ATTACHMENT_SIZE / (1024 * 1024)
                ),
            );
            continue;
        }
        if let Some(current) = space.attachment(&id, name)? {
            if space.read_blob(&current.hash)?.as_deref() == Some(bytes.as_slice()) {
                continue;
            }
            space.detach(&id, name, by)?;
        }
        match space.attach(&id, name, bytes.as_slice(), by) {
            Ok(_) => report.attached += 1,
            Err(e) => report.skip(what, e.to_string()),
        }
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_are_stable_and_dated() {
        let a = node_id(NoteApp::Bear, "X", Timestamp(1_792_489_800_000));
        assert_eq!(a, node_id(NoteApp::Bear, "X", Timestamp(1_792_489_800_000)));
        assert_ne!(
            a,
            node_id(NoteApp::Notion, "X", Timestamp(1_792_489_800_000))
        );
        assert_eq!(a.created_at(), Timestamp(1_792_489_800_000));
    }
}

//! The tools for link kinds, canvases and attachments: `link_kinds`, `read_canvas`,
//! `attach_file` and `read_attachment`.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use knode_format::{Attachment, LinkDirection, LinkKind, NodeId, Provenance, kinds, view_kinds};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Outcome, ToolDefinition, ToolOutput, count, non_empty, provenance_json, resolve_ref};
use crate::backend::Backend;

/// The most bytes `attach_file` attaches: 10 MiB.
pub const MAX_ATTACH_BYTES: u64 = 10 * 1024 * 1024;

/// The most bytes of text `read_attachment` returns: 256 KiB.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

pub(super) fn definitions(node_ref: &str, provenance: &Value) -> Vec<ToolDefinition> {
    let attachment = attachment_schema(provenance);
    vec![
        ToolDefinition {
            name: "link_kinds",
            title: "Kinds of link",
            description: "List the kinds of link the user's Knode memory has, with how each \
                reads from both ends (such as part of / has part) and what it means: the \
                built-in kinds (related, part-of, derived-from, answers, contradicts, and \
                mentions, which comes only from [[links]] in a body) and the kinds this space \
                defines. Call it before link_nodes to pick the kind that says how two nodes \
                relate.",
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "kinds": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "description": "The name to pass to link_nodes." },
                                "label": { "type": "string", "description": "How the link reads from its source." },
                                "inverse_label": { "type": "string", "description": "How it reads from its target; empty for none." },
                                "direction": { "type": "string", "enum": ["directed", "symmetric"] },
                                "description": { "type": "string" },
                                "built_in": { "type": "boolean" }
                            },
                            "required": ["kind", "label", "inverse_label", "direction", "description", "built_in"]
                        }
                    }
                },
                "required": ["kinds"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "read_canvas",
            title: "Read a canvas",
            description: "Read a canvas of the user's Knode memory: a board where the user \
                arranged notes by hand, with groups and links between them. Returns the canvas's \
                title and purpose, each card (node title, kind, position, size, group and first \
                line), the groups and the links between the cards. Leave canvas out to list the \
                canvases. Call read_node on a card for its full text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "canvas": { "type": "string", "description": "The canvas: its node id or exact title. Leave out to list the canvases." }
                }
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "canvases": {
                        "type": "array",
                        "description": "The canvases of the space, when no canvas was given.",
                        "items": {
                            "type": "object",
                            "properties": { "id": { "type": "string" }, "title": { "type": "string" } },
                            "required": ["id", "title"]
                        }
                    },
                    "canvas": {
                        "type": ["object", "null"],
                        "properties": {
                            "id": { "type": "string", "description": "The canvas node id." },
                            "title": { "type": "string" },
                            "body": { "type": "string", "description": "What the canvas is for." }
                        }
                    },
                    "cards": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "The node id." },
                                "title": { "type": "string" },
                                "kind": { "type": "string" },
                                "x": { "type": "number", "description": "The centre, in points; x grows to the right." },
                                "y": { "type": "number", "description": "The centre, in points; y grows down." },
                                "width": { "type": ["number", "null"] },
                                "height": { "type": ["number", "null"] },
                                "group": { "type": ["string", "null"], "description": "The id of its group." },
                                "collapsed": { "type": "boolean" },
                                "excerpt": { "type": "string" }
                            },
                            "required": ["id", "title", "kind", "x", "y", "width", "height", "group", "collapsed", "excerpt"]
                        }
                    },
                    "groups": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "label": { "type": "string" },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "width": { "type": "number" },
                                "height": { "type": "number" }
                            },
                            "required": ["id", "label", "x", "y", "width", "height"]
                        }
                    },
                    "links": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" },
                                "to": { "type": "string" },
                                "kind": { "type": "string" },
                                "label": { "type": "string", "description": "How the link reads from its source." }
                            },
                            "required": ["from", "to", "kind", "label"]
                        }
                    }
                },
                "required": ["canvases", "canvas", "cards", "groups", "links"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "attach_file",
            title: "Attach a file",
            description: "Attach a file to a node of the user's Knode memory, such as a \
                diagram, a screenshot or a PDF the note talks about; ![[name]] in the node's \
                text then shows it. Pass either path, a file inside one of the notes folders the \
                user added to this space (see `knode folders list`), or name and content_base64 \
                with the bytes. At most 10 MB. Attaching the same file again does nothing more.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": node_ref },
                    "path": { "type": "string", "description": "An absolute path to a file inside a notes folder of this space." },
                    "name": { "type": "string", "description": "The file name, such as diagram.svg. Required with content_base64; with path, defaults to the file's name." },
                    "content_base64": { "type": "string", "description": "The bytes of the file, in base64." }
                },
                "required": ["node"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "attachment": attachment },
                "required": ["attachment"]
            }),
            read_only: false,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "read_attachment",
            title: "Read an attachment",
            description: "Read a file attached to a node of the user's Knode memory, by the \
                name read_node lists under attachments. Text files (Markdown, plain text, CSV, \
                JSON, SVG and the like) come back as text, up to 256 KB; for other files you \
                get what they are, their type and size.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": node_ref },
                    "name": { "type": "string", "description": "The attachment's name, from read_node." }
                },
                "required": ["node", "name"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "attachment": attachment,
                    "text": { "type": ["string", "null"], "description": "The content, for a text file; null otherwise." },
                    "truncated": { "type": "boolean", "description": "Whether the text was cut at 256 KB." }
                },
                "required": ["attachment", "text", "truncated"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
    ]
}

/// The JSON Schema of an attachment.
pub(super) fn attachment_schema(provenance: &Value) -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "Its name in the node; ![[name]] shows it." },
            "media_type": { "type": "string", "description": "Such as image/png." },
            "size": { "type": "integer", "description": "In bytes." },
            "hash": { "type": "string", "description": "The SHA-256 of its bytes." },
            "created_at": { "type": "string" },
            "created_by": provenance
        },
        "required": ["name", "media_type", "size", "hash", "created_at", "created_by"]
    })
}

pub(super) fn attachment_json(attachment: &Attachment) -> Value {
    json!({
        "name": attachment.name,
        "media_type": attachment.media_type,
        "size": attachment.size,
        "hash": attachment.hash,
        "created_at": attachment.created_at.to_string(),
        "created_by": provenance_json(&attachment.created_by),
    })
}

/// A size for people, such as `12 KB`.
pub(super) fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

fn kind_json(kind: &LinkKind) -> Value {
    json!({
        "kind": kind.kind,
        "label": kind.label,
        "inverse_label": kind.inverse_label,
        "direction": kind.direction.as_str(),
        "description": kind.description,
        "built_in": kind.built_in,
    })
}

/// One line for a kind, such as `part-of: part of / has part. The source is part of...`.
pub(super) fn kind_line(kind: &LinkKind) -> String {
    let reads = match kind.direction {
        LinkDirection::Symmetric => format!("{} (both ways)", kind.label),
        LinkDirection::Directed => format!("{} / {}", kind.label, kind.label_from(false)),
    };
    let mut line = format!("`{}`: {reads}", kind.kind);
    if !kind.description.is_empty() {
        let _ = write!(line, ". {}", kind.description);
    }
    line
}

pub(super) fn link_kinds<B: Backend + ?Sized>(backend: &B) -> Outcome {
    let kinds = backend.link_kinds()?;
    let mut text = format!(
        "{} (name: from the source / from the target):\n\n",
        count(kinds.len(), "kind of link")
    );
    for kind in &kinds {
        let _ = writeln!(text, "- {}", kind_line(kind));
    }
    text.push_str(
        "\nPass the name as kind to link_nodes. mentions comes only from [[links]] in a body.\n",
    );
    let structured = json!({ "kinds": kinds.iter().map(kind_json).collect::<Vec<_>>() });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
pub(super) struct CanvasArgs {
    canvas: Option<String>,
}

pub(super) fn read_canvas<B: Backend + ?Sized>(backend: &B, args: CanvasArgs) -> Outcome {
    let Some(reference) = non_empty(args.canvas) else {
        let canvases: Vec<_> = backend
            .views()?
            .into_iter()
            .filter(|v| v.kind == view_kinds::CANVAS)
            .filter_map(|v| v.node_id.map(|id| (id, v.name)))
            .collect();
        let mut text = String::new();
        if canvases.is_empty() {
            text.push_str("The space has no canvases.\n");
        } else {
            let _ = writeln!(text, "{}:\n", count(canvases.len(), "canvas"));
            for (id, title) in &canvases {
                let _ = writeln!(text, "- **{title}** (id `{id}`)");
            }
            text.push_str("\nPass one as canvas to read it.\n");
        }
        let structured = json!({
            "canvases": canvases.iter().map(|(id, title)| json!({ "id": id.to_string(), "title": title })).collect::<Vec<_>>(),
            "canvas": null, "cards": [], "groups": [], "links": [],
        });
        return Ok(ToolOutput::ok(text, structured));
    };
    let id = canvas_ref(backend, &reference)?;
    let Some(board) = backend.canvas_board(&id)? else {
        return Err(ToolOutput::error(format!(
            "\"{reference}\" is not a canvas. Leave canvas out to list the canvases."
        )));
    };
    let title_of = |node: &NodeId| {
        board
            .items
            .iter()
            .find(|i| i.position.node_id == *node)
            .map_or_else(|| node.to_string(), |i| i.title.clone())
    };
    let label_of = |kind: &str| {
        board
            .link_kinds
            .iter()
            .find(|k| k.kind == kind)
            .map_or_else(|| kind.to_string(), |k| k.label.clone())
    };
    let group_label = |group: &Option<String>| {
        group.as_ref().and_then(|g| {
            board.groups.iter().find(|x| x.id == *g).map(|x| {
                if x.label.is_empty() {
                    "unnamed group".to_string()
                } else {
                    x.label.clone()
                }
            })
        })
    };

    let mut text = format!("# Canvas: {}\n\n", board.title);
    if !board.body.trim().is_empty() {
        let _ = writeln!(text, "{}\n", board.body.trim_end());
    }
    let _ = writeln!(text, "- id: `{id}`");
    if board.items.is_empty() {
        text.push_str("- no cards yet\n");
    } else {
        let _ = writeln!(
            text,
            "- {} (x grows right, y grows down):",
            count(board.items.len(), "card")
        );
        for item in &board.items {
            let p = &item.position;
            let _ = write!(
                text,
                "  - [[{}]] ({}, id `{}`) at ({:.0}, {:.0})",
                item.title, item.kind, p.node_id, p.x, p.y
            );
            if let (Some(w), Some(h)) = (p.width, p.height) {
                let _ = write!(text, ", {w:.0} x {h:.0}");
            }
            if let Some(group) = group_label(&p.group_id) {
                let _ = write!(text, ", in {group}");
            }
            if !item.excerpt.is_empty() {
                let _ = write!(text, ": {}", item.excerpt);
            }
            text.push('\n');
        }
    }
    if !board.groups.is_empty() {
        let _ = writeln!(text, "- groups:");
        for group in &board.groups {
            let label = if group.label.is_empty() {
                "unnamed group"
            } else {
                group.label.as_str()
            };
            let _ = writeln!(
                text,
                "  - {label} (id `{}`) at ({:.0}, {:.0}), {:.0} x {:.0}",
                group.id, group.x, group.y, group.width, group.height
            );
        }
    }
    if !board.edges.is_empty() {
        let _ = writeln!(text, "- links:");
        for edge in &board.edges {
            let _ = writeln!(
                text,
                "  - [[{}]] {} [[{}]] ({})",
                title_of(&edge.from),
                label_of(&edge.kind),
                title_of(&edge.to),
                edge.kind
            );
        }
    }
    let structured = json!({
        "canvases": [],
        "canvas": { "id": id.to_string(), "title": board.title, "body": board.body },
        "cards": board.items.iter().map(|i| json!({
            "id": i.position.node_id.to_string(),
            "title": i.title,
            "kind": i.kind,
            "x": i.position.x,
            "y": i.position.y,
            "width": i.position.width,
            "height": i.position.height,
            "group": i.position.group_id,
            "collapsed": i.position.display == knode_format::ViewDisplay::Collapsed,
            "excerpt": i.excerpt,
        })).collect::<Vec<_>>(),
        "groups": board.groups.iter().map(|g| json!({
            "id": g.id, "label": g.label, "x": g.x, "y": g.y, "width": g.width, "height": g.height,
        })).collect::<Vec<_>>(),
        "links": board.edges.iter().map(|e| json!({
            "from": e.from.to_string(), "to": e.to.to_string(), "kind": e.kind, "label": label_of(&e.kind),
        })).collect::<Vec<_>>(),
    });
    Ok(ToolOutput::ok(text, structured))
}

/// A canvas by node id or title: a node of kind `canvas` is preferred among nodes with the
/// same title.
fn canvas_ref<B: Backend + ?Sized>(backend: &B, reference: &str) -> Result<NodeId, ToolOutput> {
    if let Ok(id) = NodeId::parse(reference.trim()) {
        return Ok(id);
    }
    let found = backend.find_by_title(reference)?;
    match found
        .iter()
        .find(|n| n.kind == kinds::CANVAS)
        .or(found.first())
    {
        Some(node) => Ok(node.id),
        None => Err(ToolOutput::error(format!(
            "No canvas is titled \"{}\". Leave canvas out to list the canvases.",
            reference.trim()
        ))),
    }
}

#[derive(Deserialize)]
pub(super) struct AttachArgs {
    node: String,
    path: Option<String>,
    name: Option<String>,
    content_base64: Option<String>,
}

fn too_large(name: &str) -> ToolOutput {
    ToolOutput::error(format!(
        "{name} is larger than {} MB, the most an agent attaches. Ask the user to attach it \
         with `knode attach`.",
        MAX_ATTACH_BYTES / (1024 * 1024)
    ))
}

pub(super) fn attach_file<B: Backend + ?Sized>(
    backend: &B,
    args: AttachArgs,
    by: &Provenance,
) -> Outcome {
    let node = resolve_ref(backend, &args.node)?;
    let (name, bytes) = match (non_empty(args.path), args.content_base64) {
        (Some(_), Some(_)) => {
            return Err(ToolOutput::error(
                "Pass either path or content_base64, not both.",
            ));
        }
        (Some(path), None) => {
            let file = allowed_file(backend, Path::new(&path))?;
            let name = non_empty(args.name).unwrap_or_else(|| {
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let len = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            if len > MAX_ATTACH_BYTES {
                return Err(too_large(&name));
            }
            let bytes = std::fs::read(&file)
                .map_err(|e| ToolOutput::error(format!("Cannot read {}: {e}.", file.display())))?;
            (name, bytes)
        }
        (None, Some(content)) => {
            let Some(name) = non_empty(args.name) else {
                return Err(ToolOutput::error(
                    "Pass name, the file name such as diagram.svg, with content_base64.",
                ));
            };
            if (content.len() as u64) / 4 * 3 > MAX_ATTACH_BYTES + 3 {
                return Err(too_large(&name));
            }
            let bytes = BASE64
                .decode(content.trim())
                .map_err(|e| ToolOutput::error(format!("content_base64 is not base64: {e}.")))?;
            (name, bytes)
        }
        (None, None) => {
            return Err(ToolOutput::error(
                "Pass path (a file inside a notes folder of this space) or name and content_base64.",
            ));
        }
    };
    if bytes.len() as u64 > MAX_ATTACH_BYTES {
        return Err(too_large(&name));
    }
    let attachment = backend.attach(&node, &name, &bytes, by)?;
    let title = backend
        .get_node(&node)?
        .map(|n| n.title)
        .unwrap_or_else(|| node.to_string());
    let text = format!(
        "Attached {} ({}, {}) to [[{title}]]. Write ![[{}]] in its text to show it.\n",
        attachment.name,
        attachment.media_type,
        size(attachment.size),
        attachment.name
    );
    Ok(ToolOutput::ok(
        text,
        json!({ "attachment": attachment_json(&attachment) }),
    ))
}

/// The file at `path` when it is inside a `notes` folder of the space that is read, with no
/// hidden file or folder on the way: an agent attaches only what the user put in their notes.
fn allowed_file<B: Backend + ?Sized>(backend: &B, path: &Path) -> Result<PathBuf, ToolOutput> {
    let refuse = |why: &str| {
        ToolOutput::error(format!(
            "Cannot attach {}: {why}. An agent attaches only files inside a notes folder the user \
             added to this space (`knode folders add <dir> --kind notes`); otherwise pass the \
             bytes as content_base64.",
            path.display()
        ))
    };
    if !path.is_absolute() {
        return Err(refuse("the path is not absolute"));
    }
    let file = std::fs::canonicalize(path).map_err(|_| refuse("there is no such file"))?;
    if !file.is_file() {
        return Err(refuse("it is not a file"));
    }
    for folder in backend.folders()? {
        if folder.kind != knode_format::folder_kinds::NOTES || !folder.capture {
            continue;
        }
        let Some(root) = folder.path.and_then(|p| std::fs::canonicalize(p).ok()) else {
            continue;
        };
        let Ok(inside) = file.strip_prefix(&root) else {
            continue;
        };
        let hidden = inside.components().any(|c| match c {
            Component::Normal(part) => part.to_string_lossy().starts_with('.'),
            _ => true,
        });
        if hidden {
            return Err(refuse("hidden files and folders are not attached"));
        }
        return Ok(file);
    }
    Err(refuse("it is not inside a notes folder of this space"))
}

#[derive(Deserialize)]
pub(super) struct ReadAttachmentArgs {
    node: String,
    name: String,
}

/// Whether a media type is text an agent can read.
fn is_text(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || matches!(
            media_type,
            "application/json" | "application/xml" | "image/svg+xml"
        )
}

pub(super) fn read_attachment<B: Backend + ?Sized>(
    backend: &B,
    args: ReadAttachmentArgs,
) -> Outcome {
    let node = resolve_ref(backend, &args.node)?;
    let name = args.name.trim();
    let list = backend.attachments(&node)?;
    let Some(found) = list.iter().find(|a| a.name == name) else {
        let names: Vec<String> = list.iter().map(|a| format!("`{}`", a.name)).collect();
        let known = if names.is_empty() {
            "it has no attachments".to_string()
        } else {
            format!("its attachments are {}", names.join(", "))
        };
        return Err(ToolOutput::error(format!(
            "The node has no attachment named \"{name}\": {known}."
        )));
    };
    let mut text = format!(
        "{} ({}, {}), attached {} by {} ({}).\n",
        found.name,
        found.media_type,
        size(found.size),
        found.created_at,
        found.created_by.actor,
        found.created_by.kind.as_str()
    );
    let mut content = Value::Null;
    let mut truncated = false;
    if is_text(&found.media_type) {
        if found.size > MAX_ATTACH_BYTES {
            text.push_str("\nIt is too large to read here.\n");
        } else if let Some((_, bytes)) = backend.read_attachment(&node, name)? {
            truncated = bytes.len() > MAX_TEXT_BYTES;
            let shown = &bytes[..bytes.len().min(MAX_TEXT_BYTES)];
            let body = String::from_utf8_lossy(shown).into_owned();
            let _ = write!(text, "\n{body}\n");
            if truncated {
                text.push_str("\n(cut at 256 KB)\n");
            }
            content = Value::String(body);
        }
    } else {
        text.push_str("\nIt is not a text file, so its content is not shown here.\n");
    }
    let structured = json!({
        "attachment": attachment_json(found),
        "text": content,
        "truncated": truncated,
    });
    Ok(ToolOutput::ok(text, structured))
}

#[cfg(test)]
mod tests {
    use knode_format::{FolderSpec, LinkKindSpec, NewNode, PositionSpec, Space};
    use serde_json::Map;

    use super::*;
    use crate::backend::DirectBackend;
    use crate::tools::call;

    fn backend() -> (tempfile::TempDir, DirectBackend) {
        let dir = tempfile::tempdir().unwrap();
        let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        (dir, DirectBackend::from_space(space))
    }

    fn run(backend: &DirectBackend, name: &str, value: Value) -> ToolOutput {
        let args: Map<String, Value> = value.as_object().unwrap().clone();
        call(backend, "test-agent", name, args).expect("a known tool")
    }

    fn me() -> Provenance {
        Provenance::person("Ada")
    }

    #[test]
    fn link_kinds_lists_built_in_and_defined_kinds() {
        let (_dir, backend) = backend();
        backend
            .set_link_kind(
                &LinkKindSpec::directed("depends-on", "depends on", "needed by")
                    .with_description("The source needs the target."),
                &me(),
            )
            .unwrap();
        let out = run(&backend, "link_kinds", json!({}));
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text.contains("- `part-of`: part of / has part."),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("- `related`: related to (both ways)."),
            "{}",
            out.text
        );
        assert!(
            out.text
                .contains("- `depends-on`: depends on / needed by. The source needs the target."),
            "{}",
            out.text
        );
        let kinds = out.structured.unwrap()["kinds"].as_array().unwrap().len();
        assert_eq!(kinds, 7);

        // link_nodes takes the kind, and names the kinds when it does not know one.
        let a = backend
            .create_node(NewNode::note("App", ""), &me())
            .unwrap();
        let b = backend.create_node(NewNode::note("Db", ""), &me()).unwrap();
        let out = run(
            &backend,
            "link_nodes",
            json!({ "from": "App", "to": "Db", "kind": "depends-on" }),
        );
        assert_eq!(out.text, "Linked [[App]] depends on [[Db]] (depends-on).\n");
        let out = run(
            &backend,
            "link_nodes",
            json!({ "from": "App", "to": "Db", "kind": "blocks" }),
        );
        assert!(out.is_error);
        assert!(out.text.contains("no link kind \"blocks\""), "{}", out.text);
        assert!(out.text.contains("depends-on"), "{}", out.text);

        // read_node shows each link as it reads from the node.
        let out = run(&backend, "read_node", json!({ "id": b.id.to_string() }));
        assert!(
            out.text.contains(&format!(
                "- backlinks:\n  - needed by [[App]] (depends-on, id `{}`)",
                a.id
            )),
            "{}",
            out.text
        );
        let labeled = &out.structured.unwrap()["labeled_links"][0];
        assert_eq!(labeled["label"], "needed by");
        assert_eq!(labeled["outgoing"], false);
    }

    #[test]
    fn read_canvas_lists_and_reads_canvases() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        let canvas = space
            .create_canvas(NewNode::note("Launch", "What ships when."), &me())
            .unwrap();
        let a = space
            .create_node(NewNode::note("Pricing", "**Per seat** pricing."), &me())
            .unwrap();
        let b = space
            .create_node(NewNode::note("Plan", "[[Pricing]]"), &me())
            .unwrap();
        let mut card = PositionSpec::at(a.id, 10.0, 20.0);
        card.size = Some((120.0, 80.0));
        space
            .set_positions(
                &canvas.id,
                &[card, PositionSpec::at(b.id, 300.0, 20.0)],
                &me(),
            )
            .unwrap();
        let backend = DirectBackend::from_space(space);

        let out = run(&backend, "read_canvas", json!({}));
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("- **Launch** (id"), "{}", out.text);
        let listed = out.structured.unwrap();
        assert_eq!(listed["canvases"][0]["title"], "Launch");
        assert!(listed["canvas"].is_null());

        let out = run(&backend, "read_canvas", json!({ "canvas": "launch" }));
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text
                .starts_with("# Canvas: Launch\n\nWhat ships when.\n"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains("[[Pricing]] (note, id")
                && out
                    .text
                    .contains("at (10, 20), 120 x 80: Per seat pricing."),
            "{}",
            out.text
        );
        assert!(
            out.text
                .contains("  - [[Plan]] mentions [[Pricing]] (mentions)"),
            "{}",
            out.text
        );
        let board = out.structured.unwrap();
        assert_eq!(board["cards"].as_array().unwrap().len(), 2);
        assert_eq!(board["links"][0]["label"], "mentions");

        let out = run(&backend, "read_canvas", json!({ "canvas": "Plan" }));
        assert!(out.is_error);
        assert!(out.text.contains("is not a canvas"), "{}", out.text);
    }

    #[test]
    fn attach_file_takes_bytes_or_files_in_notes_folders() {
        let (dir, backend) = backend();
        let node = backend
            .create_node(NewNode::note("Beds", ""), &me())
            .unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(notes.join(".obsidian")).unwrap();
        std::fs::write(notes.join("beds.md"), "# Beds\n").unwrap();
        std::fs::write(notes.join(".obsidian/app.json"), "{}").unwrap();
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();

        // No notes folder yet: nothing on disk can be attached.
        let path = notes.join("beds.md");
        let out = run(
            &backend,
            "attach_file",
            json!({ "node": "Beds", "path": path.to_string_lossy() }),
        );
        assert!(out.is_error);
        assert!(
            out.text.contains("not inside a notes folder"),
            "{}",
            out.text
        );

        backend
            .with(|s| {
                s.set_folder(
                    &FolderSpec {
                        id: None,
                        kind: "notes".into(),
                        path: notes.to_string_lossy().into_owned(),
                        capture: true,
                        rules: json!({}),
                    },
                    &me(),
                )
            })
            .unwrap();
        let out = run(
            &backend,
            "attach_file",
            json!({ "node": "Beds", "path": path.to_string_lossy() }),
        );
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text
                .starts_with("Attached beds.md (text/markdown, 7 bytes) to [[Beds]].")
        );
        for (refused, why) in [
            (outside.clone(), "not inside a notes folder"),
            (notes.join(".obsidian/app.json"), "hidden"),
            (notes.join("../secret.txt"), "not inside a notes folder"),
            (PathBuf::from("beds.md"), "not absolute"),
        ] {
            let out = run(
                &backend,
                "attach_file",
                json!({ "node": "Beds", "path": refused.to_string_lossy() }),
            );
            assert!(out.is_error, "{}", refused.display());
            assert!(out.text.contains(why), "{}", out.text);
        }

        let out = run(
            &backend,
            "attach_file",
            json!({ "node": node.id.to_string(), "name": "data.csv", "content_base64": BASE64.encode("a,b\n1,2\n") }),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            out.structured.unwrap()["attachment"]["media_type"],
            "text/csv"
        );
        assert!(
            run(
                &backend,
                "attach_file",
                json!({ "node": "Beds", "content_base64": "eA==" })
            )
            .is_error
        );
        assert!(
            run(
                &backend,
                "attach_file",
                json!({ "node": "Beds", "name": "x", "content_base64": "@@" })
            )
            .is_error
        );
        assert!(run(&backend, "attach_file", json!({ "node": "Beds" })).is_error);
        let big = BASE64.encode(vec![0u8; MAX_ATTACH_BYTES as usize + 1]);
        let out = run(
            &backend,
            "attach_file",
            json!({ "node": "Beds", "name": "big.bin", "content_base64": big }),
        );
        assert!(out.text.contains("larger than 10 MB"), "{}", out.text);

        // read_node lists them, read_attachment reads text.
        let out = run(&backend, "read_node", json!({ "title": "Beds" }));
        assert!(out.text.contains("- attachments (read_attachment reads one):\n  - beds.md (text/markdown, 7 bytes)\n  - data.csv"), "{}", out.text);
        assert_eq!(
            out.structured.unwrap()["attachments"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let out = run(
            &backend,
            "read_attachment",
            json!({ "node": "Beds", "name": "data.csv" }),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.structured.unwrap()["text"], "a,b\n1,2\n");
        let out = run(
            &backend,
            "read_attachment",
            json!({ "node": "Beds", "name": "nope" }),
        );
        assert!(
            out.text
                .contains("its attachments are `beds.md`, `data.csv`"),
            "{}",
            out.text
        );
        backend
            .attach(&node.id, "photo.png", b"\x89PNG\r\n\x1a\n...", &me())
            .unwrap();
        let out = run(
            &backend,
            "read_attachment",
            json!({ "node": "Beds", "name": "photo.png" }),
        );
        assert!(out.structured.unwrap()["text"].is_null());
        assert!(out.text.contains("not a text file"), "{}", out.text);
    }
}

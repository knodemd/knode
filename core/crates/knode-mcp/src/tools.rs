//! The tools agents call: `search`, `read_node`, `write_node`, `link_nodes`, `recent`,
//! `history`, `undo`, `link_kinds`, `read_canvas`, `attach_file` and `read_attachment`.
//!
//! Each tool answers with Markdown for the model to read and the same data as structured
//! content that follows the tool's output schema. This module does not depend on the MCP SDK,
//! so the tools can be tested with any [`Backend`].

use std::fmt::Write as _;

use knode_format::{
    ActorKind, Change, ChangeId, Link, Node, NodeId, NodeLink, NodeSummary, NodeUpdate, Provenance,
    SearchHit, actions, link_kinds,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::backend::{Backend, BackendError};

mod maps;

pub use maps::{MAX_ATTACH_BYTES, MAX_TEXT_BYTES};

/// The most results `search` and `recent` return.
pub const MAX_LIMIT: usize = 50;
/// How many results `search` and `recent` return when no limit is given.
pub const DEFAULT_LIMIT: usize = 10;

/// A tool: its name, what it does and the shape of its input and output.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// The name agents call it by.
    pub name: &'static str,
    /// A title for people.
    pub title: &'static str,
    /// What it does and when to use it, for the model.
    pub description: &'static str,
    /// The JSON Schema of its arguments.
    pub input_schema: Value,
    /// The JSON Schema of its structured content.
    pub output_schema: Value,
    /// Whether it only reads.
    pub read_only: bool,
    /// Whether calling it twice with the same arguments does nothing more.
    pub idempotent: bool,
    /// Whether it may take back what was written before, rather than only add.
    pub destructive: bool,
}

/// What a tool call returns.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// Markdown for the model.
    pub text: String,
    /// The same result as data, following the output schema. `None` for errors.
    pub structured: Option<Value>,
    /// Whether the call failed. The text says why and what to do.
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(text: String, structured: Value) -> Self {
        ToolOutput {
            text,
            structured: Some(structured),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        ToolOutput {
            text: text.into(),
            structured: None,
            is_error: true,
        }
    }
}

impl From<BackendError> for ToolOutput {
    fn from(error: BackendError) -> Self {
        ToolOutput::error(error.to_string())
    }
}

/// The names of every tool, in the order they are listed.
pub const TOOL_NAMES: [&str; 11] = [
    "search",
    "read_node",
    "write_node",
    "link_nodes",
    "recent",
    "history",
    "undo",
    "link_kinds",
    "read_canvas",
    "attach_file",
    "read_attachment",
];

/// Every tool, in the order they are listed.
pub fn definitions() -> Vec<ToolDefinition> {
    let provenance = json!({
        "type": "object",
        "description": "Who made the change.",
        "properties": {
            "kind": { "type": "string", "enum": ["person", "agent", "system"] },
            "actor": { "type": "string", "description": "The person or agent, such as claude-code." }
        },
        "required": ["kind", "actor"]
    });
    let summary = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "The node id, a ULID." },
            "title": { "type": "string" },
            "kind": { "type": "string" },
            "created_at": { "type": "string", "description": "RFC 3339, UTC." },
            "updated_at": { "type": "string", "description": "RFC 3339, UTC." },
            "updated_by": provenance
        },
        "required": ["id", "title", "kind", "created_at", "updated_at", "updated_by"]
    });
    let link = json!({
        "type": "object",
        "properties": {
            "from": { "type": "string" },
            "from_title": { "type": "string" },
            "to": { "type": "string" },
            "to_title": { "type": "string" },
            "kind": { "type": "string" },
            "created_at": { "type": "string" },
            "created_by": provenance
        },
        "required": ["from", "from_title", "to", "to_title", "kind", "created_at", "created_by"]
    });
    let node = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "title": { "type": "string" },
            "kind": { "type": "string" },
            "body": { "type": "string", "description": "The text, in Markdown." },
            "tags": { "type": "array", "items": { "type": "string" } },
            "created_at": { "type": "string" },
            "created_by": provenance,
            "updated_at": { "type": "string" },
            "updated_by": provenance
        },
        "required": ["id", "title", "kind", "body", "tags", "created_at", "created_by", "updated_at", "updated_by"]
    });
    let limit = json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_LIMIT,
        "default": DEFAULT_LIMIT,
        "description": "How many nodes to return at most."
    });
    let node_ref = "A node id, or the exact title of a node (case and extra spaces are ignored).";
    let change = json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": "The change id, to pass to undo." },
            "action": { "type": "string", "description": "What was done, such as node.create, node.update, node.delete, link.create or undo." },
            "subject": { "type": "string", "description": "What it was done to: a node id, or another subject such as a setting." },
            "subject_title": { "type": ["string", "null"], "description": "The current title of the node, or null when it is not a node that exists now." },
            "changed_at": { "type": "string", "description": "RFC 3339, UTC." },
            "changed_by": provenance,
            "undoes": { "type": ["integer", "null"], "description": "For an undo, the change it undid." }
        },
        "required": ["id", "action", "subject", "subject_title", "changed_at", "changed_by", "undoes"]
    });

    let attachment = maps::attachment_schema(&provenance);
    let labeled = json!({
        "type": "object",
        "properties": {
            "node": { "type": "string", "description": "The node at the other end." },
            "title": { "type": "string" },
            "kind": { "type": "string" },
            "label": { "type": "string", "description": "How the link reads from this node, such as part of or has part." },
            "outgoing": { "type": "boolean", "description": "true for a link from this node, false for a backlink." }
        },
        "required": ["node", "title", "kind", "label", "outgoing"]
    });
    let mut tools = vec![
        ToolDefinition {
            name: "search",
            title: "Search memory",
            description: "Search the user's Knode memory: the notes, decisions and past \
                conversations they and their agents have saved. Use it before answering \
                questions about earlier work, past decisions, preferences or project context, \
                and before writing a note, to update an existing one instead of making a \
                duplicate. Matches every word in titles and text, ignoring case and accents; \
                the last word also matches as a prefix. Use a few distinctive words, not a \
                sentence. Returns the best matches with a snippet; call read_node for the full text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A few words to look for, such as \"pricing decision\"." },
                    "limit": limit
                },
                "required": ["query"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "hits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "node": summary,
                                "snippet": { "type": "string", "description": "Text around the words found, matches in **bold**." },
                                "score": { "type": "number", "description": "Higher is better, only comparable within one search." }
                            },
                            "required": ["node", "snippet", "score"]
                        }
                    }
                },
                "required": ["query", "hits"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "read_node",
            title: "Read a node",
            description: "Read one node of the user's Knode memory in full: its Markdown text, \
                kind, tags, who wrote it and when, its links and backlinks with how each reads \
                (such as part of, has part), and its attached files. Pass the id from search or \
                recent, or the node's exact title. [[Title]] in the text is a link to another \
                node you can read the same way; read_attachment reads an attached file. The version it returns lets write_node \
                merge your rewrite with what others change meanwhile.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The node id, from search or recent." },
                    "title": { "type": "string", "description": "The exact title, when you do not have the id." }
                }
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "node": node,
                    "links": { "type": "array", "items": link, "description": "Links from this node." },
                    "backlinks": { "type": "array", "items": link, "description": "Links to this node." },
                    "labeled_links": { "type": "array", "items": labeled, "description": "Links going out, then backlinks, each with how it reads from this node. A link of a symmetric kind between two nodes is listed once." },
                    "attachments": { "type": "array", "items": attachment, "description": "Files attached to this node." },
                    "same_title": { "type": "array", "items": summary, "description": "Other nodes with the same title." },
                    "version": { "type": ["string", "null"], "description": "The version of the text read. Pass it to write_node with a new body." }
                },
                "required": ["node", "links", "backlinks", "labeled_links", "attachments", "same_title"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "write_node",
            title: "Write a node",
            description: "Save something worth remembering to the user's Knode memory, where \
                every agent and the user can find it later: a decision and its reasons, a \
                finding, a plan, a summary of what was worked out. Write the body in Markdown \
                (GitHub Flavored), and link other nodes by writing [[Their title]]. With an id, \
                edits that node. Without one, edits the node with this exact title if there is \
                one, or creates it. Fields left out stay as they are; set append to add to the \
                end of the body instead of replacing it. When you rewrite a body you read, pass \
                the version read_node gave: if the user or another agent changed the node since, \
                your edit is merged with theirs instead of overwriting it. Search first to avoid \
                duplicates. Keep one topic per node, with a short, specific title.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The node to edit. Leave out to find the node by title or create it." },
                    "title": { "type": "string", "description": "The title. Required to create a node; with an id, renames it, and [[links]] to it follow." },
                    "body": { "type": "string", "description": "The text, in Markdown. Replaces the body unless append is true." },
                    "append": { "type": "boolean", "default": false, "description": "Add body to the end of the existing text, after a blank line." },
                    "version": { "type": "string", "description": "The version read_node gave for the text you rewrote. Changes made since are kept. Left out, the version you last read is used." },
                    "kind": { "type": "string", "description": "What the node is: note (the default), decision, idea, source, or another lower-case word." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags, replacing the current ones." }
                }
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "result": { "type": "string", "enum": ["created", "updated", "unchanged"] },
                    "node": summary,
                    "merged": { "type": "boolean", "description": "True when others changed the body since the version you read, and your edit was merged with theirs." }
                },
                "required": ["result", "node"]
            }),
            read_only: false,
            idempotent: false,
            destructive: false,
        },
        ToolDefinition {
            name: "link_nodes",
            title: "Link two nodes",
            description: "Link two nodes of the user's Knode memory with a kind that says how \
                they relate, so either can be found from the other: related (the default), \
                part-of (a note of a project), derived-from (a summary of a conversation), \
                answers (an answer to a question), contradicts, or a kind this space defines; \
                link_kinds lists them all with their meaning. Links written as [[Title]] in a \
                body are made automatically; this tool is for links that are not in the text. \
                Linking twice does nothing more.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": node_ref },
                    "to": { "type": "string", "description": node_ref },
                    "kind": {
                        "type": "string",
                        "default": link_kinds::RELATED,
                        "description": "A kind from link_kinds: related (the default), part-of, derived-from, answers, contradicts, or one this space defines. Not mentions, which comes only from [[links]] in a body."
                    }
                },
                "required": ["from", "to"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "link": link },
                "required": ["link"]
            }),
            read_only: false,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "recent",
            title: "Recent nodes",
            description: "List the nodes of the user's Knode memory that changed most recently, \
                newest first, with who changed them. Use it to pick up where the user or another \
                agent left off, or to see what was saved lately.",
            input_schema: json!({
                "type": "object",
                "properties": { "limit": limit }
            }),
            output_schema: json!({
                "type": "object",
                "properties": { "nodes": { "type": "array", "items": summary } },
                "required": ["nodes"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "history",
            title: "Change history",
            description: "List the latest changes to the user's Knode memory, newest first:                 who changed what and when, as a person or as an agent. Pass a node (its id or                 exact title) for the changes to that node only, including links made or removed                 from it; a deleted node's history is still there by id. Leave it out for the                 whole space. Use it to see what you or another agent wrote, or to find the id                 of a change of your own to pass to undo.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": node_ref },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "default": DEFAULT_LIMIT,
                        "description": "How many changes to return at most."
                    }
                }
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": ["string", "null"], "description": "The node id, or null for the whole space." },
                    "changes": { "type": "array", "items": change }
                },
                "required": ["node", "changes"]
            }),
            read_only: true,
            idempotent: true,
            destructive: false,
        },
        ToolDefinition {
            name: "undo",
            title: "Undo a change",
            description: "Undo one change that you, this agent, made to the user's Knode memory:                 a node you created is deleted, a node you changed or deleted goes back to how it                 was just before, a link you made or removed is put back. Get the change id from                 history. Only your own changes can be undone here, and never when someone made                 a later change to the same nodes or links: then tell the user, who can undo it                 with `knode undo <change> --force` after checking. Nothing is lost: the undo is                 one more change, and undoing it redoes yours.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "change": { "type": "integer", "description": "The id of the change to undo, from history." }
                },
                "required": ["change"]
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "undone": { "type": "integer", "description": "The change that was undone." },
                    "change": change
                },
                "required": ["undone", "change"]
            }),
            read_only: false,
            idempotent: false,
            destructive: true,
        },
    ];
    tools.extend(maps::definitions(node_ref, &provenance));
    tools
}

/// Runs a tool for the agent `agent` (the client name from MCP), or returns `None` if there is
/// no tool with this name.
pub fn call<B: Backend + ?Sized>(
    backend: &B,
    agent: &str,
    name: &str,
    arguments: Map<String, Value>,
) -> Option<ToolOutput> {
    let by = Provenance::agent(agent);
    let output = match name {
        "search" => parse(arguments).and_then(|args| search(backend, args)),
        "read_node" => parse(arguments).and_then(|args| read_node(backend, args)),
        "write_node" => parse(arguments).and_then(|args| write_node(backend, args, &by)),
        "link_nodes" => parse(arguments).and_then(|args| link_nodes(backend, args, &by)),
        "recent" => parse(arguments).and_then(|args| recent(backend, args)),
        "history" => parse(arguments).and_then(|args| history(backend, args)),
        "undo" => parse(arguments).and_then(|args| undo(backend, args, &by)),
        "link_kinds" => maps::link_kinds(backend),
        "read_canvas" => parse(arguments).and_then(|args| maps::read_canvas(backend, args)),
        "attach_file" => parse(arguments).and_then(|args| maps::attach_file(backend, args, &by)),
        "read_attachment" => parse(arguments).and_then(|args| maps::read_attachment(backend, args)),
        _ => return None,
    };
    Some(output.unwrap_or_else(|e| e))
}

type Outcome = Result<ToolOutput, ToolOutput>;

fn parse<T: for<'de> Deserialize<'de>>(arguments: Map<String, Value>) -> Result<T, ToolOutput> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|e| ToolOutput::error(format!("Invalid arguments: {e}.")))
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
}

fn search<B: Backend + ?Sized>(backend: &B, args: SearchArgs) -> Outcome {
    let query = args.query.trim();
    if query.is_empty() {
        return Err(ToolOutput::error(
            "The query is empty. Pass a few words to look for.",
        ));
    }
    let hits = backend.search(query, clamp_limit(args.limit))?;
    let mut text = String::new();
    if hits.is_empty() {
        let _ = write!(
            text,
            "No nodes match \"{query}\". Try fewer or other words, or `recent` to see what changed lately."
        );
    } else {
        let _ = writeln!(text, "{} for \"{query}\":", count(hits.len(), "node"));
        for (i, hit) in hits.iter().enumerate() {
            let _ = write!(text, "\n{}. ", i + 1);
            summary_line(&mut text, &hit.node);
            let snippet = one_line(&hit.snippet);
            if !snippet.is_empty() {
                let _ = write!(text, "\n   {snippet}");
            }
            text.push('\n');
        }
    }
    let structured = json!({
        "query": query,
        "hits": hits.iter().map(hit_json).collect::<Vec<_>>(),
    });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
struct ReadArgs {
    id: Option<String>,
    title: Option<String>,
}

fn read_node<B: Backend + ?Sized>(backend: &B, args: ReadArgs) -> Outcome {
    let (node, same_title) = match (non_empty(args.id), non_empty(args.title)) {
        (Some(id), _) => {
            let id = parse_id(&id)?;
            let node = backend.get_node(&id)?.ok_or_else(|| {
                ToolOutput::error(format!("No node has the id {id}. Use search to find it."))
            })?;
            (node, Vec::new())
        }
        (None, Some(title)) => {
            let mut found = backend.find_by_title(&title)?;
            if found.is_empty() {
                return Err(ToolOutput::error(format!(
                    "No node is titled \"{title}\". Use search to find it."
                )));
            }
            let first = found.remove(0);
            let node = backend.get_node(&first.id)?.ok_or_else(|| {
                ToolOutput::error(format!("The node \"{title}\" was just deleted."))
            })?;
            (node, found)
        }
        (None, None) => {
            return Err(ToolOutput::error("Pass the id of the node, or its title."));
        }
    };
    let mut node = node;
    // The body and its version together, so a rewrite of it merges from exactly this text.
    let version = match backend.open_body(&node.id)? {
        Some(doc) => {
            node.body = doc.text;
            Some(doc.version)
        }
        None => None,
    };
    let links = backend.links_from(&node.id)?;
    let backlinks = backend.backlinks(&node.id)?;
    let labeled = backend.node_links(&node.id)?;
    let attachments = backend.attachments(&node.id)?;

    let mut text = String::new();
    let _ = writeln!(text, "# {}\n", node.title);
    if node.body.trim().is_empty() {
        text.push_str("_(empty)_\n");
    } else {
        text.push_str(node.body.trim_end());
        text.push('\n');
    }
    let _ = writeln!(text, "\n---\n");
    let _ = writeln!(text, "- id: `{}`", node.id);
    let _ = writeln!(text, "- kind: {}", node.kind);
    if !node.tags.is_empty() {
        let _ = writeln!(text, "- tags: {}", node.tags.join(", "));
    }
    let _ = writeln!(
        text,
        "- created: {} by {}",
        node.created_at,
        who(&node.created_by)
    );
    let _ = writeln!(
        text,
        "- updated: {} by {}",
        node.updated_at,
        who(&node.updated_by)
    );
    labeled_lines(&mut text, "links", labeled.iter().filter(|l| l.outgoing));
    labeled_lines(
        &mut text,
        "backlinks",
        labeled.iter().filter(|l| !l.outgoing),
    );
    if !attachments.is_empty() {
        let _ = writeln!(text, "- attachments (read_attachment reads one):");
        for a in &attachments {
            let _ = writeln!(
                text,
                "  - {} ({}, {})",
                a.name,
                a.media_type,
                maps::size(a.size)
            );
        }
    }
    if !same_title.is_empty() {
        let ids: Vec<String> = same_title.iter().map(|n| format!("`{}`", n.id)).collect();
        let _ = writeln!(text, "- other nodes with this title: {}", ids.join(", "));
    }
    if let Some(version) = &version {
        let _ = writeln!(
            text,
            "- version: `{version}` (pass it to write_node with a new body)"
        );
    }

    let structured = json!({
        "node": node_json(&node),
        "links": links.iter().map(link_json).collect::<Vec<_>>(),
        "backlinks": backlinks.iter().map(link_json).collect::<Vec<_>>(),
        "labeled_links": labeled.iter().map(|l| json!({
            "node": l.node.to_string(),
            "title": l.title,
            "kind": l.link.kind,
            "label": l.label,
            "outgoing": l.outgoing,
        })).collect::<Vec<_>>(),
        "attachments": attachments.iter().map(maps::attachment_json).collect::<Vec<_>>(),
        "same_title": same_title.iter().map(summary_json).collect::<Vec<_>>(),
        "version": version,
    });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
struct WriteArgs {
    id: Option<String>,
    title: Option<String>,
    body: Option<String>,
    #[serde(default)]
    append: bool,
    kind: Option<String>,
    tags: Option<Vec<String>>,
    version: Option<String>,
}

fn write_node<B: Backend + ?Sized>(backend: &B, args: WriteArgs, by: &Provenance) -> Outcome {
    let title = non_empty(args.title.clone());
    let kind = non_empty(args.kind.clone());
    let existing = match non_empty(args.id.clone()) {
        Some(id) => {
            let id = parse_id(&id)?;
            let node = backend.get_node(&id)?.ok_or_else(|| {
                ToolOutput::error(format!(
                    "No node has the id {id}. Leave the id out to create a node."
                ))
            })?;
            Some((node, title.clone()))
        }
        None => {
            let Some(title) = title.clone() else {
                return Err(ToolOutput::error(
                    "Pass a title to create a node, or the id of the node to edit.",
                ));
            };
            match backend.find_by_title(&title)?.first() {
                Some(found) => backend.get_node(&found.id)?.map(|node| (node, None)),
                None => None,
            }
        }
    };

    let mut merged = false;
    let (result, node) = match existing {
        Some((current, new_title)) => {
            let (body, appended) = match (args.body, args.append) {
                (Some(extra), true) => (None, Some(extra)),
                (body, _) => (body, None),
            };
            let update = NodeUpdate {
                kind,
                title: new_title,
                body,
                tags: args.tags,
            };
            let before = current.updated_at;
            let written = backend.write_body(&current.id, update, args.version.as_deref(), by)?;
            merged = written.merged;
            let mut node = written.node;
            if let Some(extra) = appended {
                // Added at the end of the text as it is now, merged with edits made meanwhile.
                node = backend.append_body(&current.id, &extra, by)?.node;
            }
            let result = if node.updated_at == before && node == current {
                "unchanged"
            } else {
                "updated"
            };
            (result, node)
        }
        None => {
            let new = knode_format::NewNode {
                kind: kind.unwrap_or_default(),
                title: title.unwrap_or_default(),
                body: args.body.unwrap_or_default(),
                tags: args.tags.unwrap_or_default(),
            };
            ("created", backend.create_node(new, by)?)
        }
    };

    let verb = match result {
        "created" => "Created",
        "updated" => "Updated",
        _ => "No change to",
    };
    let mut text = format!(
        "{verb} {} [[{}]] (id `{}`).\n",
        node.kind, node.title, node.id
    );
    if merged {
        text.push_str(
            "Someone else changed this node since you read it. Your edit was merged with \
             theirs, so nothing they wrote was overwritten. Read it again with read_node to see \
             the result before changing it further.\n",
        );
    }
    let structured = json!({
        "result": result,
        "node": summary_json(&summary_of(&node)),
        "merged": merged,
    });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
struct LinkArgs {
    from: String,
    to: String,
    kind: Option<String>,
}

fn link_nodes<B: Backend + ?Sized>(backend: &B, args: LinkArgs, by: &Provenance) -> Outcome {
    let from = resolve_ref(backend, &args.from)?;
    let to = resolve_ref(backend, &args.to)?;
    let kind = non_empty(args.kind).unwrap_or_else(|| link_kinds::RELATED.to_string());
    if kind == link_kinds::MENTIONS {
        return Err(ToolOutput::error(
            "mentions links come only from [[links]] in a body. Use related, or edit the body with write_node.",
        ));
    }
    let kinds = backend.link_kinds()?;
    let Some(known) = kinds.iter().find(|k| k.kind == kind) else {
        let names: Vec<&str> = kinds
            .iter()
            .filter(|k| k.kind != link_kinds::MENTIONS)
            .map(|k| k.kind.as_str())
            .collect();
        return Err(ToolOutput::error(format!(
            "This space has no link kind \"{kind}\". Use one of {}; link_kinds says what each \
             means. Ask the user to define a new kind with `knode kinds add`.",
            names.join(", ")
        )));
    };
    let link = backend.link(&from, &to, &kind, by)?;
    let text = format!(
        "Linked [[{}]] {} [[{}]] ({}).\n",
        link.from_title, known.label, link.to_title, link.kind
    );
    Ok(ToolOutput::ok(text, json!({ "link": link_json(&link) })))
}

#[derive(Deserialize)]
struct RecentArgs {
    limit: Option<usize>,
}

fn recent<B: Backend + ?Sized>(backend: &B, args: RecentArgs) -> Outcome {
    let nodes = backend.recent(clamp_limit(args.limit))?;
    let mut text = String::new();
    if nodes.is_empty() {
        text.push_str("The space is empty. Save something with write_node.\n");
    } else {
        let _ = writeln!(
            text,
            "{} changed most recently:",
            count(nodes.len(), "node")
        );
        for (i, node) in nodes.iter().enumerate() {
            let _ = write!(text, "\n{}. ", i + 1);
            summary_line(&mut text, node);
        }
        text.push('\n');
    }
    let structured = json!({ "nodes": nodes.iter().map(summary_json).collect::<Vec<_>>() });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
struct HistoryArgs {
    node: Option<String>,
    limit: Option<usize>,
}

fn history<B: Backend + ?Sized>(backend: &B, args: HistoryArgs) -> Outcome {
    let limit = clamp_limit(args.limit);
    let node = match non_empty(args.node) {
        // A deleted node has no title any more, but its id still has a history.
        Some(node) => Some(match NodeId::parse(&node) {
            Ok(id) => id,
            Err(_) => resolve_ref(backend, &node)?,
        }),
        None => None,
    };
    let changes = match &node {
        Some(id) => backend.changes_of(&id.to_string(), limit)?,
        None => backend.changes(limit, None)?,
    };
    let mut titles = std::collections::HashMap::new();
    for change in &changes {
        if let Ok(id) = NodeId::parse(&change.subject)
            && let std::collections::hash_map::Entry::Vacant(entry) = titles.entry(id)
        {
            entry.insert(backend.get_node(&id)?.map(|n| n.title));
        }
    }
    let title_of = |change: &Change| -> Option<String> {
        NodeId::parse(&change.subject)
            .ok()
            .and_then(|id| titles.get(&id).cloned().flatten())
    };
    let mut text = String::new();
    if changes.is_empty() {
        text.push_str("No changes yet.\n");
    } else {
        let what = match &node {
            Some(id) => match titles.get(id).cloned().flatten() {
                Some(title) => format!("to [[{title}]]"),
                None => format!("to node `{id}`"),
            },
            None => "to the space".to_string(),
        };
        let _ = writeln!(
            text,
            "The latest {} {what}, newest first:\n",
            count(changes.len(), "change")
        );
        for change in &changes {
            let subject = match title_of(change) {
                Some(title) => format!(" [[{title}]]"),
                None if change.subject.is_empty() => String::new(),
                None => format!(" `{}`", change.subject),
            };
            let undoes = change
                .undoes
                .map(|u| format!(" (undoes change {u})"))
                .unwrap_or_default();
            let _ = writeln!(
                text,
                "- change {}: {}{subject}{undoes}, {} by {}",
                change.id,
                change.action,
                change.changed_at,
                who(&change.changed_by)
            );
        }
    }
    let structured = json!({
        "node": node.map(|id| id.to_string()),
        "changes": changes.iter().map(|c| change_json(c, title_of(c))).collect::<Vec<_>>(),
    });
    Ok(ToolOutput::ok(text, structured))
}

#[derive(Deserialize)]
struct UndoArgs {
    change: ChangeId,
}

fn undo<B: Backend + ?Sized>(backend: &B, args: UndoArgs, by: &Provenance) -> Outcome {
    let Some(detail) = backend.change(args.change)? else {
        return Err(ToolOutput::error(format!(
            "There is no change {}. Use history to find the id of your change.",
            args.change
        )));
    };
    let original = &detail.change;
    // Knode Service checks the name of the agent's key itself; the name the client gives is
    // only trusted for a space opened directly.
    let mine = original.changed_by.kind == ActorKind::Agent
        && (backend.knows_the_agent() || original.changed_by.actor == by.actor);
    if !mine {
        return Err(ToolOutput::error(format!(
            "Change {} was made by {}, not by you ({}). You can only undo your own changes; \
             ask the user, who can run `knode undo {}` if they want it undone.",
            original.id,
            who(&original.changed_by),
            by.actor,
            original.id
        )));
    }
    let undo = backend.undo(original.id, false, by).map_err(|e| match e {
        BackendError::Invalid(message) => ToolOutput::error(format!(
            "{message}. Nothing was changed. If it is still worth undoing, tell the user: they \
             can check the history and run `knode undo {} --force`.",
            original.id
        )),
        other => other.into(),
    })?;
    let title = match NodeId::parse(&undo.subject) {
        Ok(id) => backend.get_node(&id)?.map(|n| n.title),
        Err(_) => None,
    };
    let what = match (&title, original.action.as_str()) {
        (_, actions::NODE_CREATE) => "the node it created was deleted".to_string(),
        (Some(title), _) => format!("[[{title}]] is back to how it was before"),
        (None, _) => "what it changed is back to how it was before".to_string(),
    };
    let text = format!(
        "Undid change {} ({}): {what}. The undo is change {}; undo it to redo yours.\n",
        original.id, original.action, undo.id
    );
    let structured = json!({ "undone": original.id, "change": change_json(&undo, title) });
    Ok(ToolOutput::ok(text, structured))
}

fn change_json(change: &Change, subject_title: Option<String>) -> Value {
    json!({
        "id": change.id,
        "action": change.action,
        "subject": change.subject,
        "subject_title": subject_title,
        "changed_at": change.changed_at.to_string(),
        "changed_by": provenance_json(&change.changed_by),
        "undoes": change.undoes,
    })
}

fn resolve_ref<B: Backend + ?Sized>(backend: &B, reference: &str) -> Result<NodeId, ToolOutput> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(ToolOutput::error(
            "Pass a node id or title for from and to.",
        ));
    }
    if let Ok(id) = NodeId::parse(reference)
        && backend.get_node(&id)?.is_some()
    {
        return Ok(id);
    }
    match backend.find_by_title(reference)?.first() {
        Some(found) => Ok(found.id),
        None => Err(ToolOutput::error(format!(
            "No node has the id or title \"{reference}\". Use search to find it, or write_node to create it."
        ))),
    }
}

fn parse_id(text: &str) -> Result<NodeId, ToolOutput> {
    NodeId::parse(text).map_err(|_| {
        ToolOutput::error(format!(
            "\"{text}\" is not a node id. Ids look like 01K83N6V4W5D2XQJ0Y7RZ9T1AB; pass a title instead if you have one."
        ))
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn who(by: &Provenance) -> String {
    format!("{} ({})", by.actor, by.kind.as_str())
}

fn summary_line(text: &mut String, node: &NodeSummary) {
    let _ = write!(
        text,
        "**{}** ({}, id `{}`), updated {} by {}",
        node.title,
        node.kind,
        node.id,
        node.updated_at,
        who(&node.updated_by)
    );
}

/// Links as a node shows them, such as `- links:` then `  - part of [[Plan]] (part-of, id ..)`.
fn labeled_lines<'a>(text: &mut String, heading: &str, links: impl Iterator<Item = &'a NodeLink>) {
    let mut links = links.peekable();
    if links.peek().is_none() {
        return;
    }
    let _ = writeln!(text, "- {heading}:");
    for link in links {
        let _ = writeln!(
            text,
            "  - {} [[{}]] ({}, id `{}`)",
            link.label, link.title, link.link.kind, link.node
        );
    }
}

fn summary_of(node: &Node) -> NodeSummary {
    NodeSummary {
        id: node.id,
        kind: node.kind.clone(),
        title: node.title.clone(),
        created_at: node.created_at,
        updated_at: node.updated_at,
        updated_by: node.updated_by.clone(),
    }
}

fn provenance_json(by: &Provenance) -> Value {
    json!({ "kind": by.kind.as_str(), "actor": by.actor })
}

fn summary_json(node: &NodeSummary) -> Value {
    json!({
        "id": node.id.to_string(),
        "title": node.title,
        "kind": node.kind,
        "created_at": node.created_at.to_string(),
        "updated_at": node.updated_at.to_string(),
        "updated_by": provenance_json(&node.updated_by),
    })
}

fn hit_json(hit: &SearchHit) -> Value {
    json!({
        "node": summary_json(&hit.node),
        "snippet": hit.snippet,
        "score": hit.score,
    })
}

fn node_json(node: &Node) -> Value {
    json!({
        "id": node.id.to_string(),
        "title": node.title,
        "kind": node.kind,
        "body": node.body,
        "tags": node.tags,
        "created_at": node.created_at.to_string(),
        "created_by": provenance_json(&node.created_by),
        "updated_at": node.updated_at.to_string(),
        "updated_by": provenance_json(&node.updated_by),
    })
}

fn link_json(link: &Link) -> Value {
    json!({
        "from": link.from.to_string(),
        "from_title": link.from_title,
        "to": link.to.to_string(),
        "to_title": link.to_title,
        "kind": link.kind,
        "created_at": link.created_at.to_string(),
        "created_by": provenance_json(&link.created_by),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DirectBackend;
    use knode_format::{ActorKind, Space};

    fn backend() -> (tempfile::TempDir, DirectBackend) {
        let dir = tempfile::tempdir().unwrap();
        let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        (dir, DirectBackend::from_space(space))
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn run(backend: &DirectBackend, name: &str, value: Value) -> ToolOutput {
        call(backend, "test-agent", name, args(value)).expect("a known tool")
    }

    /// Checks that every required key of the schema is in the value, recursively.
    fn conforms(schema: &Value, value: &Value) -> bool {
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                let Some(object) = value.as_object() else {
                    return false;
                };
                let required = schema["required"].as_array().cloned().unwrap_or_default();
                let props = schema["properties"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                required
                    .iter()
                    .all(|key| object.contains_key(key.as_str().unwrap()))
                    && object
                        .iter()
                        .all(|(key, v)| props.get(key).is_none_or(|s| conforms(s, v)))
            }
            Some("array") => value
                .as_array()
                .is_some_and(|items| items.iter().all(|v| conforms(&schema["items"], v))),
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("integer") => value.is_i64() || value.is_u64(),
            Some("boolean") => value.is_boolean(),
            _ => true,
        }
    }

    fn check_schema(name: &str, output: &ToolOutput) {
        let def = definitions().into_iter().find(|d| d.name == name).unwrap();
        let structured = output.structured.as_ref().expect("structured content");
        assert!(
            conforms(&def.output_schema, structured),
            "{name} output does not follow its schema: {structured:#}"
        );
    }

    #[test]
    fn every_tool_is_defined_once() {
        let names: Vec<&str> = definitions().iter().map(|d| d.name).collect();
        assert_eq!(names, TOOL_NAMES);
        for def in definitions() {
            assert_eq!(def.input_schema["type"], "object");
            assert_eq!(def.output_schema["type"], "object");
            assert!(
                def.description.len() > 80,
                "{} needs a description",
                def.name
            );
        }
    }

    #[test]
    fn unknown_tools_are_none() {
        let (_dir, backend) = backend();
        assert!(call(&backend, "a", "delete_everything", Map::new()).is_none());
    }

    #[test]
    fn a_rewrite_based_on_the_version_read_keeps_later_edits() {
        let (_dir, backend) = backend();
        let out = run(
            &backend,
            "write_node",
            json!({ "title": "Plan", "body": "Ship the format." }),
        );
        let id = out.structured.unwrap()["node"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let read = run(&backend, "read_node", json!({ "id": id }));
        check_schema("read_node", &read);
        let version = read.structured.unwrap()["version"]
            .as_str()
            .unwrap()
            .to_string();

        // Another agent adds a line meanwhile.
        let other = call(
            &backend,
            "other-agent",
            "write_node",
            args(json!({ "id": id, "body": "Their line.", "append": true })),
        )
        .unwrap();
        assert!(!other.is_error, "{}", other.text);

        let out = run(
            &backend,
            "write_node",
            json!({ "id": id, "body": "Ship the format first.", "version": version }),
        );
        assert!(!out.is_error, "{}", out.text);
        check_schema("write_node", &out);
        assert!(out.text.contains("merged"), "{}", out.text);
        let node = backend
            .get_node(&NodeId::parse(&id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(node.body, "Ship the format first.\n\nTheir line.");

        let bad = run(
            &backend,
            "write_node",
            json!({ "id": id, "body": "x", "version": "v1" }),
        );
        assert!(bad.is_error);
        assert!(bad.text.contains("read_node"), "{}", bad.text);
    }

    #[test]
    fn write_creates_then_updates_by_title() {
        let (_dir, backend) = backend();
        let out = run(
            &backend,
            "write_node",
            json!({ "title": "Pricing", "body": "Per seat.", "tags": ["Biz"] }),
        );
        assert!(!out.is_error, "{}", out.text);
        check_schema("write_node", &out);
        let structured = out.structured.unwrap();
        assert_eq!(structured["result"], "created");
        assert_eq!(structured["node"]["updated_by"]["actor"], "test-agent");
        assert_eq!(structured["node"]["updated_by"]["kind"], "agent");
        let id = structured["node"]["id"].as_str().unwrap().to_string();

        let out = run(
            &backend,
            "write_node",
            json!({ "title": "pricing", "body": "Flat fee.", "append": true }),
        );
        assert_eq!(out.structured.unwrap()["result"], "updated");
        let node = backend
            .get_node(&NodeId::parse(&id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(node.body, "Per seat.\n\nFlat fee.");
        assert_eq!(node.tags, ["biz"]);
        assert_eq!(node.updated_by.kind, ActorKind::Agent);

        let out = run(
            &backend,
            "write_node",
            json!({ "id": id, "body": "Flat fee." }),
        );
        assert_eq!(out.structured.unwrap()["result"], "updated");
        let out = run(
            &backend,
            "write_node",
            json!({ "id": id, "body": "Flat fee." }),
        );
        assert_eq!(out.structured.unwrap()["result"], "unchanged");
        assert!(out.text.starts_with("No change to note [[Pricing]]"));
    }

    #[test]
    fn write_explains_what_is_missing() {
        let (_dir, backend) = backend();
        let out = run(&backend, "write_node", json!({ "body": "No title" }));
        assert!(out.is_error);
        assert!(out.text.contains("Pass a title"));
        let out = run(&backend, "write_node", json!({ "id": "nope" }));
        assert!(out.is_error);
        assert!(out.text.contains("not a node id"));
        let out = run(
            &backend,
            "write_node",
            json!({ "title": "x", "kind": "Bad Kind" }),
        );
        assert!(out.is_error, "{}", out.text);
        let out = run(&backend, "write_node", json!({ "title": 3 }));
        assert!(out.is_error);
        assert!(out.text.starts_with("Invalid arguments"));
    }

    #[test]
    fn search_read_link_and_recent() {
        let (_dir, backend) = backend();
        run(
            &backend,
            "write_node",
            json!({ "title": "Roadmap", "body": "Ship v0.1 this week." }),
        );
        run(
            &backend,
            "write_node",
            json!({ "title": "Plan", "body": "Follow the [[Roadmap]].", "kind": "decision" }),
        );

        let out = run(&backend, "search", json!({ "query": "ship" }));
        check_schema("search", &out);
        assert!(out.text.contains("**Roadmap**"), "{}", out.text);
        assert!(out.text.contains("**Ship**"), "{}", out.text);
        assert_eq!(
            out.structured.unwrap()["hits"][0]["node"]["title"],
            "Roadmap"
        );

        let out = run(&backend, "search", json!({ "query": "zebra" }));
        assert!(out.text.starts_with("No nodes match"));
        assert!(run(&backend, "search", json!({ "query": " " })).is_error);

        let out = run(&backend, "read_node", json!({ "title": "roadmap" }));
        check_schema("read_node", &out);
        assert!(out.text.starts_with("# Roadmap\n\nShip v0.1 this week.\n"));
        assert!(
            out.text
                .contains("- backlinks:\n  - mentioned in [[Plan]] (mentions"),
            "{}",
            out.text
        );
        let roadmap = out.structured.unwrap()["node"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = run(&backend, "read_node", json!({ "id": roadmap }));
        assert_eq!(
            out.structured.unwrap()["node"]["body"],
            "Ship v0.1 this week."
        );
        assert!(run(&backend, "read_node", json!({})).is_error);
        assert!(run(&backend, "read_node", json!({ "title": "None" })).is_error);

        let out = run(
            &backend,
            "link_nodes",
            json!({ "from": roadmap, "to": "Plan", "kind": "part-of" }),
        );
        assert!(!out.is_error, "{}", out.text);
        check_schema("link_nodes", &out);
        assert_eq!(out.text, "Linked [[Roadmap]] part of [[Plan]] (part-of).\n");
        assert!(
            run(
                &backend,
                "link_nodes",
                json!({ "from": "Roadmap", "to": "Plan", "kind": "mentions" })
            )
            .is_error
        );
        assert!(
            run(
                &backend,
                "link_nodes",
                json!({ "from": "Roadmap", "to": "Nowhere" })
            )
            .is_error
        );

        let out = run(&backend, "recent", json!({ "limit": 1 }));
        check_schema("recent", &out);
        let nodes = out.structured.unwrap()["nodes"].as_array().unwrap().len();
        assert_eq!(nodes, 1);
        assert!(out.text.starts_with("1 node changed most recently"));
    }

    #[test]
    fn history_lists_changes_and_undo_takes_back_only_your_own() {
        let (_dir, backend) = backend();
        let out = run(&backend, "history", json!({}));
        check_schema("history", &out);
        assert_eq!(out.text, "No changes yet.\n");

        let created = run(
            &backend,
            "write_node",
            json!({ "title": "Plan", "body": "One." }),
        );
        let id = created.structured.unwrap()["node"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        run(&backend, "write_node", json!({ "id": id, "body": "Two." }));
        let person = Provenance::person("Ada");
        let other = backend
            .create_node(knode_format::NewNode::note("Hers", "Mine."), &person)
            .unwrap();

        let out = run(&backend, "history", json!({ "node": "plan" }));
        check_schema("history", &out);
        assert!(
            out.text
                .starts_with("The latest 2 changes to [[Plan]], newest first:"),
            "{}",
            out.text
        );
        assert!(out.text.contains("node.update [[Plan]], "), "{}", out.text);
        assert!(out.text.contains("by test-agent (agent)"), "{}", out.text);
        let changes = out.structured.unwrap()["changes"].clone();
        let update = changes[0]["id"].as_i64().unwrap();
        assert_eq!(changes[0]["subject_title"], "Plan");

        let out = run(&backend, "history", json!({ "limit": 1 }));
        assert!(out.text.contains("[[Hers]]"), "{}", out.text);
        let hers = out.structured.unwrap()["changes"][0]["id"]
            .as_i64()
            .unwrap();

        // Someone else's change: refused, and nothing changes.
        let out = run(&backend, "undo", json!({ "change": hers }));
        assert!(out.is_error);
        assert!(
            out.text.contains("by Ada (person), not by you"),
            "{}",
            out.text
        );
        assert!(backend.get_node(&other.id).unwrap().is_some());
        assert!(run(&backend, "undo", json!({ "change": 999 })).is_error);

        // Its own change: undone.
        let out = run(&backend, "undo", json!({ "change": update }));
        assert!(!out.is_error, "{}", out.text);
        check_schema("undo", &out);
        assert!(
            out.text.starts_with(&format!(
                "Undid change {update} (node.update): [[Plan]] is back"
            )),
            "{}",
            out.text
        );
        let node = backend
            .get_node(&NodeId::parse(&id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(node.body, "One.");
        let again = run(&backend, "undo", json!({ "change": update }));
        assert!(again.is_error);
        assert!(again.text.contains("already undone"), "{}", again.text);

        // A change of its own with later changes by someone else is not forced.
        let create = changes[1]["id"].as_i64().unwrap();
        backend
            .update_node(
                &node.id,
                NodeUpdate {
                    body: Some("Ada's edit.".into()),
                    ..Default::default()
                },
                &person,
            )
            .unwrap();
        let out = run(&backend, "undo", json!({ "change": create }));
        assert!(out.is_error);
        assert!(out.text.contains("knode undo"), "{}", out.text);
        assert_eq!(
            backend.get_node(&node.id).unwrap().unwrap().body,
            "Ada's edit."
        );

        // A deleted node's history is still there by id.
        let deleted = run(
            &backend,
            "write_node",
            json!({ "title": "Gone", "body": "x" }),
        );
        let gone = deleted.structured.unwrap()["node"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let gone_id = NodeId::parse(&gone).unwrap();
        let undo_create = backend.changes_of(&gone, 1).unwrap()[0].id;
        run(&backend, "undo", json!({ "change": undo_create }));
        assert!(backend.get_node(&gone_id).unwrap().is_none());
        let out = run(&backend, "history", json!({ "node": gone }));
        assert!(
            out.text.contains(&format!("to node `{gone}`")),
            "{}",
            out.text
        );
    }

    #[test]
    fn an_unavailable_space_says_what_to_do() {
        let backend = DirectBackend::unavailable("no space: run `knode init <name>`");
        let out = call(&backend, "a", "recent", Map::new()).unwrap();
        assert!(out.is_error);
        assert_eq!(out.text, "no space: run `knode init <name>`");

        let backend = DirectBackend::new("/does/not/exist.knode");
        let out = call(&backend, "a", "recent", Map::new()).unwrap();
        assert!(out.is_error);
        assert!(
            out.text.contains("cannot open the Knode space"),
            "{}",
            out.text
        );
    }
}

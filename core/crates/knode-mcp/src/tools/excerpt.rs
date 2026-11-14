//! Bounded body retrieval, without unbounded links, attachments or duplicate-title lists.
use knode_format::NodeId;
use serde::Deserialize;
use serde_json::json;

use super::{Outcome, ToolDefinition, ToolOutput};
use crate::backend::Backend;

const DEFAULT_CHARS: usize = 2_000;
const MAX_CHARS: usize = 8_000;

pub(super) fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_excerpt",
        title: "Read part of a node",
        description: "Read a bounded range of a note's Markdown body by node id. Start with the id from search and start=0; continue with next_start and the returned snapshot. Offsets count Unicode scalar values, not bytes or tokens. A changed snapshot is rejected: restart at zero. The excerpt omits links and other metadata; use read_node when you need the complete note or before rewriting it. Partial text may omit qualifications: read surrounding ranges as needed. This is not a token or billing guarantee.",
        input_schema: json!({
            "type": "object", "properties": {
                "id": {"type":"string"},
                "start": {"type":"integer", "minimum":0, "default":0},
                "max_chars": {"type":"integer", "minimum":1, "maximum":MAX_CHARS, "default":DEFAULT_CHARS},
                "snapshot": {"type":"string", "description":"Required when start is nonzero; use the previous read_excerpt snapshot, not a write_node version."}
            }, "required":["id"], "additionalProperties":false
        }),
        output_schema: json!({
            "type":"object", "properties": {
                "id":{"type":"string"}, "title":{"type":"string"},
                "text":{"type":"string"}, "snapshot":{"type":"string"},
                "start":{"type":"integer"}, "end":{"type":"integer"},
                "total_chars":{"type":"integer"}, "next_start":{"type":["integer","null"]},
                "partial":{"type":"boolean"}
            }, "required":["id","title","text","snapshot","start","end","total_chars","next_start","partial"]
        }),
        read_only: true,
        idempotent: true,
        destructive: false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    id: String,
    #[serde(default)]
    start: usize,
    max_chars: Option<usize>,
    snapshot: Option<String>,
}

pub(super) fn read<B: Backend + ?Sized>(backend: &B, args: Args) -> Outcome {
    let limit = args.max_chars.unwrap_or(DEFAULT_CHARS);
    if !(1..=MAX_CHARS).contains(&limit) {
        return Err(ToolOutput::error("max_chars must be between 1 and 8000."));
    }
    if args.start > 0 && args.snapshot.is_none() {
        return Err(ToolOutput::error(
            "Pass the previous snapshot to continue, or restart at start=0.",
        ));
    }
    let id = NodeId::parse(&args.id)
        .map_err(|_| ToolOutput::error("Pass a valid node id from search."))?;
    let node = backend
        .get_node(&id)?
        .ok_or_else(|| ToolOutput::error("Node not found. Use search to find it."))?;
    let doc = backend.open_body(&id)?.ok_or_else(|| {
        ToolOutput::error("This backend cannot provide a versioned excerpt. Use read_node.")
    })?;
    if args.snapshot.as_ref().is_some_and(|v| v != &doc.version) {
        return Err(ToolOutput::error(
            "The note changed since that snapshot. Restart at start=0; do not concatenate ranges from different snapshots.",
        ));
    }
    let total = doc.text.chars().count();
    if args.start > total {
        return Err(ToolOutput::error(
            "start is beyond the end of the note. Restart at start=0.",
        ));
    }
    let text: String = doc.text.chars().skip(args.start).take(limit).collect();
    let end = args.start + text.chars().count();
    let next = (end < total).then_some(end);
    let partial = args.start > 0 || end < total;
    // Titles are user data too; do not let an oversized title defeat the response bound.
    let title: String = node.title.chars().take(160).collect();
    let rendered = format!(
        "# {title}\n\n{text}\n\n---\nRange {}..{end} of {total} characters; partial: {partial}. Next start: {}.\nSnapshot: {}\nUse the same snapshot to continue. This is a body excerpt, not a complete note; read_node includes links and is required before a full rewrite.\n",
        args.start,
        next.map_or_else(|| "none".into(), |n| n.to_string()),
        doc.version
    );
    Ok(ToolOutput::ok(
        rendered,
        json!({
            "id":id.to_string(), "title":title, "text":text, "snapshot":doc.version,
            "start":args.start, "end":end, "total_chars":total, "next_start":next, "partial":partial
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DirectBackend;
    use crate::tools::{call, definitions};
    use knode_format::{NewNode, Provenance, Space};
    use serde_json::Value;

    fn invoke(backend: &DirectBackend, args: Value) -> ToolOutput {
        call(
            backend,
            "excerpt-test",
            "read_excerpt",
            args.as_object().unwrap().clone(),
        )
        .unwrap()
    }
    #[test]
    fn unicode_pages_reassemble_exactly_and_stale_snapshots_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        let body =
            "á🦀e\u{301}\r\n# Context\nOriginal decision.\n# Qualification\nOnly for a demo.";
        let node = space
            .create_node(NewNode::note("Demo", body), &Provenance::person("Tester"))
            .unwrap();
        let backend = DirectBackend::from_space(space);
        let mut start = 0;
        let mut snapshot = None;
        let mut restored = String::new();
        loop {
            let result = invoke(
                &backend,
                json!({"id":node.id.to_string(),"start":start,"max_chars":3,"snapshot":snapshot}),
            );
            assert!(!result.is_error, "{}", result.text);
            let data = result.structured.unwrap();
            restored.push_str(data["text"].as_str().unwrap());
            snapshot = Some(data["snapshot"].as_str().unwrap().to_owned());
            if data["next_start"].is_null() {
                break;
            }
            start = data["next_start"].as_u64().unwrap();
        }
        assert_eq!(restored, body);
        let write = call(
            &backend,
            "excerpt-test",
            "write_node",
            json!({"id":node.id.to_string(),"body":"changed"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert!(!write.is_error);
        let stale = invoke(
            &backend,
            json!({"id":node.id.to_string(),"start":1,"snapshot":snapshot}),
        );
        assert!(stale.is_error);
        assert!(stale.text.contains("changed"));
    }
    #[test]
    fn bounds_empty_notes_and_missing_continuation_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        let empty = space
            .create_node(NewNode::note("Empty", ""), &Provenance::person("Tester"))
            .unwrap();
        let long = space
            .create_node(
                NewNode::note("Long", "x".repeat(100_000)),
                &Provenance::person("Tester"),
            )
            .unwrap();
        let backend = DirectBackend::from_space(space);
        let r = invoke(&backend, json!({"id":empty.id.to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.unwrap()["next_start"], Value::Null);
        for args in [
            json!({"id":long.id.to_string(),"max_chars":0}),
            json!({"id":long.id.to_string(),"max_chars":8001}),
            json!({"id":long.id.to_string(),"start":1}),
        ] {
            assert!(invoke(&backend, args).is_error);
        }
        let r = invoke(&backend, json!({"id":long.id.to_string()}));
        assert_eq!(
            r.structured.as_ref().unwrap()["text"]
                .as_str()
                .unwrap()
                .len(),
            2000
        );
        assert!(r.text.len() < 4000);
        let excerpt_bytes = r.text.len()
            + serde_json::to_vec(r.structured.as_ref().unwrap())
                .unwrap()
                .len();
        let full = call(
            &backend,
            "excerpt-test",
            "read_node",
            json!({"id":long.id.to_string()})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        let full_bytes = full.text.len()
            + serde_json::to_vec(full.structured.as_ref().unwrap())
                .unwrap()
                .len();
        assert!(excerpt_bytes < 8_000);
        assert!(full_bytes > 200_000);
        // ASCII fixture: transport bytes only, not provider tokens or measured billing.
        eprintln!(
            "100k-character fixture: full response {full_bytes} bytes; excerpt {excerpt_bytes} bytes (text + structured content)"
        );

        assert!(
            definitions()
                .iter()
                .any(|d| d.name == "read_excerpt" && d.read_only)
        );
    }
}

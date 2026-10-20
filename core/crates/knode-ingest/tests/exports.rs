//! Importing data exports. The fixtures in `fixtures/exports` are synthetic: invented
//! conversations written by hand in the structure of each export.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use knode_format::{ChangeOp, NodeId, Provenance, Space, kinds, link_kinds, property_keys};
use knode_ingest::{
    CLAUDE_PROJECT_PROPERTY, ExportFormat, ImportError, detect_export, import_export,
};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/exports")
        .join(name)
}

fn by() -> Provenance {
    Provenance::person("example")
}

fn space(dir: &TempDir) -> Space {
    Space::create(dir.path().join("Test.knode"), "Test").unwrap()
}

/// Zips a folder, its files under `prefix` (empty for the top of the archive).
fn zip_dir(dir: &Path, prefix: &str, out: &Path) {
    let mut writer = zip::ZipWriter::new(File::create(out).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            writer
                .start_file(format!("{prefix}{rel}"), options)
                .unwrap();
            writer.write_all(&fs::read(&path).unwrap()).unwrap();
        }
    }
    writer.finish().unwrap();
}

fn node_with(space: &Space, key: &str, value: &str) -> NodeId {
    let found = space.find_by_property(key, value).unwrap();
    assert_eq!(found.len(), 1, "one node with {key} = {value}");
    found[0].id
}

#[test]
fn imports_a_claude_export_with_its_projects() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let path = fixture("claude");
    assert_eq!(detect_export(&path).unwrap(), ExportFormat::ClaudeAi);
    let report = import_export(&mut space, &path, &by()).unwrap();
    assert_eq!(report.format, ExportFormat::ClaudeAi);
    assert_eq!(report.created().count(), 2);
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.empty, 1);
    assert!(report.skipped.is_empty());

    let id = node_with(
        &space,
        property_keys::CONVERSATION_SESSION,
        "c1c1c1c1-0000-4000-8000-000000000001",
    );
    let node = space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.kind, kinds::CONVERSATION);
    assert_eq!(node.title, "Tomato spacing");
    assert_eq!(
        node.body,
        "# Tomato spacing\n\n## User\n\nHow far apart should tomatoes be planted?\n\n\
         _[attached: bed-layout.txt]_\n\n_[image]_\n\n## Claude\n\n- `web_search`: \
         `tomato plant spacing`\n\n### Spacing\nPlant them **45 to 60 cm** apart.\n\n\
         ## User\n\nThanks!\n"
    );
    let properties = space.properties(&id).unwrap();
    assert_eq!(properties[property_keys::CONVERSATION_AGENT], "claude-ai");
    assert_eq!(
        properties[property_keys::CONVERSATION_STARTED_AT],
        "2025-03-02T10:00:00.000Z"
    );
    assert_eq!(
        properties[property_keys::CONVERSATION_ENDED_AT],
        "2025-03-02T10:03:00.000Z"
    );
    let history = space.history(&id).unwrap();
    assert_eq!(history[0].changed_by.actor, "example");
    assert_eq!(
        history[0].changed_by.session.as_deref(),
        Some("c1c1c1c1-0000-4000-8000-000000000001")
    );

    // A conversation with no name takes its title from the first prompt.
    let untitled = node_with(
        &space,
        property_keys::CONVERSATION_SESSION,
        "c1c1c1c1-0000-4000-8000-000000000002",
    );
    assert_eq!(
        space.get_node(&untitled).unwrap().unwrap().title,
        "Write a haiku about rain"
    );

    let project = node_with(
        &space,
        CLAUDE_PROJECT_PROPERTY,
        "b1b1b1b1-0000-4000-8000-000000000001",
    );
    let project_node = space.get_node(&project).unwrap().unwrap();
    assert_eq!(project_node.title, "Garden planning");
    assert!(
        project_node
            .body
            .contains("## Instructions\n\nAnswer briefly")
    );
    assert!(
        project_node
            .body
            .contains("### beds.md\n\n### Beds\nThree raised beds")
    );
    let links = space.links_from(&id).unwrap();
    assert!(
        links
            .iter()
            .any(|link| link.kind == link_kinds::PART_OF && link.to == project)
    );
}

#[test]
fn importing_again_updates_instead_of_duplicating() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    import_export(&mut space, fixture("claude"), &by()).unwrap();
    let count = space.node_count().unwrap();

    let again = import_export(&mut space, fixture("claude"), &by()).unwrap();
    assert!(again.conversations.is_empty());
    assert!(again.projects.is_empty());
    assert_eq!(again.unchanged, 3);
    assert_eq!(space.node_count().unwrap(), count);

    // A newer export where a conversation grew updates its node.
    let newer = dir.path().join("newer");
    fs::create_dir(&newer).unwrap();
    for file in ["projects.json", "users.json"] {
        fs::copy(fixture("claude").join(file), newer.join(file)).unwrap();
    }
    let mut conversations: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture("claude/conversations.json")).unwrap()).unwrap();
    conversations[1]["chat_messages"]
        .as_array_mut()
        .unwrap()
        .push(
            serde_json::json!({"uuid": "m6", "sender": "human", "text": "Another one",
            "created_at": "2025-03-04T18:05:00.000000Z", "content": []}),
        );
    fs::write(
        newer.join("conversations.json"),
        serde_json::to_vec(&conversations).unwrap(),
    )
    .unwrap();
    let report = import_export(&mut space, &newer, &by()).unwrap();
    assert_eq!(report.updated().count(), 1);
    assert_eq!(report.created().count(), 0);
    assert_eq!(
        report.conversations[0].export_id,
        "c1c1c1c1-0000-4000-8000-000000000002"
    );
    assert_eq!(space.node_count().unwrap(), count);
}

#[test]
fn reads_a_zip_as_downloaded() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("data-2025-03-08.zip");
    zip_dir(&fixture("claude"), "", &archive);
    let mut space = space(&dir);
    assert_eq!(detect_export(&archive).unwrap(), ExportFormat::ClaudeAi);
    let report = import_export(&mut space, &archive, &by()).unwrap();
    assert_eq!(report.created().count(), 2);

    // The same export as a folder finds the same nodes.
    let report = import_export(&mut space, fixture("claude"), &by()).unwrap();
    assert!(report.conversations.is_empty());

    // A zip whose files sit in a folder is read too.
    let nested = dir.path().join("chatgpt.zip");
    zip_dir(&fixture("chatgpt"), "export-2025/", &nested);
    let report = import_export(&mut space, &nested, &by()).unwrap();
    assert_eq!(report.format, ExportFormat::ChatGpt);
    assert_eq!(report.created().count(), 1);
}

#[test]
fn imports_the_current_branch_of_chatgpt_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = import_export(&mut space, fixture("chatgpt"), &by()).unwrap();
    assert_eq!(report.format, ExportFormat::ChatGpt);
    assert_eq!(report.created().count(), 1);
    assert_eq!(report.empty, 1);

    let id = node_with(
        &space,
        property_keys::CONVERSATION_SESSION,
        "67c4a1b0-0000-8000-8000-000000000001",
    );
    let node = space.get_node(&id).unwrap().unwrap();
    assert_eq!(node.title, "Bread hydration");
    assert_eq!(
        node.body,
        "# Bread hydration\n\n## User\n\n_[image]_\n\nWhat hydration is this dough?\n\n\
         ## ChatGPT\n\n- `python`: `flour = 500…`\n- `web.run`: `sourdough hydration chart`\n\n\
         That is **75%** hydration .\n\n## User\n\nDraw me a loaf\n\n## ChatGPT\n\n\
         - `dalle.text2im`: `A rustic sourdough loaf`\n\n_[image]_\n\nHere is your loaf.\n"
    );
    let properties = space.properties(&id).unwrap();
    assert_eq!(properties[property_keys::CONVERSATION_AGENT], "chatgpt");
    assert_eq!(properties[property_keys::CONVERSATION_MODEL], "gpt-4o");
    assert_eq!(
        properties[property_keys::CONVERSATION_STARTED_AT],
        "2025-03-02T10:00:00.123Z"
    );
}

#[test]
fn streams_a_large_chatgpt_export() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    fs::create_dir(&export).unwrap();
    fs::write(export.join("chat.html"), "<html></html>").unwrap();
    let mut out = std::io::BufWriter::new(File::create(export.join("conversations.json")).unwrap());
    out.write_all(b"[").unwrap();
    let total = 200;
    for n in 0..total {
        if n > 0 {
            out.write_all(b",").unwrap();
        }
        let conversation = serde_json::json!({
            "id": format!("conv-{n:04}"), "title": format!("Conversation {n}"),
            "create_time": 1_740_000_000.0 + n as f64, "current_node": "a",
            "mapping": {
                "u": {"id": "u", "parent": null, "children": ["a"], "message": {
                    "author": {"role": "user"}, "create_time": 1_740_000_000.0 + n as f64,
                    "content": {"content_type": "text", "parts": ["x".repeat(2000)]}}},
                "a": {"id": "a", "parent": "u", "children": [], "message": {
                    "author": {"role": "assistant"}, "recipient": "all",
                    "content": {"content_type": "text", "parts": [format!("Answer {n}")]}}}
            }
        });
        serde_json::to_writer(&mut out, &conversation).unwrap();
    }
    out.write_all(b"]").unwrap();
    out.flush().unwrap();
    drop(out);

    let mut space = space(&dir);
    let report = import_export(&mut space, &export, &by()).unwrap();
    assert_eq!(report.created().count(), total);
    assert_eq!(space.node_count().unwrap(), total as u64);
}

#[test]
fn imports_gemini_activity_from_takeout_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = import_export(&mut space, fixture("gemini-json"), &by()).unwrap();
    assert_eq!(report.format, ExportFormat::GeminiTakeout);
    // Two prompts five minutes apart are one conversation; a prompt the next day another.
    assert_eq!(report.created().count(), 2);
    let first = &report.conversations[0];
    assert_eq!(first.export_id, "takeout-1741251900000");
    assert_eq!(first.title, "What is the gravity on Mars?");
    let node = space.get_node(&first.node_id).unwrap().unwrap();
    assert_eq!(
        node.body,
        "# What is the gravity on Mars?\n\n## User\n\nWhat is the gravity on Mars?\n\n\
         _[image]_\n\n## Gemini\n\nGravity on Mars is about **3.71 m/s²**.\n\n- 38% of \
         Earth's\n- Varies slightly\n\n## User\n\nAnd on the moon?\n\n## Gemini\n\n\
         About **1.62 m/s²**, a sixth of Earth's.\n"
    );
    let properties = space.properties(&first.node_id).unwrap();
    assert_eq!(properties[property_keys::CONVERSATION_AGENT], "gemini-web");
    assert_eq!(
        properties[property_keys::CONVERSATION_ENDED_AT],
        "2025-03-06T09:10:00.000Z"
    );

    let again = import_export(&mut space, fixture("gemini-json"), &by()).unwrap();
    assert!(again.conversations.is_empty());
    assert_eq!(again.unchanged, 2);
}

#[test]
fn imports_gemini_activity_from_takeout_html() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let archive = dir.path().join("takeout-20250308T000000Z-001.zip");
    zip_dir(&fixture("gemini-html"), "", &archive);
    let report = import_export(&mut space, &archive, &by()).unwrap();
    assert_eq!(report.format, ExportFormat::GeminiTakeout);
    assert_eq!(report.created().count(), 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].item, "Broken date");
    // The same prompts as the JSON form, at the same times, give the same session.
    assert_eq!(report.conversations[0].export_id, "takeout-1741251900000");
}

#[test]
fn rejects_what_is_not_an_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();
    assert!(matches!(
        import_export(&mut space, &empty, &by()),
        Err(ImportError::UnknownFormat(_))
    ));

    let broken = dir.path().join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join("projects.json"), "[]").unwrap();
    fs::write(broken.join("conversations.json"), "[{\"uuid\": ").unwrap();
    assert!(matches!(
        import_export(&mut space, &broken, &by()),
        Err(ImportError::Json { .. })
    ));

    let not_zip = dir.path().join("export.zip");
    fs::write(&not_zip, "not a zip").unwrap();
    assert!(matches!(
        import_export(&mut space, &not_zip, &by()),
        Err(ImportError::Io(_))
    ));
}

#[test]
fn reports_what_each_import_did() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(&dir);
    let report = import_export(&mut space, fixture("claude"), &by()).unwrap();
    assert!(
        report
            .conversations
            .iter()
            .all(|c| c.op == ChangeOp::Create)
    );
    assert_eq!(report.projects[0].title, "Garden planning");
}

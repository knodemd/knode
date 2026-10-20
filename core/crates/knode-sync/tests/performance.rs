//! How fast a long note edits together. Run with
//! `cargo test --release -p knode-sync --test performance -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use knode_format::{NewNode, Provenance, Space};
use knode_sync::loro::{ExportMode, LoroDoc};
use knode_sync::{BODY, BodyEdit, Docs, Version};

fn note_of(bytes: usize) -> String {
    let paragraph = "Knode keeps what you and your agents know in one place, linked and \
                     searchable, so the next conversation starts where the last one ended.\n\n";
    paragraph.repeat(bytes / paragraph.len() + 1)[..bytes].to_string()
}

fn folder_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        total += if meta.is_dir() {
            folder_size(&entry.path())
        } else {
            meta.len()
        };
    }
    total
}

fn report(what: &str, edits: usize, took: Duration) {
    println!(
        "{what}: {edits} edits in {:.2?}, {:.2?} per edit",
        took,
        took / edits as u32
    );
}

#[test]
#[ignore]
fn a_50_kb_note_with_1000_edits_stays_fast() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = Space::create(dir.path().join("Perf.knode"), "Perf").unwrap();
    let ada = Provenance::person("Ada");
    let claude = Provenance::agent("claude-code");
    let body = note_of(50 * 1024);
    let node = space
        .create_node(NewNode::note("Long note", &body), &ada)
        .unwrap();
    let id = node.id;
    let mut docs = Docs::new();

    let start = Instant::now();
    docs.open_doc(&mut space, &id).unwrap();
    println!("first open (makes the document): {:.2?}", start.elapsed());

    // A person typing: 1,000 single-character splices.
    let start = Instant::now();
    for i in 0..1000 {
        let at = (i * 37) % 50_000;
        docs.apply_local_edit(&mut space, &id, BodyEdit::insert(at, "x"), &ada)
            .unwrap();
    }
    report("person typing (splices)", 1000, start.elapsed());

    // An agent rewriting the whole note, each time from the version it read.
    let start = Instant::now();
    for i in 0..1000 {
        let open = docs.open_doc(&mut space, &id).unwrap();
        let mut text = open.text;
        let at = text.char_indices().nth((i * 53) % 40_000).unwrap().0;
        text.insert_str(at, "agent ");
        docs.apply_local_edit(
            &mut space,
            &id,
            BodyEdit::text(text).based_on(open.version),
            &claude,
        )
        .unwrap();
    }
    report("agent rewriting (full text, diffed)", 1000, start.elapsed());

    // An open editor with its own copy sending its updates.
    let editor = LoroDoc::new();
    editor
        .import(
            &docs
                .export_updates_since(&mut space, &id, &Version::empty())
                .unwrap(),
        )
        .unwrap();
    editor
        .set_peer_id(docs.new_peer(&mut space, &ada).unwrap())
        .unwrap();
    let start = Instant::now();
    for i in 0..1000 {
        let before = editor.oplog_vv();
        editor
            .get_text(BODY)
            .insert((i * 41) % 50_000, "y")
            .unwrap();
        editor.commit();
        let update = editor.export(ExportMode::updates(&before)).unwrap();
        docs.merge_updates(&mut space, &id, &update).unwrap();
    }
    report("editor updates merged", 1000, start.elapsed());

    let text = space.get_node(&id).unwrap().unwrap().body;
    assert_eq!(text.len(), 50 * 1024 + 1000 + 6000 + 1000);
    assert_eq!(editor.get_text(BODY).to_string().len(), text.len());

    let start = Instant::now();
    let compaction = docs.compact(&space, &id).unwrap();
    println!(
        "compaction: {:.2?}, snapshot {} KB",
        start.elapsed(),
        compaction.snapshot_bytes / 1024
    );
    let start = Instant::now();
    let open = Docs::new().open_doc(&mut space, &id).unwrap();
    println!("open in a new program: {:.2?}", start.elapsed());
    assert_eq!(open.text, text);
    println!(
        "sync/ {} KB, knode.db with history of 3,000 versions {} KB",
        folder_size(&space.loro_dir(&id)) / 1024,
        folder_size(space.path()) / 1024 - folder_size(&space.loro_dir(&id)) / 1024
    );
}

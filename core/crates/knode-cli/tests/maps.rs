//! `knode links`, `knode link`, `knode kinds`, `knode attach`, `knode attachments` and
//! `knode import` of data exports.

mod common;

use std::fs;

use common::{Sandbox, ingest_fixture};
use serde_json::Value;

/// A sandbox with a space holding the notes "App" and "Database".
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    for title in ["App", "Database"] {
        let file = sb.path(&format!("{title}.md"));
        fs::write(&file, format!("# {title}\n")).unwrap();
        sb.ok(&["import", file.to_str().unwrap()]);
    }
    sb
}

#[test]
fn links_are_typed_and_read_from_both_ends() {
    let sb = sandbox();
    let out = sb.ok(&["kinds"]);
    assert!(out.contains("part-of"), "{out}");
    assert!(out.contains("[built in]"), "{out}");

    let err = sb.fails(&["link", "App", "Database", "--kind", "depends-on"]);
    assert!(err.contains("no link kind \"depends-on\""), "{err}");
    let out = sb.ok(&[
        "kinds",
        "add",
        "depends-on",
        "--label",
        "depends on",
        "--inverse",
        "needed by",
        "--description",
        "The source needs the target.",
    ]);
    assert!(
        out.contains("reads \"depends on\" from its source and \"needed by\""),
        "{out}"
    );
    let out = sb.ok(&["kinds", "list", "--json"]);
    let kinds: Value = serde_json::from_str(&out).unwrap();
    assert!(
        kinds
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["kind"] == "depends-on" && k["created_by"]["actor"] == "tester")
    );

    let out = sb.ok(&["link", "App", "Database", "--kind", "depends-on"]);
    assert_eq!(
        out.trim(),
        "Linked \"App\" depends on \"Database\" (depends-on)."
    );
    let out = sb.ok(&["link", "App", "Database"]);
    assert!(out.contains("related to"), "{out}");

    let out = sb.ok(&["links", "Database"]);
    assert!(out.contains("<- needed by \"App\"  (depends-on,"), "{out}");
    assert!(out.contains("related to \"App\""), "{out}");
    let out = sb.ok(&["links", "App", "--json"]);
    let links: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(links.as_array().unwrap().len(), 2);
    assert!(
        sb.fails(&["links", "Nowhere"])
            .contains("no node has the id or title")
    );

    let err = sb.fails(&["kinds", "remove", "depends-on"]);
    assert!(err.contains("depends-on"), "{err}");
    assert!(
        sb.fails(&["kinds", "remove", "part-of"])
            .contains("built-in")
    );
    assert!(
        sb.fails(&["link", "App", "Database", "--kind", "mentions"])
            .contains("mentions")
    );
}

#[test]
fn files_are_attached_and_listed() {
    let sb = sandbox();
    let file = sb.path("plan.pdf");
    fs::write(&file, b"%PDF-1.7 not really").unwrap();
    let out = sb.ok(&["attach", "App", file.to_str().unwrap()]);
    assert!(
        out.starts_with("Attached plan.pdf (application/pdf, 19 bytes) to \"App\"."),
        "{out}"
    );
    let out = sb.ok(&["attachments", "App"]);
    assert!(
        out.contains("plan.pdf  (application/pdf, 19 bytes,"),
        "{out}"
    );
    let out = sb.ok(&["attachments", "App", "--json"]);
    let list: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(list[0]["name"], "plan.pdf");
    assert_eq!(list[0]["created_by"]["actor"], "tester");
    let out = sb.ok(&["attachments", "Database"]);
    assert_eq!(out.trim(), "\"Database\" has no attachments.");
    assert!(
        sb.fails(&["attach", "App", "missing.png"])
            .contains("is not a file")
    );
}

#[test]
fn data_exports_are_imported_once() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let export = ingest_fixture("exports/claude");
    let out = sb.ok(&["import", export.to_str().unwrap()]);
    assert!(out.starts_with("Imported "), "{out}");
    assert!(out.contains("from the Claude export at"), "{out}");
    assert!(!out.starts_with("Imported 0 new"), "{out}");
    let again = sb.ok(&["import", export.to_str().unwrap()]);
    assert!(again.starts_with("Imported 0 new conversations"), "{again}");
    assert!(again.contains(": 0 updated,"), "{again}");

    let out = sb.ok(&["search", "--json", "the"]);
    let hits: Value = serde_json::from_str(&out).unwrap();
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|h| h["node"]["kind"] == "conversation"),
        "{out}"
    );

    // Neither an export nor Markdown: a clear message.
    let zip = sb.path("photos.zip");
    fs::write(&zip, b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    let err = sb.fails(&["import", zip.to_str().unwrap()]);
    assert!(
        err.contains("is not a data export, a Markdown file or a session"),
        "{err}"
    );
}

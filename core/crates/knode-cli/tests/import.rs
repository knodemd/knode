//! `knode import` of every source, told from the path or named with `--from`, from the
//! synthetic files the knode-service tests use. No network: a URL is served from this machine.

mod common;
#[path = "../../knode-service/tests/fixtures/imports.rs"]
mod imports;

use std::fs;
use std::path::Path;

use common::Sandbox;
use imports as fx;
use serde_json::Value;

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn imports_every_source_told_from_its_path() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let files = sb.path("files");
    fs::create_dir_all(&files).unwrap();

    let cases = [
        (fx::ingest("exports/claude"), "Claude export"),
        (fx::pdf(&files), "PDF"),
        (fx::saved_page(&files), "saved web page"),
        (fx::clippings(&files), "Kindle clippings"),
        (fx::notebook(&files), "Kindle notebook"),
        (fx::readwise_csv(&files), "Readwise CSV export"),
        (fx::readwise_markdown(&files), "Readwise Markdown export"),
        (fx::apple_notes(&files), "Apple Notes"),
        (fx::bear_backup(&files), "Bear"),
        (fx::notion_zip(&files), "Notion export"),
        (fx::enex(&files), "Evernote export"),
    ];
    for (path, format) in &cases {
        let out = sb.ok(&["import", s(path)]);
        assert!(
            out.starts_with(&format!("Imported {} ({format}): ", path.display())),
            "{out}"
        );
    }

    let out = sb.ok(&["import", s(&fx::pdf(&files))]);
    assert!(out.contains(": 0 new, 0 updated, 1 unchanged."), "{out}");
    assert!(out.contains("\n  unchanged: On Rivers"), "{out}");
    let out = sb.ok(&["import", s(&fx::clippings(&files))]);
    assert!(out.contains("0 new"), "{out}");
    let out = sb.ok(&["import", s(&fx::enex(&files))]);
    assert!(out.contains(": 0 new, 0 updated, 2 unchanged."), "{out}");

    // Each kind of node, by the person.
    let space = knode_format::Space::open(sb.space("work")).unwrap();
    let walden = space.find_by_title("Walden").unwrap();
    assert_eq!(walden.len(), 1, "one Walden from Kindle and Readwise");
    let node = space.get_node(&walden[0].id).unwrap().unwrap();
    assert_eq!(node.kind, "source");
    assert_eq!(node.created_by.actor, "tester");
    assert_eq!(space.find_by_title("Garden plan").unwrap().len(), 2);
}

#[test]
fn reading_lists_each_document() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let files = sb.path("files");
    let out = sb.ok(&["import", s(&fx::clippings(&files))]);
    assert!(
        out.contains(": 2 new, 0 updated, 0 unchanged, 2 highlights added."),
        "{out}"
    );
    assert!(out.contains("\n  new: Walden (1 highlight added)"), "{out}");
    assert!(
        out.contains("\n  new: On Liberty (1 highlight added)"),
        "{out}"
    );
    let out = sb.ok(&["import", s(&fx::notebook(&files))]);
    assert!(
        out.contains("\n  updated: Walden (1 highlight added)"),
        "{out}"
    );
}

#[test]
fn from_names_the_source() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let export = sb.path("Notes export");
    fs::create_dir_all(export.join("Work")).unwrap();
    fs::write(export.join("Work/Plan.md"), "# Plan\n\nWater the beds.\n").unwrap();

    // A folder of Markdown is Markdown, unless it is named Apple Notes' export.
    let out = sb.ok(&["import", s(&export), "--from", "apple-notes", "--json"]);
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["source"], "apple-notes");
    assert_eq!(report["created"], 1);
    assert_eq!(report["nodes"][0]["title"], "Plan");
    assert_eq!(report["nodes"][0]["outcome"], "created");
    assert!(report["nodes"][0]["id"].is_string());

    let clippings = sb.path("kindle.txt");
    fs::write(
        &clippings,
        "Walden (Henry David Thoreau)\n- Your Highlight on page 3 | Location 40 | Added on Monday, January 5, 2026 9:15:02 PM\n\nSimplify.\n",
    )
    .unwrap();
    let out = sb.ok(&["import", s(&clippings), "--from", "kindle"]);
    assert!(out.contains("(Kindle clippings): 1 new"), "{out}");

    // A kind that does not fit, or does not exist.
    let err = sb.fails(&["import", s(&export), "--from", "pdf"]);
    assert!(err.contains("is not a PDF file"), "{err}");
    let err = sb.fails(&["import", s(&export), "--from", "onenote"]);
    assert!(err.contains("invalid value 'onenote'"), "{err}");
    assert!(err.contains("apple-notes"), "{err}");

    // Nothing to read.
    let err = sb.fails(&["import"]);
    assert!(err.contains("required"), "{err}");
}

#[test]
fn apple_notes_and_bear_are_read_from_this_mac() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let containers = sb.home().join("Library/Group Containers");
    if cfg!(target_os = "macos") {
        // No Bear here: what is missing is named.
        let err = sb.fails(&["import", "--from", "bear"]);
        assert!(err.contains("does not exist"), "{err}");

        fx::apple_notes(&containers);
        let out = sb.ok(&["import", "--from", "apple-notes"]);
        assert!(out.contains("(Apple Notes): 1 new"), "{out}");
    } else {
        let err = sb.fails(&["import", "--from", "apple-notes"]);
        assert!(err.contains("on a Mac only"), "{err}");
    }
    let err = sb.fails(&["import", "--from", "notion"]);
    assert!(err.contains("name what to import"), "{err}");
}

#[cfg(unix)]
#[test]
fn apple_notes_without_access_says_how_to_give_it() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let container = fx::apple_notes(&sb.path("files"));
    fs::set_permissions(&container, fs::Permissions::from_mode(0o000)).unwrap();
    let out = sb.run(&["import", s(&container), "--from", "apple-notes"]);
    fs::set_permissions(&container, fs::Permissions::from_mode(0o755)).unwrap();
    if out.status.success() {
        // Running as root: permissions do not apply.
        return;
    }
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("Full Disk Access"), "{err}");
    assert!(!err.contains("(code "), "{err}");
}

#[test]
fn a_url_given_is_fetched() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let (url, server) = fx::serve_once(fx::ARTICLE);
    let out = sb.ok(&["import", &url, "--json"]);
    assert_eq!(server.join().unwrap(), "GET /essays/on-rivers HTTP/1.1");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["source"], "web");
    assert_eq!(report["format"], "web page");
    assert_eq!(report["from"], url.as_str());
    assert_eq!(report["created"], 1);

    let err = sb.fails(&["import", "ftp://example.com/page"]);
    assert!(err.contains("does not exist"), "{err}");
    let err = sb.fails(&["import", "https://example.com/", "--from", "pdf"]);
    assert!(err.contains("a URL is a web page"), "{err}");
}

#[cfg(unix)]
#[test]
fn imports_directly_when_the_service_cannot_run() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["service", "stop"]);
    let enex = fx::enex(&sb.path("files"));
    let home = sb.knode_home();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o500)).unwrap();
    let out = sb.run(&["import", s(&enex)]);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("warning: Knode Service cannot run"),
        "{stderr}"
    );
    assert!(stdout.contains("(Evernote export): 2 new"), "{stdout}");
}

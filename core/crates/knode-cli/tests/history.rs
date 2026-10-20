//! `knode history`, `undo`, `restore`, `export`, `import <folder>` and `markdown folder`, through
//! the service the CLI starts and with the space opened directly.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use common::Sandbox;
use knode_format::{NewNode, NodeUpdate, Provenance, Space};
use serde_json::Value;

/// The number after `word` in `text`, such as the change in "change 12".
fn number_after(text: &str, word: &str) -> String {
    let start = text
        .find(word)
        .unwrap_or_else(|| panic!("no {word:?} in {text}"))
        + word.len();
    text[start..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect()
}

fn line_with<'a>(text: &'a str, part: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(part))
        .unwrap_or_else(|| panic!("no line with {part:?} in {text}"))
}

#[test]
fn history_undo_and_restore() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let space = sb.space("work");
    let notes = sb.path("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("Plan.md"), "One.\n").unwrap();
    sb.ok(&["import", notes.to_str().unwrap()]);
    fs::write(notes.join("Plan.md"), "Two.\n").unwrap();
    sb.ok(&["import", notes.to_str().unwrap()]);

    let out = sb.ok(&["history"]);
    assert!(
        out.contains("node.create  \"Plan\"  by tester (person)"),
        "{out}"
    );
    let out = sb.ok(&["history", "plan"]);
    assert!(out.starts_with("\"Plan\"  "), "{out}");
    let update = line_with(&out, "node.update");
    let change = number_after(update, "change");
    let first = number_after(line_with(&out, "node.create"), "version");
    assert!(out.contains("knode restore"), "{out}");

    let out = sb.ok(&["undo", &change]);
    assert!(
        out.starts_with(&format!(
            "Undid change {change} (node.update of \"Plan\"). The undo is change "
        )),
        "{out}"
    );
    let err = sb.fails(&["undo", &change]);
    assert!(err.contains("already undone"), "{err}");
    let err = sb.fails(&["undo", "999"]);
    assert!(err.contains("there is no change 999"), "{err}");

    // Undoing the create would overwrite the later changes: refused without --force.
    let out = sb.ok(&["history", "Plan", "--json"]);
    let json: Value = serde_json::from_str(&out).unwrap();
    let changes = json["changes"].as_array().unwrap();
    let create = changes.last().unwrap()["id"].to_string();
    let err = sb.fails(&["undo", &create]);
    assert!(err.contains("would overwrite later changes"), "{err}");
    assert!(
        err.contains(&format!("knode undo {create} --force")),
        "{err}"
    );

    let err = sb.fails(&["restore", "Plan", "999999"]);
    assert!(err.contains("has no version 999999"), "{err}");
    let err = sb.fails(&["restore", "Nothing", "1"]);
    assert!(err.contains("no node has the id or title"), "{err}");
    let out = sb.ok(&["restore", "Plan", &first]);
    assert_eq!(
        out.trim(),
        format!(
            "Restored \"Plan\" ({}) to version {first}.",
            id_of(&sb, "Plan")
        )
    );

    sb.ok(&["undo", &create, "--force"]);
    let out = sb.ok(&["history", &id_of_deleted(&space)]);
    assert!(out.contains("(deleted)"), "{out}");
    let deleted = number_after(line_with(&out, "undo"), "version");
    let out = sb.ok(&["restore", &id_of_deleted(&space), &deleted]);
    assert!(out.starts_with("Recovered \"Plan\""), "{out}");
}

fn id_of(sb: &Sandbox, title: &str) -> String {
    let space = Space::open(sb.space("work")).unwrap();
    space.find_by_title(title).unwrap()[0].id.to_string()
}

fn id_of_deleted(space: &std::path::Path) -> String {
    let space = Space::open(space).unwrap();
    space
        .changes(100, None)
        .unwrap()
        .last()
        .unwrap()
        .subject
        .clone()
}

#[test]
fn export_and_import_a_space() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    {
        let mut space = Space::open(sb.space("work")).unwrap();
        let agent = Provenance::agent("claude-code").with_session("s1");
        let plan = space
            .create_node(NewNode::note("Plan", "See [[Goal]]."), &agent)
            .unwrap();
        space
            .create_node(NewNode::note("Goal", "Ship.").with_tags(["v2"]), &agent)
            .unwrap();
        let update = NodeUpdate {
            body: Some("See [[Goal]] soon.".into()),
            ..Default::default()
        };
        space
            .update_node(&plan.id, update, &Provenance::person("Ada"))
            .unwrap();
    }
    let out_dir = sb.path("out");
    let out = sb.ok(&["export", out_dir.to_str().unwrap()]);
    assert_eq!(
        out.trim(),
        format!(
            "Exported 2 notes to {}: 2 written, 0 unchanged, 0 removed.",
            out_dir.display()
        )
    );
    let out = sb.ok(&["export", out_dir.to_str().unwrap()]);
    assert!(out.contains("0 written, 2 unchanged"), "{out}");
    let err = sb.fails(&["export", sb.space("work").join("inside").to_str().unwrap()]);
    assert!(err.contains("inside the space"), "{err}");

    // Into another space: the same ids, dates and authors.
    sb.ok(&["init", "copy"]);
    let copy = sb.space("copy");
    let out = sb.ok(&[
        "import",
        out_dir.to_str().unwrap(),
        "--space",
        copy.to_str().unwrap(),
    ]);
    assert!(out.contains(": 2 new, 0 updated, 0 unchanged."), "{out}");
    let original = Space::open(sb.space("work")).unwrap();
    let copied = Space::open(&copy).unwrap();
    for summary in original.recent(10).unwrap() {
        let a = original.get_node(&summary.id).unwrap().unwrap();
        let b = copied.get_node(&summary.id).unwrap().unwrap();
        assert_eq!(a, b);
    }
}

fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_live_markdown_folder() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let out = sb.ok(&["markdown", "folder"]);
    assert!(out.contains("is off"), "{out}");

    let dir = sb.path("Live");
    let out = sb.ok(&["markdown", "folder", dir.to_str().unwrap()]);
    assert!(
        out.starts_with(&format!(
            "The live Markdown folder is now {}.",
            dir.display()
        )),
        "{out}"
    );
    let notes = sb.path("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("Idea.md"), "Live.\n").unwrap();
    sb.ok(&["import", notes.to_str().unwrap()]);
    wait_for("Idea.md", || dir.join("Idea.md").exists());
    let out = sb.ok(&["markdown", "folder"]);
    assert_eq!(
        out.trim(),
        format!("The live Markdown folder is {}.", dir.display())
    );

    let cloud = sb.home().join("Dropbox/Notes");
    let out = sb.run(&["markdown", "folder", cloud.to_str().unwrap()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning:") && err.contains("Dropbox"), "{err}");

    let out = sb.ok(&["markdown", "folder", "--off"]);
    assert!(out.contains("is off. Its files stay as they are."), "{out}");
    let err = sb.fails(&["markdown", "folder", dir.to_str().unwrap(), "--off"]);
    assert!(err.contains("cannot be used with"), "{err}");

    // Recorded in the space, relative to the home folder.
    let space = Space::open(sb.space("work")).unwrap();
    let change = &space.changes(5, None).unwrap()[0];
    assert_eq!(change.subject, "markdown.folder");
    let set = space
        .changes(10, None)
        .unwrap()
        .into_iter()
        .find(|c| c.detail["value"] == "~/Dropbox/Notes");
    assert!(set.is_some());
}

#[test]
fn without_the_service_the_commands_open_the_space_directly() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["service", "stop"]);
    let home = sb.knode_home();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o500)).unwrap();

    let notes = sb.path("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("Direct.md"), "Here.\n").unwrap();
    let out = sb.run(&["import", notes.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{err}");
    assert!(err.contains("warning: Knode Service cannot run"), "{err}");
    let out = sb.ok(&["history"]);
    assert!(out.contains("\"Direct\""), "{out}");
    let change = number_after(&out, "change");
    sb.ok(&["undo", &change]);
    sb.ok(&["export", sb.path("out").to_str().unwrap()]);
    let out = sb.ok(&["markdown", "folder", sb.path("Live").to_str().unwrap()]);
    assert!(out.contains("knode service start"), "{out}");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();

    let space = Space::open(sb.space("work")).unwrap();
    assert_eq!(space.node_count().unwrap(), 0, "the import was undone");
}

//! `knode projects`, `knode projects move`, `knode undo` of a move and `knode unlink`, through
//! the service the CLI starts.

#![cfg(unix)]

mod common;

use std::fs;

use common::Sandbox;
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

/// Personal, the default space, with the project folder `acme`, the note Plan part of it and the
/// note Brand, which stays; and an empty Team space.
fn sandbox() -> Sandbox {
    let sb = Sandbox::new();
    sb.ok(&["init", "Personal"]);
    sb.ok(&["init", "Team"]);
    let folder = sb.home().join("work/acme");
    fs::create_dir_all(&folder).unwrap();
    sb.ok(&["folders", "add", folder.to_str().unwrap()]);
    for (title, body) in [
        ("Plan", "Ship it, in the colours of [[Brand]]."),
        ("Brand", "Blue."),
    ] {
        let file = sb.path(&format!("{title}.md"));
        fs::write(&file, format!("# {title}\n\n{body}\n")).unwrap();
        sb.ok(&["import", file.to_str().unwrap()]);
    }
    sb.ok(&["link", "Plan", "acme", "--kind", "part-of"]);
    sb
}

#[test]
fn a_project_moves_to_another_space_and_the_move_is_undone() {
    let sb = sandbox();
    let out = sb.ok(&["projects"]);
    assert!(out.contains("acme  (1 notes)"), "{out}");

    let out = sb.ok(&["projects", "move", "acme", "--to", "Team", "--dry-run"]);
    assert!(
        out.contains("Would move \"acme\" from Personal to Team"),
        "{out}"
    );
    assert!(out.contains("1 project (acme), 1 note"), "{out}");
    assert!(out.contains("\"Plan\" mentions \"Brand\""), "{out}");
    assert!(out.contains("Project folder ~/work/acme"), "{out}");
    let out = sb.ok(&["projects", "list", "--space", "Team", "--json"]);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        Value::Array(vec![])
    );

    let out = sb.ok(&["projects", "move", "acme", "--to", "Team"]);
    assert!(
        out.contains("Moved \"acme\" from Personal to Team"),
        "{out}"
    );
    let change = number_after(&out, "It is change");
    let out = sb.ok(&["projects", "--space", "Team"]);
    assert!(out.contains("acme  (1 notes)"), "{out}");
    let out = sb.ok(&["folders", "list", "--space", "Team"]);
    assert!(out.contains("~/work/acme"), "{out}");
    assert!(sb.ok(&["projects"]).contains("no projects"));
    // Its history came along: the move, with the version it had.
    let out = sb.ok(&["history", "Plan", "--space", "Team"]);
    assert!(out.contains("project.move"), "{out}");
    assert!(out.contains("by tester (person)"), "{out}");

    // Undone from the space it left: back there, gone from Team.
    let out = sb.ok(&["undo", &change]);
    assert!(out.contains("Undid the move of 2 nodes"), "{out}");
    assert!(sb.ok(&["projects"]).contains("acme  (1 notes)"));
    assert!(
        sb.ok(&["projects", "--space", "Team"])
            .contains("no projects")
    );
    let err = sb.fails(&["undo", &change]);
    assert!(err.contains("already undone"), "{err}");

    // An agent's grant does not let it move anything: there is no tool for it, and the service
    // refuses agents (tested in knode-service).
}

#[test]
fn a_project_is_reparented_with_unlink_and_link() {
    let sb = sandbox();
    let folder = sb.home().join("work/org");
    fs::create_dir_all(&folder).unwrap();
    sb.ok(&["folders", "add", folder.to_str().unwrap()]);
    sb.ok(&["link", "acme", "org", "--kind", "part-of"]);
    let out = sb.ok(&["projects"]);
    assert!(out.contains("org  (0 notes)"), "{out}");
    assert!(out.contains("\n  acme  (1 notes)"), "{out}");

    let out = sb.ok(&["unlink", "acme", "org", "--kind", "part-of"]);
    assert_eq!(
        out.trim(),
        "Removed the part-of link from \"acme\" to \"org\"."
    );
    let out = sb.ok(&["unlink", "acme", "org", "--kind", "part-of"]);
    assert!(out.contains("There is no part-of link"), "{out}");
    let out = sb.ok(&["projects"]);
    assert!(out.starts_with("acme  (1 notes)"), "{out}");
    let err = sb.fails(&["unlink", "Plan", "Brand", "--kind", "mentions"]);
    assert!(err.contains("mentions"), "{err}");
}

#[test]
fn a_move_to_an_unknown_project_or_the_same_space_fails() {
    let sb = sandbox();
    let err = sb.fails(&["projects", "move", "nowhere", "--to", "Team"]);
    assert!(err.contains("no project \"nowhere\""), "{err}");
    let err = sb.fails(&["projects", "move", "acme", "--to", "Personal"]);
    assert!(err.contains("already in that space"), "{err}");
}

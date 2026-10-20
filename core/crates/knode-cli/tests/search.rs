//! `knode search` with its modes, and `knode index`, through Knode Service. Search by meaning
//! uses a tiny model written on the fly: nothing is downloaded.

mod common;

use std::fs;

use common::Sandbox;
use serde_json::Value;

/// A sandbox with a default space holding a few notes, imported from Markdown files.
fn sandbox_with_notes() -> Sandbox {
    let sb = Sandbox::new();
    sb.ok(&["init", "Work"]);
    let notes = sb.path("notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("Cat.md"), "# the cat\n\nthe cat sat\n").unwrap();
    fs::write(notes.join("Dog.md"), "a dog ran far away\n").unwrap();
    sb.ok(&["import", notes.to_str().unwrap()]);
    sb
}

/// Whether this build searches by meaning at all.
const SEMANTIC: bool = cfg!(feature = "semantic");

/// The note a search by words prints when meaning was asked for.
fn fallback_note() -> &'static str {
    if SEMANTIC {
        "note: Search by meaning is off"
    } else {
        "note: This build of Knode searches by words only"
    }
}

fn json(out: &str) -> Value {
    serde_json::from_str(out).unwrap()
}

#[test]
fn search_flags_pick_the_mode_and_fall_back_to_words() {
    let sb = sandbox_with_notes();
    // Search by meaning is off in the sandbox: every mode runs by words and says why.
    let out = sb.run(&["search", "cat"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.starts_with("1. Cat  (note, "), "{stdout}");
    assert!(stderr.contains(fallback_note()), "{stderr}");

    let found = json(&sb.ok(&["search", "cat", "--meaning", "--json"]));
    assert_eq!(found["mode"], "words");
    let found = json(&sb.ok(&["search", "cat", "--words", "--json"]));
    assert_eq!(found["mode"], "words");
    assert!(found.get("note").is_none(), "{found}");
    assert_eq!(found["hits"].as_array().unwrap().len(), 1);

    let err = sb.fails(&["search", "cat", "--words", "--meaning"]);
    assert!(err.contains("cannot be used with"), "{err}");
}

#[test]
fn index_off_turns_it_off_in_the_config() {
    let sb = sandbox_with_notes();
    let out = sb.ok(&["index", "off"]);
    assert!(out.starts_with("Search by meaning is off"), "{out}");
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(config.contains("embeddings = \"off\""), "{config}");
    let out = sb.ok(&["index", "status"]);
    let expected = if SEMANTIC {
        "Search by meaning: off"
    } else {
        "Search by meaning: not available"
    };
    assert!(out.contains(expected), "{out}");
}

#[cfg(feature = "semantic")]
#[test]
fn index_run_indexes_and_search_ranks_by_meaning() {
    let sb = sandbox_with_notes();
    sb.ok(&["service", "stop"]);
    let model = sb.knode_home().join("models/multilingual-e5-small");
    fs::create_dir_all(&model).unwrap();
    knode_embed::test_support::write_tiny_model(&model);
    let run = |args: &[&str]| {
        let out = sb
            .cmd()
            .env("KNODE_EMBEDDINGS", "on")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "knode {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    let out = run(&["index", "run"]);
    assert!(out.contains("Work: 2 notes, indexed"), "{out}");
    let out = run(&["index", "status"]);
    assert!(out.contains("Search by meaning: on"), "{out}");
    assert!(out.contains("Model: multilingual-e5-small, ready"), "{out}");
    let status = json(&run(&["index", "status", "--json"]));
    assert_eq!(status["spaces"][0]["waiting"], 0);

    let found = json(&run(&["search", "cat", "--json"]));
    assert_eq!(found["mode"], "hybrid");
    assert_eq!(found["hits"].as_array().unwrap().len(), 2);
    assert_eq!(found["hits"][0]["node"]["title"], "Cat");
    let found = json(&run(&["search", "away", "--meaning", "--json"]));
    assert_eq!(found["mode"], "meaning");
    assert!(found["hits"][0]["word_rank"].is_null());

    let out = run(&["service", "status"]);
    assert!(out.contains("Work: 2 notes, indexed"), "{out}");
}

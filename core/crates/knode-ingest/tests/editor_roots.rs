//! Where Cursor, Windsurf and Cline are looked for by default, on macOS and on Linux.
//!
//! The environment is changed here, so this file holds a single test: it runs in a process of
//! its own, with no other test reading the environment at the same time.

use std::path::{Path, PathBuf};

use knode_ingest::{Agent, default_root, default_roots, discover, read_session};
use serde_json::json;

fn set(variable: &str, value: Option<&Path>) {
    // SAFETY: this is the only test in this binary, so no other thread reads the environment.
    unsafe {
        match value {
            Some(value) => std::env::set_var(variable, value),
            None => std::env::remove_var(variable),
        }
    }
}

#[test]
fn finds_editor_agents_in_their_default_folders() {
    let home = PathBuf::from("/home/example");
    for variable in [
        "KNODE_CURSOR_ROOT",
        "KNODE_WINDSURF_ROOT",
        "KNODE_CLINE_ROOT",
        "XDG_CONFIG_HOME",
        "CLINE_DATA_DIR",
        "CLINE_DIR",
    ] {
        set(variable, None);
    }
    set("HOME", Some(&home));

    let editors = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        home.join(".config")
    };
    assert_eq!(
        default_root(Agent::Cursor),
        Some(editors.join("Cursor/User/globalStorage"))
    );
    assert_eq!(
        default_root(Agent::Windsurf),
        Some(editors.join("Windsurf/User/workspaceStorage"))
    );
    let cline = default_roots(Agent::Cline);
    let extension = "User/globalStorage/saoudrizwan.claude-dev/tasks";
    assert_eq!(
        cline,
        [
            editors.join("Code").join(extension),
            editors.join("Code - Insiders").join(extension),
            editors.join("VSCodium").join(extension),
            editors.join("Cursor").join(extension),
            editors.join("Windsurf").join(extension),
            home.join(".cline/data/tasks"),
            home.join(".cline/data/sessions"),
        ]
    );

    if !cfg!(target_os = "macos") {
        set("XDG_CONFIG_HOME", Some(Path::new("/xdg/config")));
        assert_eq!(
            default_root(Agent::Cursor),
            Some(PathBuf::from("/xdg/config/Cursor/User/globalStorage"))
        );
        set("XDG_CONFIG_HOME", None);
    }

    // Cline's data folder follows the variables the Cline SDK reads.
    set("CLINE_DIR", Some(Path::new("/opt/cline")));
    assert_eq!(
        default_roots(Agent::Cline).last(),
        Some(&PathBuf::from("/opt/cline/data/sessions"))
    );
    set("CLINE_DATA_DIR", Some(Path::new("/srv/cline-data")));
    assert_eq!(
        default_roots(Agent::Cline).last(),
        Some(&PathBuf::from("/srv/cline-data/sessions"))
    );

    // One folder named for an agent replaces its defaults.
    set("KNODE_CLINE_ROOT", Some(Path::new("/elsewhere/tasks")));
    assert_eq!(
        default_roots(Agent::Cline),
        [PathBuf::from("/elsewhere/tasks")]
    );
    set("KNODE_CURSOR_ROOT", Some(Path::new("/elsewhere/cursor")));
    assert_eq!(
        default_root(Agent::Cursor),
        Some(PathBuf::from("/elsewhere/cursor"))
    );

    set("KNODE_CLINE_ROOT", None);
    set("KNODE_CURSOR_ROOT", None);

    // A classic task in an editor's extension folder, resumed into an SDK session in Cline's
    // data folder: reading either finds the other, through the default folders.
    let dir = tempfile::TempDir::new().unwrap();
    let home = dir.path().join("home");
    set("HOME", Some(&home));
    set("CLINE_DIR", None);
    set("CLINE_DATA_DIR", Some(&dir.path().join("cline-data")));
    let roots = default_roots(Agent::Cline);
    let id = "1767348000000";
    let task = roots[0].join(id);
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(
        task.join("ui_messages.json"),
        json!([
            {"ts": 1_767_348_000_000_i64, "type": "say", "say": "task", "text": "Add a toggle"},
            {"ts": 1_767_348_001_000_i64, "type": "say", "say": "completion_result", "text": "Added."}
        ])
        .to_string(),
    )
    .unwrap();
    let sessions = roots.last().unwrap().clone();
    let session = sessions.join(id);
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join(format!("{id}.messages.json")),
        json!({"version": 1, "agent": "lead", "sessionId": id, "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Add a toggle"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "Added."}]},
            {"role": "user", "content": [{"type": "text", "text": "<user_input mode=\"act\">And a test</user_input>"}]},
            {"role": "assistant", "ts": 1_767_348_100_000_i64, "content": [{"type": "text", "text": "Test added."}]}
        ]})
        .to_string(),
    )
    .unwrap();
    let classic = discover(Agent::Cline, &roots[0]).unwrap().remove(0);
    let sdk = discover(Agent::Cline, &sessions).unwrap().remove(0);
    let from_classic = read_session(&classic).unwrap();
    assert_eq!(from_classic, read_session(&sdk).unwrap());
    let texts: Vec<String> = from_classic.messages.iter().map(|m| m.text()).collect();
    assert_eq!(
        texts,
        ["Add a toggle", "Added.", "And a test", "Test added."]
    );
}

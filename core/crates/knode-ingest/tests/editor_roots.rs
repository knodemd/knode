//! Where Cursor, Windsurf and Cline are looked for by default, on macOS and on Linux.
//!
//! The environment is changed here, so this file holds a single test: it runs in a process of
//! its own, with no other test reading the environment at the same time.

use std::path::{Path, PathBuf};

use knode_ingest::{Agent, default_root, default_roots};

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
}

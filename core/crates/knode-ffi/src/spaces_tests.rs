use std::sync::Arc;

use super::*;
use crate::{ActorKind, KnodeSpace, Provenance};

fn me() -> Provenance {
    Provenance {
        kind: ActorKind::Person,
        actor: "Ada".into(),
        session: None,
        source: None,
    }
}

fn agent() -> Provenance {
    Provenance {
        kind: ActorKind::Agent,
        actor: "claude-code".into(),
        session: None,
        source: None,
    }
}

fn new_space(dir: &tempfile::TempDir) -> Arc<KnodeSpace> {
    let path = dir.path().join("Test.knode").display().to_string();
    KnodeSpace::create(path, "Test".into()).unwrap()
}

fn draft(kind: &str, path: &Path, rules: &str) -> FolderDraft {
    FolderDraft {
        id: None,
        kind: kind.into(),
        path: path.display().to_string(),
        capture: true,
        rules: rules.into(),
    }
}

#[test]
fn renames_and_changes_settings_with_the_rules_of_the_service() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    assert_eq!(
        space.rename_as("  Work  ".into(), me()).unwrap().name,
        "Work"
    );
    assert!(matches!(
        space.rename_as("Work".into(), agent()),
        Err(KnodeError::PersonOnly { .. })
    ));

    assert!(space.capture_enabled().unwrap());
    assert!(
        space
            .change_setting("capture.enabled".into(), Some("off".into()), me())
            .unwrap()
    );
    assert!(!space.capture_enabled().unwrap());
    // `on` is the default, stored as no value.
    space
        .change_setting("capture.enabled".into(), Some("on".into()), me())
        .unwrap();
    assert!(space.capture_enabled().unwrap());
    assert_eq!(space.setting("capture.enabled".into()).unwrap(), None);

    space
        .change_setting("space.icon".into(), Some("🌱".into()), me())
        .unwrap();
    assert_eq!(
        space.setting("space.icon".into()).unwrap().as_deref(),
        Some("🌱")
    );
    assert!(matches!(
        space.change_setting("space.icon".into(), Some("work".into()), me()),
        Err(KnodeError::Invalid { .. })
    ));
    assert!(matches!(
        space.change_setting("space.id".into(), Some("x".into()), me()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn adds_changes_and_removes_folders() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let project = dir.path().join("site");
    std::fs::create_dir_all(&project).unwrap();

    let added = space
        .set_folder(
            draft("project", &project, r#"{"exclude":["node_modules/**"]}"#),
            me(),
        )
        .unwrap();
    assert_eq!(added.kind, "project");
    assert!(added.capture && !added.missing);
    assert_eq!(
        added.local.as_deref(),
        Some(project.display().to_string().as_str())
    );

    // Turning capture off keeps the rules that are passed back.
    let mut off = draft("project", &project, &added.rules);
    off.id = Some(added.id.clone());
    off.capture = false;
    let changed = space.set_folder(off, me()).unwrap();
    assert_eq!(changed.id, added.id);
    assert!(!changed.capture);
    assert!(changed.rules.contains("node_modules"));

    let sessions = dir.path().join("gone");
    let agent_folder = space
        .set_folder(draft("agent", &sessions, r#"{"agent":"codex"}"#), me())
        .unwrap();
    assert!(agent_folder.missing);
    assert!(matches!(
        space.set_folder(draft("agent", &sessions, "{}"), me()),
        Err(KnodeError::Invalid { .. })
    ));
    assert!(matches!(
        space.set_folder(draft("project", Path::new("relative"), "{}"), me()),
        Err(KnodeError::Invalid { .. })
    ));
    assert!(matches!(
        space.set_folder(draft("project", &project, "{}"), agent()),
        Err(KnodeError::PersonOnly { .. })
    ));
    assert_eq!(space.folders().unwrap().len(), 2);

    assert!(space.remove_folder(added.id.clone(), me()).unwrap());
    assert!(!space.remove_folder(added.id, me()).unwrap());
    assert_eq!(space.folders().unwrap(), [agent_folder]);
}

#[test]
fn lists_the_agents_capture_reads() {
    let agents = capture_agents();
    assert_eq!(agents[0].id, "claude-code");
    assert!(agents.iter().any(|a| a.id == "codex" && a.name == "Codex"));
}

#[test]
fn remembers_and_forgets_spaces_in_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let paths = knode_config::Paths {
        home: dir.path().join("home"),
        spaces_dir: dir.path().join("Knode"),
    };
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(paths.config_file(), "capture = false\n").unwrap();
    let work = paths.space_path("Work").display().to_string();
    let home = paths.space_path("Home").display().to_string();

    assert!(remember_space_in(&paths, &work).unwrap());
    assert!(!remember_space_in(&paths, &work).unwrap());
    crate::set_default_space_in(&paths, Some(home.clone())).unwrap();
    assert_eq!(
        known_spaces_in(&paths).unwrap(),
        [home.clone(), work.clone()]
    );

    // Another default keeps the previous one known, and so does leaving none.
    crate::set_default_space_in(&paths, Some(work.clone())).unwrap();
    assert_eq!(
        known_spaces_in(&paths).unwrap(),
        [work.clone(), home.clone()]
    );
    crate::set_default_space_in(&paths, None).unwrap();
    assert_eq!(
        known_spaces_in(&paths).unwrap(),
        [home.clone(), work.clone()]
    );

    crate::set_default_space_in(&paths, Some(work.clone())).unwrap();
    assert!(forget_space_in(&paths, &work).unwrap());
    assert_eq!(crate::default_space_in(&paths).unwrap(), None);
    assert!(forget_space_in(&paths, &home).unwrap());
    assert!(!forget_space_in(&paths, &home).unwrap());
    assert!(known_spaces_in(&paths).unwrap().is_empty());
    assert!(matches!(
        remember_space_in(&paths, "Work.knode"),
        Err(KnodeError::Invalid { .. })
    ));
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("capture = false"), "{text}");
}

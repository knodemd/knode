use super::*;

fn me() -> Provenance {
    Provenance {
        kind: ActorKind::Person,
        actor: "Ada".into(),
        session: None,
        source: None,
    }
}

fn new_space(dir: &tempfile::TempDir) -> Arc<KnodeSpace> {
    let path = dir.path().join("Test.knode").display().to_string();
    KnodeSpace::create(path, "Test".into()).unwrap()
}

#[test]
fn has_a_version() {
    assert!(!version().is_empty());
}

#[test]
fn creates_and_reopens_a_space() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Test.knode").display().to_string();
    let space = KnodeSpace::create(path.clone(), "Test".into()).unwrap();
    let info = space.info().unwrap();
    assert_eq!(info.name, "Test");
    assert_eq!(info.path, path);
    assert_eq!(info.id.len(), 26);
    let note = space
        .create_node("Roadmap".into(), "Ship.".into(), me())
        .unwrap();
    drop(space);

    let space = KnodeSpace::open(path.clone()).unwrap();
    assert_eq!(space.info().unwrap().id, info.id);
    let node = space.get_node(note.id.clone()).unwrap().unwrap();
    assert_eq!(node, note);
    assert_eq!(node.created_by, me());

    assert!(matches!(
        KnodeSpace::create(path, "Again".into()),
        Err(KnodeError::AlreadyExists { .. })
    ));
}

#[test]
fn reports_what_is_not_a_space() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("Missing.knode").display().to_string();
    assert!(matches!(
        KnodeSpace::open(missing),
        Err(KnodeError::NotASpace { .. })
    ));
    let wrong = dir.path().join("Notes").display().to_string();
    assert!(matches!(
        KnodeSpace::create(wrong, "Notes".into()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn lists_recent_nodes_newest_first_with_excerpts() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let first = space
        .create_node("First".into(), "# Heading\n\n- the first line".into(), me())
        .unwrap();
    let second = space.create_node("Second".into(), "".into(), me()).unwrap();
    let third = space
        .create_node("Third".into(), "Text".into(), me())
        .unwrap();

    let ids = |list: Vec<RecentNode>| list.into_iter().map(|r| r.node.id).collect::<Vec<_>>();
    assert_eq!(
        ids(space.recent(10).unwrap()),
        [third.id.clone(), second.id.clone(), first.id.clone()]
    );
    assert_eq!(
        ids(space.recent(2).unwrap()),
        [third.id.clone(), second.id.clone()]
    );

    std::thread::sleep(std::time::Duration::from_millis(2));
    space
        .update_node(first.id.clone(), None, Some("Changed".into()), me())
        .unwrap();
    let recent = space.recent(10).unwrap();
    assert_eq!(recent[0].node.id, first.id);
    assert_eq!(recent[0].excerpt, "Changed");
    assert_eq!(recent[1].excerpt, "Text");
    assert_eq!(recent[2].excerpt, "");
}

#[test]
fn updates_and_deletes_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let note = space
        .create_node("Old".into(), "Body".into(), me())
        .unwrap();

    let agent = Provenance {
        kind: ActorKind::Agent,
        actor: "claude-code".into(),
        session: Some("s1".into()),
        source: None,
    };
    let renamed = space
        .update_node(note.id.clone(), Some("New".into()), None, agent.clone())
        .unwrap();
    assert_eq!(renamed.title, "New");
    assert_eq!(renamed.body, "Body");
    assert_eq!(renamed.updated_by, agent);
    assert!(renamed.updated_at >= note.updated_at);

    space.delete_node(note.id.clone(), me()).unwrap();
    assert_eq!(space.get_node(note.id.clone()).unwrap(), None);
    assert!(matches!(
        space.update_node(note.id.clone(), Some("x".into()), None, me()),
        Err(KnodeError::NotFound { .. })
    ));
    assert!(matches!(
        space.get_node("not an id".into()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn searches_with_snippets() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    space
        .create_node("Garden".into(), "Plant tomatoes in May.".into(), me())
        .unwrap();
    space
        .create_node("Kitchen".into(), "Nothing here.".into(), me())
        .unwrap();

    let hits = space.search("tomato".into(), 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node.title, "Garden");
    assert!(
        hits[0].snippet.contains("**tomatoes**"),
        "{}",
        hits[0].snippet
    );
    assert!(space.search("".into(), 10).unwrap().is_empty());
}

#[test]
fn follows_links_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let roadmap = space
        .create_node("Roadmap".into(), "".into(), me())
        .unwrap();
    let plan = space
        .create_node("Plan".into(), "Follow the [[Roadmap]].".into(), me())
        .unwrap();

    let from = space.links_from(plan.id.clone()).unwrap();
    assert_eq!(from.len(), 1);
    assert_eq!(from[0].to, roadmap.id);
    assert_eq!(from[0].to_title, "Roadmap");
    assert_eq!(from[0].kind, "mentions");

    let back = space.backlinks(roadmap.id.clone()).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].from, plan.id);
    assert_eq!(back[0].from_title, "Plan");
}

#[test]
fn can_be_shared_between_threads() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let space = Arc::clone(&space);
            std::thread::spawn(move || {
                space
                    .create_node(format!("Note {i}"), "".into(), me())
                    .unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(space.recent(10).unwrap().len(), 4);
}

#[test]
fn excerpts_skip_markers_and_fences() {
    assert_eq!(excerpt("```sh\n# Title\n> - quoted"), "Title");
    assert_eq!(excerpt("\n\n  \n"), "");
    assert_eq!(excerpt(&"a".repeat(200)).len(), 140);
    assert_eq!(excerpt("- [ ] Reminders"), "Reminders");
    assert_eq!(excerpt("- [x] Done"), "Done");
    assert_eq!(excerpt("**Ada:** Should we?"), "Ada: Should we?");
    assert_eq!(
        excerpt("| Region | Last frost |\n|---|---|"),
        "Region  Last frost"
    );
    assert_eq!(excerpt("|---|---|\nText"), "Text");
}

#[test]
fn renames_and_keeps_settings() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let info = space.rename("  Work  ".into()).unwrap();
    assert_eq!(info.name, "Work");
    assert!(matches!(
        space.rename(" ".into()),
        Err(KnodeError::Invalid { .. })
    ));

    assert_eq!(space.setting("space.icon".into()).unwrap(), None);
    assert!(
        space
            .set_setting("space.icon".into(), Some("🌱".into()), me())
            .unwrap()
    );
    assert!(
        !space
            .set_setting("space.icon".into(), Some("🌱".into()), me())
            .unwrap()
    );
    let agent = Provenance {
        kind: ActorKind::Agent,
        actor: "claude-code".into(),
        session: None,
        source: None,
    };
    assert!(matches!(
        space.set_setting("space.icon".into(), None, agent),
        Err(KnodeError::PersonOnly { .. })
    ));
    let path = space.info().unwrap().path;
    drop(space);

    let space = KnodeSpace::open(path).unwrap();
    assert_eq!(space.info().unwrap().name, "Work");
    assert_eq!(
        space.setting("space.icon".into()).unwrap().as_deref(),
        Some("🌱")
    );
    assert!(space.set_setting("space.icon".into(), None, me()).unwrap());
    assert_eq!(space.setting("space.icon".into()).unwrap(), None);
}

fn agent(session: &str) -> Provenance {
    Provenance {
        kind: ActorKind::Agent,
        actor: "claude-code".into(),
        session: Some(session.into()),
        source: None,
    }
}

#[test]
fn history_says_who_changed_what() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let note = space
        .create_node("Garden".into(), "Beds: tomato.".into(), me())
        .unwrap();
    space
        .update_node(
            note.id.clone(),
            None,
            Some("Beds: tomato, mint.".into()),
            agent("5f1c"),
        )
        .unwrap();

    let history = space.history(note.id.clone()).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].op, ChangeOp::Create);
    assert_eq!(history[0].changed_by, me());
    assert_eq!(history[1].op, ChangeOp::Update);
    assert_eq!(history[1].body, "Beds: tomato, mint.");
    assert_eq!(history[1].changed_by, agent("5f1c"));
    assert!(history[1].seq > history[0].seq);
    assert!(history[1].change_id.unwrap() > history[0].change_id.unwrap());
    assert_eq!(history[1].restored_from, None);

    let changes = space.changes_of(note.id.clone(), 10).unwrap();
    assert_eq!(
        changes
            .iter()
            .map(|c| c.action.as_str())
            .collect::<Vec<_>>(),
        ["node.update", "node.create"]
    );
    assert_eq!(changes[0].id, history[1].change_id.unwrap());
    assert!(matches!(
        space.history("nope".into()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn restores_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let note = space
        .create_node("Garden".into(), "One.".into(), me())
        .unwrap();
    space
        .update_node(note.id.clone(), None, Some("Two.".into()), agent("a"))
        .unwrap();
    let first = space.history(note.id.clone()).unwrap()[0].clone();

    let restored = space.restore(note.id.clone(), first.seq, me()).unwrap();
    assert_eq!(restored.body, "One.");
    let history = space.history(note.id.clone()).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(history[2].restored_from, Some(first.seq));
    assert_eq!(
        space.changes_of(note.id.clone(), 1).unwrap()[0].action,
        "node.restore"
    );

    // Restoring the version the node has writes nothing.
    space.restore(note.id.clone(), first.seq, me()).unwrap();
    assert_eq!(space.history(note.id.clone()).unwrap().len(), 3);
    assert!(matches!(
        space.restore(note.id, 9999, me()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn undoes_a_change_and_reports_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let note = space
        .create_node("Garden".into(), "One.".into(), me())
        .unwrap();
    space
        .update_node(note.id.clone(), None, Some("Two.".into()), agent("a"))
        .unwrap();
    space
        .update_node(note.id.clone(), None, Some("Three.".into()), me())
        .unwrap();
    let history = space.history(note.id.clone()).unwrap();
    let (second, third) = (history[1].change_id.unwrap(), history[2].change_id.unwrap());

    // Undoing the agent's change would overwrite the later one.
    match space.undo(second, me(), false) {
        Err(KnodeError::UndoConflict {
            change,
            later,
            message,
        }) => {
            assert_eq!(change, second);
            assert_eq!(later, [third]);
            assert!(message.contains(&third.to_string()), "{message}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        space.get_node(note.id.clone()).unwrap().unwrap().body,
        "Three."
    );

    // The last change undoes cleanly, once.
    let undo = space.undo(third, me(), false).unwrap();
    assert_eq!(undo.action, "undo");
    assert_eq!(undo.undoes, Some(third));
    assert_eq!(
        space.get_node(note.id.clone()).unwrap().unwrap().body,
        "Two."
    );
    assert!(matches!(
        space.undo(third, me(), false),
        Err(KnodeError::CannotUndo { .. })
    ));

    // Forced, the earlier change goes back past the later ones.
    space.undo(second, me(), true).unwrap();
    assert_eq!(
        space.get_node(note.id.clone()).unwrap().unwrap().body,
        "One."
    );
    assert!(matches!(
        space.undo(123_456, me(), false),
        Err(KnodeError::ChangeNotFound { id: 123_456 })
    ));
}

#[test]
fn reads_and_writes_the_default_space() {
    let dir = tempfile::tempdir().unwrap();
    let paths = knode_config::Paths {
        home: dir.path().join("home"),
        spaces_dir: dir.path().join("Knode"),
    };
    assert_eq!(default_space_in(&paths).unwrap(), None);
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(paths.config_file(), "capture = false\n").unwrap();

    let work = paths.space_path("Work").display().to_string();
    set_default_space_in(&paths, Some(work.clone())).unwrap();
    assert_eq!(default_space_in(&paths).unwrap(), Some(work.clone()));
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("capture = false"), "{text}");

    set_default_space_in(&paths, None).unwrap();
    assert_eq!(default_space_in(&paths).unwrap(), None);

    assert!(spaces_in_folder_of(&paths).unwrap().is_empty());
    KnodeSpace::create(work.clone(), "Work".into()).unwrap();
    assert_eq!(spaces_in_folder_of(&paths).unwrap(), [work]);
}

#[test]
fn turns_search_by_meaning_on_and_off() {
    let dir = tempfile::tempdir().unwrap();
    let paths = knode_config::Paths {
        home: dir.path().join("home"),
        spaces_dir: dir.path().join("Knode"),
    };
    assert!(embeddings_enabled_in(&paths).unwrap());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(paths.config_file(), "capture = false\n").unwrap();

    set_embeddings_enabled_in(&paths, false).unwrap();
    assert!(!embeddings_enabled_in(&paths).unwrap());
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("capture = false"), "{text}");
    assert!(text.contains(r#"embeddings = "off""#), "{text}");

    set_embeddings_enabled_in(&paths, true).unwrap();
    assert!(embeddings_enabled_in(&paths).unwrap());
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(!text.contains("embeddings"), "{text}");
}

#[test]
fn lists_and_keeps_notes_deleted_from_the_markdown_folder() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let note = space
        .create_node("Plan".into(), "Draft.".into(), me())
        .unwrap();
    assert!(space.markdown_deleted().unwrap().is_empty());

    let id = parse_id(&note.id).unwrap();
    space
        .lock()
        .set_property(
            &id,
            "markdown.deleted",
            "Plan.md",
            &format::Provenance::person("Ada"),
        )
        .unwrap();
    assert_eq!(
        space.markdown_deleted().unwrap(),
        vec![DeletedNote {
            id: note.id.clone(),
            title: "Plan".into(),
            path: "Plan.md".into(),
        }]
    );
    assert!(space.keep_markdown_deleted(note.id.clone()).unwrap());
    assert!(!space.keep_markdown_deleted(note.id.clone()).unwrap());
    assert!(space.markdown_deleted().unwrap().is_empty());
}

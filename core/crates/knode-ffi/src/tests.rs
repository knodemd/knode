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

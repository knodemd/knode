//! Moving a project, with its notes, links, history, attachments and folders, to another space,
//! and undoing the move.

use std::fs;

use knode_format::{
    ActorKind, ChangeOp, Error, FolderSpec, ImportedNode, NewNode, NodeId, NodeUpdate, Properties,
    Provenance, Space, actions, folder_kinds, kinds, link_kinds, property_keys,
};
use tempfile::TempDir;

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code").with_session("5f1c")
}

fn project(space: &mut Space, title: &str) -> NodeId {
    space
        .create_node(
            NewNode {
                kind: kinds::PROJECT.into(),
                title: title.into(),
                ..Default::default()
            },
            &ada(),
        )
        .unwrap()
        .id
}

fn note(space: &mut Space, title: &str, body: &str) -> NodeId {
    space
        .create_node(NewNode::note(title, body), &ada())
        .unwrap()
        .id
}

/// A personal space and a team space. In the personal one: `Life`, a project that stays; `Acme`,
/// part of it, with `App` inside; `Spec` part of Acme, edited by an agent, with a file; `Plan`
/// part of App, mentioning Spec and `Brand`, a note that stays; a related link from Brand to Spec;
/// and a project folder that holds Acme.
struct Setup {
    _dir: TempDir,
    personal: Space,
    team: Space,
    life: NodeId,
    acme: NodeId,
    app: NodeId,
    spec: NodeId,
    plan: NodeId,
    brand: NodeId,
    folder: String,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let mut personal = Space::create(dir.path().join("Personal.knode"), "Personal").unwrap();
    let team = Space::create(dir.path().join("Acme team.knode"), "Acme team").unwrap();
    let life = project(&mut personal, "Life");
    let acme = project(&mut personal, "Acme");
    let app = project(&mut personal, "App");
    personal
        .link(&acme, &life, link_kinds::PART_OF, &ada())
        .unwrap();
    personal
        .link(&app, &acme, link_kinds::PART_OF, &ada())
        .unwrap();
    let brand = note(&mut personal, "Brand", "Blue and bold.");
    let spec = note(&mut personal, "Spec", "First draft.");
    personal
        .update_node(
            &spec,
            NodeUpdate {
                body: Some("Second draft, by the agent. ![[diagram.svg]]".into()),
                tags: Some(vec!["draft".into()]),
                ..Default::default()
            },
            &claude(),
        )
        .unwrap();
    personal
        .link(&spec, &acme, link_kinds::PART_OF, &ada())
        .unwrap();
    personal
        .attach(&spec, "diagram.svg", &b"<svg/>"[..], &ada())
        .unwrap();
    let plan = note(
        &mut personal,
        "Plan",
        "Follow [[Spec]], in the colours of [[Brand]].",
    );
    personal
        .link(&plan, &app, link_kinds::PART_OF, &ada())
        .unwrap();
    personal
        .set_property(&plan, property_keys::MARKDOWN_PATH, "Acme/Plan.md", &ada())
        .unwrap();
    personal
        .link(&brand, &spec, link_kinds::RELATED, &ada())
        .unwrap();
    let added = personal
        .add_folder(
            &FolderSpec {
                id: None,
                kind: folder_kinds::PROJECT.into(),
                path: "~/work/acme".into(),
                capture: true,
                rules: serde_json::json!({}),
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(added.project.unwrap().node.id, acme);
    Setup {
        _dir: dir,
        personal,
        team,
        life,
        acme,
        app,
        spec,
        plan,
        brand,
        folder: added.folder.id,
    }
}

#[test]
fn a_preview_counts_what_moves_and_what_crosses() {
    let s = setup();
    let preview = s.personal.preview_move(&s.acme, &s.team).unwrap();
    let projects: Vec<NodeId> = preview.projects.iter().map(|p| p.id).collect();
    assert_eq!(projects, vec![s.acme, s.app]);
    assert_eq!(preview.notes, 2);
    assert_eq!(preview.attachments, 1);
    assert_eq!(preview.attachment_bytes, 6);
    // part-of App->Acme, Spec->Acme, Plan->App, and Plan mentions Spec.
    assert_eq!(preview.links_kept, 4);
    let mut crossing: Vec<(String, String, String)> = preview
        .crossing
        .iter()
        .map(|l| (l.from_title.clone(), l.kind.clone(), l.to_title.clone()))
        .collect();
    crossing.sort();
    assert_eq!(
        crossing,
        vec![
            ("Acme".into(), "part-of".into(), "Life".into()),
            ("Brand".into(), "related".into(), "Spec".into()),
            ("Plan".into(), "mentions".into(), "Brand".into()),
        ]
    );
    assert_eq!(preview.folders.len(), 1);
    assert!(preview.conflicts.is_empty());
    // Acme, App, Spec (twice) and Plan.
    assert_eq!(preview.versions, 5);
    // A preview writes nothing.
    assert!(s.team.get_node(&s.acme).unwrap().is_none());
}

#[test]
fn a_project_moves_with_its_notes_links_history_files_and_folders() {
    let mut s = setup();
    let history_before = s.personal.history(&s.spec).unwrap();
    let moved = s
        .personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();

    // Same ids, bodies, tags and properties in the team space.
    for id in [s.acme, s.app, s.spec, s.plan] {
        assert!(s.team.get_node(&id).unwrap().is_some(), "{id}");
        assert!(s.personal.get_node(&id).unwrap().is_none(), "{id}");
    }
    let spec = s.team.get_node(&s.spec).unwrap().unwrap();
    assert_eq!(spec.body, "Second draft, by the agent. ![[diagram.svg]]");
    assert_eq!(spec.tags, vec!["draft".to_string()]);
    assert_eq!(spec.updated_by.actor, "claude-code");
    assert_eq!(
        s.team
            .property(&s.plan, property_keys::MARKDOWN_PATH)
            .unwrap()
            .as_deref(),
        Some("Acme/Plan.md")
    );

    // Every version, with who made it and when.
    let history = s.team.history(&s.spec).unwrap();
    assert_eq!(history.len(), history_before.len());
    for (after, before) in history.iter().zip(&history_before) {
        assert_eq!(
            (&after.body, after.changed_at, &after.changed_by),
            (&before.body, before.changed_at, &before.changed_by)
        );
        assert_eq!(after.change_id, Some(moved.to_change));
    }
    assert!(
        history
            .iter()
            .any(|v| v.changed_by.kind == ActorKind::Agent && v.op == ChangeOp::Update)
    );

    // Links among the moved nodes are kept; the ones to Life and Brand are not.
    let parents = |space: &Space, id: &NodeId| -> Vec<NodeId> {
        space
            .links_from(id)
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == link_kinds::PART_OF)
            .map(|l| l.to)
            .collect()
    };
    assert_eq!(parents(&s.team, &s.app), vec![s.acme]);
    assert_eq!(parents(&s.team, &s.plan), vec![s.app]);
    assert!(parents(&s.team, &s.acme).is_empty());
    assert!(
        s.team
            .links_from(&s.plan)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::MENTIONS && l.to == s.spec)
    );
    // [[Brand]] keeps its text and waits for a Brand in the team space.
    assert_eq!(s.team.unresolved_links(&s.plan).unwrap(), vec!["Brand"]);
    assert!(s.personal.backlinks(&s.brand).unwrap().is_empty());
    let projects = s.team.projects().unwrap();
    assert_eq!(projects.len(), 2);

    // The file came along.
    let attachment = s.team.attachment(&s.spec, "diagram.svg").unwrap().unwrap();
    assert_eq!(
        s.team.read_blob(&attachment.hash).unwrap().unwrap(),
        b"<svg/>"
    );

    // The folder moved, and holds Acme there.
    assert!(s.personal.folder(&s.folder).unwrap().is_none());
    let folders = s.team.folders_with_projects().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].0.path, "~/work/acme");
    assert_eq!(folders[0].1.as_ref().unwrap().id, s.acme);

    // One change in each space; the one it left points at the other.
    let out = s
        .personal
        .change(moved.from_change)
        .unwrap()
        .unwrap()
        .change;
    assert_eq!(out.action, actions::PROJECT_MOVE);
    assert_eq!(out.detail["change"], serde_json::json!(moved.to_change));
    assert_eq!(out.detail["to"]["name"], "Acme team");
    let into = s.team.change(moved.to_change).unwrap().unwrap().change;
    assert_eq!(into.action, actions::PROJECT_MOVE);
    assert_eq!(into.changed_by.actor, "Ada");

    // Moved nodes are not restored or undone one space at a time.
    let deleted = s.personal.history(&s.spec).unwrap().pop().unwrap();
    assert_eq!(deleted.op, ChangeOp::Delete);
    assert!(matches!(
        s.personal.restore(&s.spec, deleted.seq, &ada()),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.personal.undo(moved.from_change, &ada(), false),
        Err(Error::CannotUndo { .. })
    ));
    assert!(matches!(
        s.team.undo(moved.to_change, &ada(), true),
        Err(Error::CannotUndo { .. })
    ));
}

#[test]
fn loro_documents_move_with_their_nodes() {
    let mut s = setup();
    let dir = s.personal.loro_dir(&s.spec).join("updates");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("01M5D1QX7W3E9Y2A6K0B8C4F5H.loro"), b"edits").unwrap();
    s.personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    let moved = s
        .team
        .loro_dir(&s.spec)
        .join("updates/01M5D1QX7W3E9Y2A6K0B8C4F5H.loro");
    assert_eq!(fs::read(moved).unwrap(), b"edits");
}

#[test]
fn only_a_person_moves_a_project() {
    let mut s = setup();
    assert!(matches!(
        s.personal.move_project(&s.acme, &mut s.team, &claude()),
        Err(Error::PersonOnly(_))
    ));
    assert!(s.team.get_node(&s.acme).unwrap().is_none());
}

#[test]
fn only_projects_move_and_not_twice() {
    let mut s = setup();
    assert!(matches!(
        s.personal.move_project(&s.spec, &mut s.team, &ada()),
        Err(Error::Invalid(_))
    ));
    s.personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    // Already there.
    let preview = s.team.preview_move(&s.acme, &s.team).unwrap();
    assert!(!preview.conflicts.is_empty());
}

#[test]
fn a_move_is_undone_in_both_spaces() {
    let mut s = setup();
    let moved = s
        .personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    // Undone from the team space, where it arrived.
    let undone = s
        .team
        .undo_move(moved.to_change, &mut s.personal, &ada())
        .unwrap();
    assert_eq!(undone.project, s.acme);
    assert_eq!(undone.from_change.undoes, Some(moved.from_change));
    assert_eq!(undone.to_change.undoes, Some(moved.to_change));

    for id in [s.acme, s.app, s.spec, s.plan] {
        assert!(s.personal.get_node(&id).unwrap().is_some(), "{id}");
        assert!(s.team.get_node(&id).unwrap().is_none(), "{id}");
        assert!(s.team.history(&id).unwrap().is_empty(), "{id}");
    }
    // Links come back, crossing ones included.
    assert!(
        s.personal
            .links_from(&s.acme)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::PART_OF && l.to == s.life)
    );
    assert!(
        s.personal
            .links_from(&s.brand)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::RELATED && l.to == s.spec)
    );
    assert!(
        s.personal
            .links_from(&s.plan)
            .unwrap()
            .iter()
            .any(|l| l.kind == link_kinds::MENTIONS && l.to == s.brand)
    );
    assert_eq!(
        s.personal
            .property(&s.plan, property_keys::MARKDOWN_PATH)
            .unwrap()
            .as_deref(),
        Some("Acme/Plan.md")
    );
    assert!(
        s.personal
            .attachment(&s.spec, "diagram.svg")
            .unwrap()
            .is_some()
    );
    assert!(s.personal.folder(&s.folder).unwrap().is_some());
    assert!(s.team.folders().unwrap().is_empty());
    assert!(s.team.attachment(&s.spec, "diagram.svg").unwrap().is_none());

    // Once.
    assert!(matches!(
        s.personal.undo_move(moved.from_change, &mut s.team, &ada()),
        Err(Error::AlreadyUndone { .. })
    ));
    // The undo is not undone one space at a time.
    assert!(matches!(
        s.personal.undo(undone.from_change.id, &ada(), true),
        Err(Error::CannotUndo { .. })
    ));
    // And the project can move again.
    s.personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    assert!(s.team.get_node(&s.plan).unwrap().is_some());
}

#[test]
fn later_work_is_not_undone_and_the_project_moves_back_with_it() {
    let mut s = setup();
    let moved = s
        .personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    s.team
        .update_node(
            &s.spec,
            NodeUpdate {
                body: Some("Third draft, by the team.".into()),
                ..Default::default()
            },
            &Provenance::person("Grace"),
        )
        .unwrap();
    assert!(matches!(
        s.personal.undo_move(moved.from_change, &mut s.team, &ada()),
        Err(Error::UndoConflict { .. })
    ));
    let versions = s.team.history(&s.spec).unwrap().len();

    // Moving it back brings the team's version and every earlier one, once.
    s.team
        .move_project(&s.acme, &mut s.personal, &ada())
        .unwrap();
    let spec = s.personal.get_node(&s.spec).unwrap().unwrap();
    assert_eq!(spec.body, "Third draft, by the team.");
    let history = s.personal.history(&s.spec).unwrap();
    assert_eq!(history.len(), versions);
    assert!(history.iter().any(|v| v.changed_by.actor == "Grace"));
    assert!(history.iter().any(|v| v.changed_by.actor == "claude-code"));
    assert_eq!(
        s.personal.folders_with_projects().unwrap()[0]
            .1
            .as_ref()
            .unwrap()
            .id,
        s.acme
    );
}

#[test]
fn a_node_the_target_has_stops_the_move() {
    let mut s = setup();
    let spec = s.personal.get_node(&s.spec).unwrap().unwrap();
    s.team
        .import_node(
            ImportedNode {
                id: s.spec,
                node: NewNode::note("Spec", "Their copy."),
                created_at: spec.created_at,
                updated_at: spec.updated_at,
                created_by: ada(),
                updated_by: ada(),
            },
            &Properties::new(),
            &ada(),
        )
        .unwrap();
    let preview = s.personal.preview_move(&s.acme, &s.team).unwrap();
    assert_eq!(preview.conflicts.len(), 1, "{:?}", preview.conflicts);
    assert!(matches!(
        s.personal.move_project(&s.acme, &mut s.team, &ada()),
        Err(Error::Invalid(_))
    ));
    assert!(s.team.get_node(&s.acme).unwrap().is_none());
    assert!(s.personal.get_node(&s.acme).unwrap().is_some());
}

#[test]
fn a_canvas_stays_and_is_listed() {
    let mut s = setup();
    let canvas = s
        .personal
        .create_canvas(NewNode::note("Board", ""), &ada())
        .unwrap();
    let board = canvas.node_id.unwrap();
    s.personal
        .link(&board, &s.acme, link_kinds::PART_OF, &ada())
        .unwrap();
    let preview = s.personal.preview_move(&s.acme, &s.team).unwrap();
    assert!(preview.stays.iter().any(|x| x.node == Some(board)));
    s.personal
        .move_project(&s.acme, &mut s.team, &ada())
        .unwrap();
    assert!(s.personal.get_node(&board).unwrap().is_some());
}

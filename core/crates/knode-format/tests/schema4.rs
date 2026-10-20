//! Link kinds, backlinks, views and attachments (schema version 4), through the public API.

use std::fs;
use std::path::Path;

use knode_format::{
    Error, GraphViewSpec, GroupSpec, LinkDirection, LinkKindSpec, LinkTarget, NewNode, NodeUpdate,
    PositionSpec, Provenance, SCHEMA_VERSION, Space, ViewDisplay, Viewport, actions, kinds,
    link_kinds, view_kinds,
};
use tempfile::TempDir;

fn new_space() -> (TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

fn ada() -> Provenance {
    Provenance::person("Ada")
}

fn claude() -> Provenance {
    Provenance::agent("claude-code").with_session("5f1c")
}

fn set_body(body: &str) -> NodeUpdate {
    NodeUpdate {
        body: Some(body.into()),
        ..Default::default()
    }
}

fn last_change(space: &Space) -> knode_format::Change {
    space.changes(1, None).unwrap().remove(0)
}

fn note(space: &mut Space, title: &str, body: &str) -> knode_format::Node {
    space
        .create_node(NewNode::note(title, body), &ada())
        .unwrap()
}

// Link kinds

#[test]
fn lists_the_built_in_kinds_first() {
    let (_dir, space) = new_space();
    let kinds: Vec<String> = space
        .link_kinds()
        .unwrap()
        .into_iter()
        .map(|k| k.kind)
        .collect();
    assert_eq!(kinds, link_kinds::BUILT_IN);
    let answers = space.link_kind("answers").unwrap().unwrap();
    assert!(answers.built_in);
    assert_eq!(answers.label, "answers");
    assert_eq!(answers.inverse_label, "answered by");
    let related = space.link_kind("related").unwrap().unwrap();
    assert_eq!(related.direction, LinkDirection::Symmetric);
    assert!(space.link_kind("depends-on").unwrap().is_none());
}

#[test]
fn links_only_with_known_kinds() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "Ideas for v2", "");
    let b = note(&mut space, "SQLite for local data", "");

    // Built-in kinds of schema version 4.
    space
        .link(&a.id, &b.id, link_kinds::ANSWERS, &ada())
        .unwrap();
    space
        .link(&a.id, &b.id, link_kinds::CONTRADICTS, &claude())
        .unwrap();

    // An unknown kind is refused until the space defines it.
    assert!(matches!(
        space.link(&a.id, &b.id, "depends-on", &ada()),
        Err(Error::Invalid(_))
    ));
    let kind = space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "depends on", "needed by")
                .with_description("The source cannot work without the target."),
            &claude(),
        )
        .unwrap();
    assert!(!kind.built_in);
    assert_eq!(kind.created_by, Some(claude()));
    let change = last_change(&space);
    assert_eq!(change.action, actions::LINK_KIND_SET);
    assert_eq!(change.subject, "depends-on");
    assert_eq!(change.detail["label"], "depends on");
    assert_eq!(change.detail["inverse_label"], "needed by");
    assert_eq!(change.detail["direction"], "directed");

    let link = space.link(&a.id, &b.id, "depends-on", &ada()).unwrap();
    assert_eq!(link.kind, "depends-on");
    assert_eq!(space.links_from(&a.id).unwrap().len(), 3);

    // Defining the same kind again writes nothing.
    let changes = space.changes(100, None).unwrap().len();
    space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "depends on", "needed by")
                .with_description("The source cannot work without the target."),
            &ada(),
        )
        .unwrap();
    assert_eq!(space.changes(100, None).unwrap().len(), changes);

    // Labels change at any time; the direction does not while links have the kind.
    let changed = space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "relies on", "relied on by"),
            &ada(),
        )
        .unwrap();
    assert_eq!(changed.label, "relies on");
    assert_eq!(changed.created_by, Some(claude()));
    assert_eq!(changed.updated_by, Some(ada()));
    assert!(matches!(
        space.set_link_kind(&LinkKindSpec::symmetric("depends-on", "with"), &ada()),
        Err(Error::LinkKindInUse { links: 1, .. })
    ));
    assert!(matches!(
        space.remove_link_kind("depends-on", &ada()),
        Err(Error::LinkKindInUse { links: 1, .. })
    ));

    // Once its links are gone, it can be removed; the history keeps the name.
    assert!(space.unlink_by(&a.id, &b.id, "depends-on", &ada()).unwrap());
    assert!(space.remove_link_kind("depends-on", &ada()).unwrap());
    assert!(!space.remove_link_kind("depends-on", &ada()).unwrap());
    let change = last_change(&space);
    assert_eq!(change.action, actions::LINK_KIND_REMOVE);
    assert_eq!(change.detail["label"], "relies on");
    assert!(
        space
            .link_history(&a.id)
            .unwrap()
            .iter()
            .any(|l| l.kind == "depends-on")
    );
    // Link kind changes are not undone.
    assert!(matches!(
        space.undo(change.id, &ada(), false),
        Err(Error::CannotUndo { .. })
    ));
}

#[test]
fn refuses_bad_and_built_in_kinds() {
    let (_dir, mut space) = new_space();
    for kind in ["related", "mentions", "answers"] {
        assert!(matches!(
            space.set_link_kind(&LinkKindSpec::symmetric(kind, "x"), &ada()),
            Err(Error::Invalid(_))
        ));
        assert!(space.remove_link_kind(kind, &ada()).is_err());
    }
    for spec in [
        LinkKindSpec::directed("Bad Kind", "x", ""),
        LinkKindSpec::directed("ok", "", ""),
        LinkKindSpec::directed("ok", "a\nb", ""),
        LinkKindSpec::directed("ok", "x".repeat(65), ""),
        LinkKindSpec::directed("ok", "x", "y".repeat(65)),
    ] {
        assert!(
            matches!(space.set_link_kind(&spec, &ada()), Err(Error::Invalid(_))),
            "{spec:?}"
        );
    }
    // A symmetric kind keeps no inverse label.
    let kind = space
        .set_link_kind(
            &LinkKindSpec {
                inverse_label: "ignored".into(),
                ..LinkKindSpec::symmetric("sibling-of", "sibling of")
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(kind.inverse_label, "");
}

#[test]
fn symmetric_kinds_are_one_link_between_two_nodes() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "A", "");
    let b = note(&mut space, "B", "");
    let first = space.link(&a.id, &b.id, "related", &ada()).unwrap();
    // Linking B to A returns the link from A to B.
    let again = space.link(&b.id, &a.id, "related", &claude()).unwrap();
    assert_eq!(again, first);
    assert_eq!(space.links_from(&b.id).unwrap().len(), 0);
    // A directed kind links both ways.
    space.link(&b.id, &a.id, "part-of", &ada()).unwrap();
    space.link(&a.id, &b.id, "part-of", &ada()).unwrap();
    assert_eq!(space.backlinks(&a.id).unwrap().len(), 1);
    // Unlinking a symmetric link works from either node.
    assert!(space.unlink_by(&b.id, &a.id, "related", &ada()).unwrap());
    assert!(
        !space
            .links_from(&a.id)
            .unwrap()
            .iter()
            .any(|l| l.kind == "related")
    );
}

#[test]
fn node_links_read_with_labels() {
    let (_dir, mut space) = new_space();
    let project = note(&mut space, "Garden planner", "");
    let ideas = note(
        &mut space,
        "Ideas for v2",
        "Open questions for [[Garden planner]].",
    );
    let decision = note(&mut space, "SQLite for local data", "");
    space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "depends on", "needed by"),
            &ada(),
        )
        .unwrap();
    space
        .set_link_kind(&LinkKindSpec::directed("blocks", "blocks", ""), &ada())
        .unwrap();
    space
        .link(&ideas.id, &project.id, "part-of", &ada())
        .unwrap();
    space
        .link(&ideas.id, &decision.id, "depends-on", &ada())
        .unwrap();
    space
        .link(&decision.id, &ideas.id, "blocks", &ada())
        .unwrap();
    space
        .link(&project.id, &ideas.id, "related", &ada())
        .unwrap();
    space.link(&ideas.id, &ideas.id, "related", &ada()).unwrap();

    let shown: Vec<(bool, String, String)> = space
        .node_links(&ideas.id)
        .unwrap()
        .into_iter()
        .map(|l| (l.outgoing, l.label, l.title))
        .collect();
    assert_eq!(
        shown,
        [
            (true, "depends on".into(), "SQLite for local data".into()),
            (true, "mentions".into(), "Garden planner".into()),
            (true, "part of".into(), "Garden planner".into()),
            (true, "related to".into(), "Ideas for v2".into()),
            // A kind with no inverse label reads with its name from the target.
            (false, "blocks".into(), "SQLite for local data".into()),
            (false, "related to".into(), "Garden planner".into()),
        ]
    );

    let back: Vec<(String, String)> = space
        .labeled_backlinks(&project.id)
        .unwrap()
        .into_iter()
        .map(|l| (l.label, l.title))
        .collect();
    assert_eq!(
        back,
        [
            ("mentioned in".to_string(), "Ideas for v2".to_string()),
            ("has part".into(), "Ideas for v2".into()),
        ]
    );
    // A symmetric kind linked both ways is shown once.
    let kinds = space.link_kinds().unwrap().len();
    assert_eq!(kinds, link_kinds::BUILT_IN.len() + 2);
}

#[test]
fn backlinks_are_fast_in_a_large_space() {
    let (_dir, mut space) = new_space();
    let hub = note(&mut space, "Hub", "");
    let mut others = Vec::new();
    for i in 0..300 {
        others.push(note(&mut space, &format!("Note {i}"), "See [[Hub]]."));
    }
    for other in others.iter().take(100) {
        space.link(&other.id, &hub.id, "part-of", &ada()).unwrap();
    }
    let start = std::time::Instant::now();
    let back = space.labeled_backlinks(&hub.id).unwrap();
    assert_eq!(back.len(), 400);
    assert!(start.elapsed().as_millis() < 500, "{:?}", start.elapsed());
}

// Views

#[test]
fn graph_views_are_created_changed_and_removed() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "A", "");
    let b = note(&mut space, "B", "");

    let view = space
        .set_graph_view(
            &GraphViewSpec {
                id: None,
                name: "  Decisions  ".into(),
                filter: Some(serde_json::json!({ "node_kinds": ["decision"] })),
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(view.kind, view_kinds::GRAPH);
    assert_eq!(view.name, "Decisions");
    assert_eq!(view.node_id, None);
    assert_eq!(view.filter().unwrap()["node_kinds"][0], "decision");
    let change = last_change(&space);
    assert_eq!(change.action, actions::VIEW_SET);
    assert_eq!(change.subject, view.id);
    assert_eq!(change.detail["name"], "Decisions");

    // Positions, the viewport and unknown settings add no change.
    let changes = space.changes(100, None).unwrap().len();
    space
        .set_positions(
            &view.id,
            &[
                PositionSpec::at(a.id, 10.0, 20.0),
                PositionSpec {
                    size: Some((200.0, 80.0)),
                    display: ViewDisplay::Collapsed,
                    ..PositionSpec::at(b.id, -5.5, 3.0)
                },
            ],
            &claude(),
        )
        .unwrap();
    space
        .set_viewport(
            &view.id,
            &Viewport {
                x: 1.0,
                y: 2.0,
                zoom: 1.5,
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(space.changes(100, None).unwrap().len(), changes);
    let positions = space.positions(&view.id).unwrap();
    assert_eq!(positions.len(), 2);
    let pb = positions.iter().find(|p| p.node_id == b.id).unwrap();
    assert_eq!(
        (pb.x, pb.y, pb.width, pb.height),
        (-5.5, 3.0, Some(200.0), Some(80.0))
    );
    assert_eq!(pb.display, ViewDisplay::Collapsed);
    assert_eq!(pb.updated_by, claude());
    let view = space.view(&view.id).unwrap().unwrap();
    assert_eq!(view.viewport().unwrap().zoom, 1.5);

    // Moving a node replaces its position.
    space
        .set_positions(&view.id, &[PositionSpec::at(a.id, 0.0, 0.0)], &ada())
        .unwrap();
    let pa = space
        .positions(&view.id)
        .unwrap()
        .into_iter()
        .find(|p| p.node_id == a.id)
        .unwrap();
    assert_eq!((pa.x, pa.y), (0.0, 0.0));

    // Renaming keeps the viewport; changing nothing writes nothing.
    let renamed = space
        .set_graph_view(
            &GraphViewSpec {
                id: Some(view.id.clone()),
                name: "All".into(),
                filter: None,
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(renamed.name, "All");
    assert!(renamed.filter().is_none());
    assert!(renamed.viewport().is_some());
    let changes = space.changes(100, None).unwrap().len();
    space
        .set_graph_view(
            &GraphViewSpec {
                id: Some(view.id.clone()),
                name: "All".into(),
                filter: None,
            },
            &ada(),
        )
        .unwrap();
    assert_eq!(space.changes(100, None).unwrap().len(), changes);

    // Groups are only on a canvas.
    assert!(
        space
            .set_group(
                &view.id,
                &GroupSpec {
                    id: None,
                    label: "x".into(),
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                    display: ViewDisplay::Open,
                },
                &ada()
            )
            .is_err()
    );

    assert_eq!(space.views().unwrap().len(), 1);
    assert!(space.remove_view(&view.id, &ada()).unwrap());
    assert_eq!(last_change(&space).action, actions::VIEW_REMOVE);
    assert!(space.views().unwrap().is_empty());
    assert!(space.positions(&view.id).unwrap().is_empty());
    assert!(!space.remove_view(&view.id, &ada()).unwrap());
}

#[test]
fn refuses_bad_views_and_positions() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "A", "");
    let spec = |name: &str, filter| GraphViewSpec {
        id: None,
        name: name.into(),
        filter,
    };
    assert!(space.set_graph_view(&spec(" ", None), &ada()).is_err());
    assert!(
        space
            .set_graph_view(&spec(&"x".repeat(129), None), &ada())
            .is_err()
    );
    assert!(
        space
            .set_graph_view(&spec("x", Some(serde_json::json!([1]))), &ada())
            .is_err()
    );
    let view = space.set_graph_view(&spec("x", None), &ada()).unwrap();
    let bad = [
        PositionSpec::at(a.id, f64::NAN, 0.0),
        PositionSpec {
            size: Some((0.0, 1.0)),
            ..PositionSpec::at(a.id, 0.0, 0.0)
        },
        PositionSpec {
            group_id: Some("01M5B73W76PSMH5FH4R0T2KGN7".into()),
            ..PositionSpec::at(a.id, 0.0, 0.0)
        },
    ];
    for position in bad {
        assert!(space.set_positions(&view.id, &[position], &ada()).is_err());
    }
    let missing = knode_format::NodeId::parse("01M5B73W76PSMH5FH4R0T2KGN7").unwrap();
    assert!(matches!(
        space.set_positions(&view.id, &[PositionSpec::at(missing, 0.0, 0.0)], &ada()),
        Err(Error::NotFound(_))
    ));
    assert!(space.set_positions("nope", &[], &ada()).is_err());
    assert!(
        space
            .set_viewport(
                &view.id,
                &Viewport {
                    x: 0.0,
                    y: 0.0,
                    zoom: 0.0
                },
                &ada()
            )
            .is_err()
    );
    // Nothing half written.
    assert!(space.positions(&view.id).unwrap().is_empty());
}

#[test]
fn canvases_are_nodes_and_come_back_after_a_delete() {
    let (_dir, mut space) = new_space();
    let a = note(&mut space, "A", "");
    let b = note(&mut space, "B", "");
    let canvas = space
        .create_canvas(
            NewNode::note("Garden board", "Where the beds go.").with_tags(["garden"]),
            &ada(),
        )
        .unwrap();
    assert_eq!(canvas.kind, view_kinds::CANVAS);
    assert_eq!(canvas.name, "");
    let node_id = canvas.node_id.unwrap();
    let node = space.get_node(&node_id).unwrap().unwrap();
    assert_eq!(node.kind, kinds::CANVAS);
    assert_eq!(node.title, "Garden board");
    assert_eq!(node.tags, ["garden"]);
    let change = last_change(&space);
    assert_eq!(change.action, actions::NODE_CREATE);
    assert_eq!(change.subject, node_id.to_string());
    // Searched and linked like any other node.
    assert_eq!(space.search("board", 10).unwrap()[0].node.id, node_id);
    assert_eq!(space.canvas(&node_id).unwrap().unwrap().id, canvas.id);

    let group = space
        .set_group(
            &canvas.id,
            &GroupSpec {
                id: None,
                label: "Beds".into(),
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 300.0,
                display: ViewDisplay::Open,
            },
            &ada(),
        )
        .unwrap();
    space
        .set_positions(
            &canvas.id,
            &[
                PositionSpec {
                    group_id: Some(group.id.clone()),
                    z: 2,
                    ..PositionSpec::at(a.id, 1.0, 1.0)
                },
                PositionSpec::at(b.id, 500.0, 1.0),
            ],
            &ada(),
        )
        .unwrap();
    let positions = space.positions(&canvas.id).unwrap();
    assert_eq!(positions[1].node_id, a.id, "back to front");
    assert_eq!(positions[1].group_id.as_deref(), Some(group.id.as_str()));
    // Linking two nodes on a canvas is making a link by hand.
    space.link(&a.id, &b.id, "related", &ada()).unwrap();

    // A canvas view is not removed on its own.
    assert!(space.remove_view(&canvas.id, &ada()).is_err());

    // Deleting the canvas is deleting its node; its view stays, hidden.
    space.delete_node(&node_id, &ada()).unwrap();
    assert!(space.views().unwrap().is_empty());
    let delete = last_change(&space).id;
    space.undo(delete, &ada(), false).unwrap();
    assert_eq!(space.views().unwrap()[0].id, canvas.id);
    assert_eq!(space.positions(&canvas.id).unwrap().len(), 2);
    assert_eq!(
        space.groups(&canvas.id).unwrap(),
        std::slice::from_ref(&group)
    );

    // A deleted node keeps its position, hidden, and gets it back when restored.
    space.delete_node(&b.id, &ada()).unwrap();
    assert_eq!(space.positions(&canvas.id).unwrap().len(), 1);
    let deleted = space.history(&b.id).unwrap().pop().unwrap();
    space.restore(&b.id, deleted.seq, &ada()).unwrap();
    assert_eq!(space.positions(&canvas.id).unwrap().len(), 2);

    // Removing a group leaves its nodes on the canvas, outside any group.
    assert!(space.remove_group(&group.id).unwrap());
    let positions = space.positions(&canvas.id).unwrap();
    assert_eq!(positions.len(), 2);
    assert!(positions.iter().all(|p| p.group_id.is_none()));

    // Taking a node off the canvas does not delete it.
    assert_eq!(space.remove_positions(&canvas.id, &[a.id]).unwrap(), 1);
    assert!(space.get_node(&a.id).unwrap().is_some());

    // Purging the node removes the canvas for good.
    space.purge(&node_id, &ada()).unwrap();
    assert!(space.canvas(&node_id).unwrap().is_none());
    assert!(space.view(&canvas.id).unwrap().is_none());
}

// Attachments

fn blob_files(space: &Space) -> Vec<String> {
    let mut found = Vec::new();
    for prefix in fs::read_dir(space.path().join("blobs")).unwrap() {
        let prefix = prefix.unwrap();
        if !prefix.file_type().unwrap().is_dir() {
            continue;
        }
        for file in fs::read_dir(prefix.path()).unwrap() {
            found.push(format!(
                "{}{}",
                prefix.file_name().to_string_lossy(),
                file.unwrap().file_name().to_string_lossy()
            ));
        }
    }
    found.sort();
    found
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture";
/// The SHA-256 of `test`.
const TEST_HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

#[test]
fn attaches_files_by_hash() {
    let (_dir, mut space) = new_space();
    let node = note(&mut space, "Beds", "");
    let attachment = space
        .attach(&node.id, "plan.txt", &b"test"[..], &claude())
        .unwrap();
    assert_eq!(attachment.hash, TEST_HASH);
    assert_eq!(attachment.size, 4);
    assert_eq!(attachment.media_type, "text/plain");
    assert_eq!(attachment.created_by, claude());
    let file = space.path().join("blobs/9f").join(&TEST_HASH[2..]);
    assert_eq!(fs::read(&file).unwrap(), b"test");
    assert_eq!(space.blob_path(TEST_HASH).unwrap(), Some(file));
    assert_eq!(space.read_blob(TEST_HASH).unwrap().unwrap(), b"test");
    let change = last_change(&space);
    assert_eq!(change.action, actions::ATTACHMENT_ADD);
    assert_eq!(change.subject, node.id.to_string());
    assert_eq!(change.detail["name"], "plan.txt");
    assert_eq!(change.detail["hash"], TEST_HASH);

    // The same content again: one file. The same name and content: nothing new.
    let changes = space.changes(100, None).unwrap().len();
    let again = space
        .attach(&node.id, "plan.txt", &b"test"[..], &ada())
        .unwrap();
    assert_eq!(again, attachment);
    assert_eq!(space.changes(100, None).unwrap().len(), changes);
    let other = note(&mut space, "Other", "");
    space
        .attach(&other.id, "copy.txt", &b"test"[..], &ada())
        .unwrap();
    assert_eq!(blob_files(&space), [TEST_HASH]);

    // A name taken by other content gets a number.
    let second = space
        .attach(&node.id, "plan.txt", &b"other"[..], &ada())
        .unwrap();
    assert_eq!(second.name, "plan 2.txt");
    let third = space.attach(&node.id, "plan.txt", PNG, &ada()).unwrap();
    assert_eq!(third.name, "plan 3.txt");
    assert_eq!(third.media_type, "image/png", "found from the content");
    let names: Vec<String> = space
        .attachments(&node.id)
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names.len(), 3);
    assert!(names.contains(&"plan 2.txt".to_string()));

    // Names are cleaned of characters links and file systems use.
    let odd = space
        .attach(&node.id, " a/b[1]#c.txt ", &b"x"[..], &ada())
        .unwrap();
    assert_eq!(odd.name, "a-b-1--c.txt");
    assert!(space.attach(&node.id, "  ", &b"x"[..], &ada()).is_err());
    let missing = knode_format::NodeId::parse("01M5B73W76PSMH5FH4R0T2KGN7").unwrap();
    assert!(matches!(
        space.attach(&missing, "x.txt", &b"x"[..], &ada()),
        Err(Error::NotFound(_))
    ));
    // No temporary file is left behind.
    let temps = fs::read_dir(space.path().join("blobs"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        })
        .count();
    assert_eq!(temps, 0);
}

#[test]
fn attaches_a_file_from_disk() {
    let (dir, mut space) = new_space();
    let node = note(&mut space, "Beds", "");
    let path = dir.path().join("beds.svg");
    fs::write(&path, "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    let attachment = space.attach_file(&node.id, &path, &ada()).unwrap();
    assert_eq!(attachment.name, "beds.svg");
    assert_eq!(attachment.media_type, "image/svg+xml");
    assert_eq!(
        space.attachment(&node.id, "beds.svg").unwrap(),
        Some(attachment)
    );
    assert!(space.attachment(&node.id, "BEDS.svg").unwrap().is_none());
}

#[test]
fn a_corrupt_or_missing_file_is_never_used() {
    let (_dir, mut space) = new_space();
    let node = note(&mut space, "Beds", "");
    space
        .attach(&node.id, "plan.txt", &b"test"[..], &ada())
        .unwrap();
    let file = space.blob_path(TEST_HASH).unwrap().unwrap();
    fs::write(&file, "changed").unwrap();
    assert!(space.read_blob(TEST_HASH).is_err());
    fs::remove_file(&file).unwrap();
    assert_eq!(space.read_blob(TEST_HASH).unwrap(), None);
    assert_eq!(space.blob_path(TEST_HASH).unwrap(), None);
    // The attachment is still listed, shown as missing.
    assert_eq!(space.attachments(&node.id).unwrap().len(), 1);
    assert!(space.blob_path("not a hash").is_err());
}

#[test]
fn wiki_links_point_at_attachments_before_titles() {
    let (_dir, mut space) = new_space();
    let diagram = note(&mut space, "beds.svg", "A node with the same title.");
    let node = note(
        &mut space,
        "Garden",
        "![[beds.svg]] and [[plan.pdf]] and [[Diagram]].",
    );
    // Before attaching: a link to the node titled beds.svg, and plan.pdf waits.
    assert_eq!(space.links_from(&node.id).unwrap().len(), 1);
    assert_eq!(
        space.unresolved_links(&node.id).unwrap(),
        ["Diagram", "plan.pdf"]
    );

    space
        .attach(&node.id, "beds.svg", &b"<svg/>"[..], &ada())
        .unwrap();
    space
        .attach(&node.id, "plan.pdf", &b"%PDF-1.7"[..], &ada())
        .unwrap();
    // The attachments win: no link, no unresolved link.
    assert!(space.links_from(&node.id).unwrap().is_empty());
    assert_eq!(space.unresolved_links(&node.id).unwrap(), ["Diagram"]);
    assert!(matches!(
        space.resolve_link(&node.id, "beds.svg").unwrap(),
        Some(LinkTarget::Attachment(a)) if a.name == "beds.svg"
    ));
    assert!(matches!(
        space.resolve_link(&diagram.id, "beds.svg").unwrap(),
        Some(LinkTarget::Node(n)) if n.id == diagram.id
    ));
    assert!(matches!(
        space.resolve_link(&node.id, "Garden#Plan").unwrap(),
        Some(LinkTarget::Node(n)) if n.id == node.id
    ));
    assert!(space.resolve_link(&node.id, "Diagram").unwrap().is_none());

    // A body written later keeps resolving to the attachment.
    space
        .update_node(&node.id, set_body("Only [[plan.pdf]] now."), &ada())
        .unwrap();
    assert!(space.links_from(&node.id).unwrap().is_empty());
    assert!(space.unresolved_links(&node.id).unwrap().is_empty());

    // Removing it lets the link wait again.
    assert!(space.detach(&node.id, "plan.pdf", &ada()).unwrap());
    assert_eq!(space.unresolved_links(&node.id).unwrap(), ["plan.pdf"]);
    let change = last_change(&space);
    assert_eq!(change.action, actions::ATTACHMENT_REMOVE);
    assert_eq!(change.detail["name"], "plan.pdf");
    assert!(!space.detach(&node.id, "plan.pdf", &ada()).unwrap());
    // The blob stays.
    assert_eq!(blob_files(&space).len(), 2);
}

#[test]
fn attachments_are_undone_and_redone() {
    let (_dir, mut space) = new_space();
    let node = note(&mut space, "Beds", "![[plan.txt]]");
    let added = {
        space
            .attach(&node.id, "plan.txt", &b"test"[..], &ada())
            .unwrap();
        last_change(&space).id
    };
    let undo = space.undo(added, &ada(), false).unwrap();
    assert_eq!(undo.undoes, Some(added));
    assert!(space.attachments(&node.id).unwrap().is_empty());
    assert_eq!(space.unresolved_links(&node.id).unwrap(), ["plan.txt"]);
    assert!(matches!(
        space.undo(added, &ada(), false),
        Err(Error::AlreadyUndone { .. })
    ));
    // Undoing the undo attaches it again.
    space.undo(undo.id, &ada(), false).unwrap();
    assert_eq!(space.attachments(&node.id).unwrap()[0].hash, TEST_HASH);
    assert!(space.unresolved_links(&node.id).unwrap().is_empty());

    // Undoing a remove adds it back, if the name is free and the file is there.
    space.detach(&node.id, "plan.txt", &ada()).unwrap();
    let removed = last_change(&space).id;
    space
        .attach(&node.id, "plan.txt", &b"other"[..], &ada())
        .unwrap();
    assert!(matches!(
        space.undo(removed, &ada(), false),
        Err(Error::CannotUndo { .. })
    ));
    space.detach(&node.id, "plan.txt", &ada()).unwrap();
    space.undo(removed, &ada(), false).unwrap();
    assert_eq!(
        space
            .attachment(&node.id, "plan.txt")
            .unwrap()
            .unwrap()
            .hash,
        TEST_HASH
    );

    // Undoing an add whose attachment changed since fails.
    space.detach(&node.id, "plan.txt", &ada()).unwrap();
    let other = space
        .attach(&node.id, "plan.txt", &b"other"[..], &ada())
        .unwrap();
    let added_other = last_change(&space).id;
    space.detach(&node.id, "plan.txt", &ada()).unwrap();
    space
        .attach(&node.id, "plan.txt", &b"test"[..], &ada())
        .unwrap();
    assert_ne!(other.hash, TEST_HASH);
    assert!(matches!(
        space.undo(added_other, &ada(), false),
        Err(Error::CannotUndo { .. })
    ));

    // An attachment outlives its deleted node and comes back with it.
    space.delete_node(&node.id, &ada()).unwrap();
    assert_eq!(space.attachments(&node.id).unwrap().len(), 1);
    let deleted = last_change(&space).id;
    space.undo(deleted, &ada(), false).unwrap();
    assert!(space.unresolved_links(&node.id).unwrap().is_empty());
}

#[test]
fn purges_only_unused_blobs_and_only_by_a_person() {
    let (_dir, mut space) = new_space();
    let kept = note(&mut space, "Kept", "");
    space
        .attach(&kept.id, "plan.txt", &b"test"[..], &ada())
        .unwrap();
    let detached = space
        .attach(&kept.id, "old.txt", &b"old"[..], &ada())
        .unwrap();
    space.detach(&kept.id, "old.txt", &ada()).unwrap();
    // Referenced by hash from a body: kept, though no attachment names it.
    let by_url = space.add_blob(&b"by url"[..], "x.bin", &ada()).unwrap();
    note(
        &mut space,
        "Linker",
        &format!("![Picture](knode:blob/{})", by_url.hash),
    );
    // Referenced only by an old version: kept.
    let in_history = space.add_blob(&b"in history"[..], "y.bin", &ada()).unwrap();
    let edited = note(
        &mut space,
        "Edited",
        &format!("[file](knode:blob/{})", in_history.hash),
    );
    space
        .update_node(&edited.id, set_body("gone"), &ada())
        .unwrap();
    // A file no row names, as a crash would leave.
    let stray = "ab".repeat(32);
    fs::create_dir_all(space.path().join("blobs/ab")).unwrap();
    fs::write(space.path().join("blobs/ab").join(&stray[2..]), "stray").unwrap();
    // A file that does not follow the layout is not touched.
    fs::write(space.path().join("blobs/README"), "keep").unwrap();
    // Attached to a deleted node: kept, so the node can come back with it.
    let deleted = note(&mut space, "Deleted", "");
    let on_deleted = space
        .attach(&deleted.id, "d.txt", &b"deleted"[..], &ada())
        .unwrap();
    space.delete_node(&deleted.id, &ada()).unwrap();

    assert!(matches!(
        space.purge_blobs(&claude()),
        Err(Error::PersonOnly(_))
    ));
    let mut expected = vec![detached.hash.clone(), stray.clone()];
    expected.sort();
    assert_eq!(space.purge_blobs(&ada()).unwrap(), expected);
    let change = last_change(&space);
    assert_eq!(change.action, actions::BLOBS_PURGE);
    assert_eq!(change.subject, "");
    assert_eq!(change.detail["hashes"].as_array().unwrap().len(), 2);
    let files = blob_files(&space);
    assert!(files.contains(&TEST_HASH.to_string()));
    assert!(files.contains(&by_url.hash));
    assert!(files.contains(&in_history.hash));
    assert!(files.contains(&on_deleted.hash));
    assert!(!files.contains(&detached.hash));
    assert!(!files.contains(&stray));
    assert!(space.blob(&detached.hash).unwrap().is_none());
    assert!(space.path().join("blobs/README").exists());
    // Nothing more to purge: nothing recorded.
    let changes = space.changes(100, None).unwrap().len();
    assert!(space.purge_blobs(&ada()).unwrap().is_empty());
    assert_eq!(space.changes(100, None).unwrap().len(), changes);

    // Purging the deleted node removes its attachments and the files only it used.
    let purge = space.purge(&deleted.id, &ada()).unwrap();
    assert_eq!(purge.detail["hashes"][0], on_deleted.hash);
    assert!(!blob_files(&space).contains(&on_deleted.hash));
    assert!(space.attachments(&deleted.id).unwrap().is_empty());
}

#[test]
fn refuses_files_over_the_limit() {
    struct Endless;
    impl std::io::Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(7);
            Ok(buf.len())
        }
    }
    let (_dir, mut space) = new_space();
    let node = note(&mut space, "Big", "");
    assert!(matches!(
        space.attach(&node.id, "big.bin", Endless, &ada()),
        Err(Error::Invalid(_))
    ));
    assert!(space.attachments(&node.id).unwrap().is_empty());
    assert!(blob_files(&space).is_empty());
    let left = fs::read_dir(space.path().join("blobs")).unwrap().count();
    assert_eq!(left, 0, "the temporary file is removed");
}

// Migrating

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn opens_the_schema_3_example_space() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Example.knode");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/example-v3.knode"),
        &path,
    );
    let version = |path: &Path| -> u32 {
        rusqlite::Connection::open(path.join("knode.db"))
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(version(&path), 3);
    let changes_before = {
        let conn = rusqlite::Connection::open(path.join("knode.db")).unwrap();
        conn.query_row("SELECT count(*) FROM changes", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    };

    let mut space = Space::open(&path).unwrap();
    assert_eq!(version(&path), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 4);
    assert_eq!(space.node_count().unwrap(), 5);
    // Migration 4 changes no row.
    assert_eq!(
        space.changes(1000, None).unwrap().len() as i64,
        changes_before
    );
    assert!(space.views().unwrap().is_empty());
    assert_eq!(
        space.link_kinds().unwrap().len(),
        link_kinds::BUILT_IN.len()
    );

    // The links it had read with labels, and everything new works.
    let project = space.find_by_title("Garden planner").unwrap().remove(0);
    let back = space.labeled_backlinks(&project.id).unwrap();
    assert!(back.iter().any(|l| l.label == "has part"));
    space
        .set_link_kind(
            &LinkKindSpec::directed("depends-on", "depends on", "needed by"),
            &ada(),
        )
        .unwrap();
    space
        .attach(&project.id, "plan.txt", &b"test"[..], &ada())
        .unwrap();
    let canvas = space
        .create_canvas(NewNode::note("Board", ""), &ada())
        .unwrap();
    space
        .set_positions(
            &canvas.id,
            &[PositionSpec::at(project.id, 0.0, 0.0)],
            &ada(),
        )
        .unwrap();
    drop(space);
    let space = Space::open(&path).unwrap();
    assert_eq!(space.positions(&canvas.id).unwrap().len(), 1);
}

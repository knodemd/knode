//! The graph a view shows and the board of a canvas.

use knode_format::{
    GraphFilter, GraphViewSpec, GroupSpec, NewNode, PositionSpec, Provenance, Space, ViewDisplay,
};
use serde_json::json;

fn space() -> (tempfile::TempDir, Space) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    (dir, space)
}

#[test]
fn a_graph_keeps_what_its_filter_keeps() {
    let (_dir, mut space) = space();
    let ada = Provenance::person("Ada");
    let bot = Provenance::agent("claude-code");
    let plan = space
        .create_node(
            NewNode::note("Plan", "See [[Idea]].").with_kind("decision"),
            &ada,
        )
        .unwrap();
    let idea = space.create_node(NewNode::note("Idea", ""), &bot).unwrap();
    let other = space
        .create_node(NewNode::note("Other", "").with_tags(["x"]), &ada)
        .unwrap();
    space.link(&other.id, &plan.id, "part-of", &ada).unwrap();

    let all = space.graph(None, None).unwrap();
    assert_eq!(all.nodes.len(), 3);
    assert_eq!(all.nodes[0].id, other.id, "newest first");
    assert_eq!(all.edges.len(), 2);
    assert!(all.positions.is_empty());
    assert!(all.link_kinds.iter().any(|k| k.kind == "part-of"));

    let by_ada = GraphFilter {
        actors: Some(vec!["Ada".into()]),
        link_kinds: Some(vec!["part-of".into()]),
        ..Default::default()
    };
    let graph = space.graph(None, Some(&by_ada)).unwrap();
    let ids: Vec<_> = graph.nodes.iter().map(|n| n.id).collect();
    assert_eq!(ids, [other.id, plan.id]);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].kind, "part-of");

    // A view's own filter applies, and its positions come back for the nodes shown.
    let view = space
        .set_graph_view(
            &GraphViewSpec {
                id: None,
                name: "Decisions".into(),
                filter: Some(json!({ "node_kinds": ["decision"] })),
            },
            &ada,
        )
        .unwrap();
    space
        .set_positions(
            &view.id,
            &[
                PositionSpec::at(plan.id, 1.0, 2.0),
                PositionSpec::at(idea.id, 3.0, 4.0),
            ],
            &ada,
        )
        .unwrap();
    let graph = space.graph(Some(&view.id), None).unwrap();
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.positions.len(), 1);
    assert_eq!(graph.positions[0].node_id, plan.id);
    assert!(space.graph(Some("nope"), None).is_err());
}

#[test]
fn a_canvas_board_has_its_items_groups_and_links() {
    let (_dir, mut space) = space();
    let ada = Provenance::person("Ada");
    let canvas = space
        .create_canvas(NewNode::note("Launch", "What ships when."), &ada)
        .unwrap();
    let node = canvas.node_id.unwrap();
    let a = space
        .create_node(NewNode::note("A", "## **First** line\nmore"), &ada)
        .unwrap();
    let b = space
        .create_node(NewNode::note("B", "[[A]]"), &ada)
        .unwrap();
    let outside = space
        .create_node(NewNode::note("C", "[[A]]"), &ada)
        .unwrap();
    let group = space
        .set_group(
            &canvas.id,
            &GroupSpec {
                id: None,
                label: "Now".into(),
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
                display: ViewDisplay::Open,
            },
            &ada,
        )
        .unwrap();
    let mut in_group = PositionSpec::at(a.id, 10.0, 10.0);
    in_group.group_id = Some(group.id.clone());
    in_group.size = Some((80.0, 40.0));
    space
        .set_positions(
            &canvas.id,
            &[in_group, PositionSpec::at(b.id, 200.0, 0.0)],
            &ada,
        )
        .unwrap();

    let board = space.canvas_board(&node).unwrap().unwrap();
    assert_eq!(board.title, "Launch");
    assert_eq!(board.body, "What ships when.");
    assert_eq!(board.items.len(), 2);
    let first = board
        .items
        .iter()
        .find(|i| i.position.node_id == a.id)
        .unwrap();
    assert_eq!(first.excerpt, "First line");
    assert_eq!(first.position.group_id.as_deref(), Some(group.id.as_str()));
    assert_eq!(board.groups, [group]);
    assert_eq!(board.edges.len(), 1, "the link from C is not on the canvas");
    assert_eq!((board.edges[0].from, board.edges[0].to), (b.id, a.id));

    assert!(space.canvas_board(&outside.id).unwrap().is_none());
    space.delete_node(&node, &ada).unwrap();
    assert!(space.canvas_board(&node).unwrap().is_none());
}

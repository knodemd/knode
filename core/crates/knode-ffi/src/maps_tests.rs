use std::collections::HashSet;
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
    let path = dir.path().join("Maps.knode").display().to_string();
    KnodeSpace::create(path, "Maps".into()).unwrap()
}

fn changes(space: &KnodeSpace) -> usize {
    space.lock().changes(1000, None).unwrap().len()
}

#[test]
fn lists_built_in_kinds_and_defines_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let kinds = space.link_kinds().unwrap();
    let names: Vec<&str> = kinds.iter().map(|k| k.kind.as_str()).collect();
    assert_eq!(
        names,
        [
            "mentions",
            "related",
            "part-of",
            "derived-from",
            "answers",
            "contradicts"
        ]
    );
    assert!(kinds.iter().all(|k| k.built_in));
    let related = kinds.iter().find(|k| k.kind == "related").unwrap();
    assert_eq!(related.direction, LinkDirection::Symmetric);

    let spec = LinkKindSpec {
        kind: "depends-on".into(),
        label: "depends on".into(),
        inverse_label: "needed by".into(),
        direction: LinkDirection::Directed,
        description: "Cannot be done without the target.".into(),
    };
    let kind = space.set_link_kind(spec.clone(), me()).unwrap();
    assert_eq!(kind.label, "depends on");
    assert!(!kind.built_in);
    assert_eq!(space.link_kinds().unwrap().len(), 7);

    let bad = LinkKindSpec {
        kind: "Depends On".into(),
        ..spec
    };
    assert!(matches!(
        space.set_link_kind(bad, me()),
        Err(KnodeError::Invalid { .. })
    ));
    assert!(space.remove_link_kind("depends-on".into(), me()).unwrap());
    assert_eq!(space.link_kinds().unwrap().len(), 6);
}

#[test]
fn shows_links_and_backlinks_with_their_labels() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let roadmap = space
        .create_node("Roadmap".into(), String::new(), me())
        .unwrap();
    let idea = space
        .create_node("Idea".into(), "See [[Roadmap]].".into(), me())
        .unwrap();
    space
        .link(idea.id.clone(), roadmap.id.clone(), "part-of".into(), me())
        .unwrap();
    space
        .link(roadmap.id.clone(), idea.id.clone(), "related".into(), me())
        .unwrap();

    let from_idea = space.node_links(idea.id.clone()).unwrap();
    let labels: Vec<(&str, bool, &str)> = from_idea
        .iter()
        .map(|l| (l.label.as_str(), l.outgoing, l.title.as_str()))
        .collect();
    assert_eq!(
        labels,
        [
            ("mentions", true, "Roadmap"),
            ("part of", true, "Roadmap"),
            ("related to", false, "Roadmap"),
        ]
    );

    let back = space.labeled_backlinks(roadmap.id.clone()).unwrap();
    let labels: Vec<&str> = back.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, ["mentioned in", "has part"]);
    assert!(back.iter().all(|l| l.node == idea.id && !l.outgoing));

    assert!(
        space
            .unlink(idea.id.clone(), roadmap.id.clone(), "part-of".into(), me())
            .unwrap()
    );
    assert!(
        !space
            .unlink(idea.id.clone(), roadmap.id.clone(), "part-of".into(), me())
            .unwrap()
    );
    assert!(matches!(
        space.link(idea.id.clone(), roadmap.id, "no-such-kind".into(), me()),
        Err(KnodeError::Invalid { .. })
    ));
}

#[test]
fn saves_graph_views_with_filters_and_viewports() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let filter = GraphFilter {
        node_kinds: Some(vec!["note".into()]),
        since: Some(1_760_000_000_000),
        ..Default::default()
    };
    let view = space
        .set_graph_view(None, "Notes".into(), Some(filter.clone()), me())
        .unwrap();
    assert_eq!(view.kind, "graph");
    assert_eq!(view.filter, Some(filter.clone()));
    assert_eq!(view.viewport, None);

    let before = changes(&space);
    let viewport = Viewport {
        x: 10.0,
        y: -20.0,
        zoom: 1.5,
    };
    space.set_viewport(view.id.clone(), viewport, me()).unwrap();
    assert_eq!(changes(&space), before, "a viewport adds no change");
    let again = space.view(view.id.clone()).unwrap().unwrap();
    assert_eq!(again.viewport, Some(viewport));

    let renamed = space
        .set_graph_view(Some(view.id.clone()), "All notes".into(), None, me())
        .unwrap();
    assert_eq!(renamed.name, "All notes");
    assert_eq!(renamed.filter, None);
    assert_eq!(renamed.viewport, Some(viewport), "the viewport is kept");
    assert_eq!(space.views().unwrap().len(), 1);

    assert!(space.remove_view(view.id.clone(), me()).unwrap());
    assert!(space.views().unwrap().is_empty());
}

#[test]
fn keeps_filter_keys_it_does_not_know() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let spec = format::GraphViewSpec {
        id: None,
        name: "Later".into(),
        filter: Some(serde_json::json!({ "tags": ["a"], "colour": "red" })),
    };
    let stored = space.lock().set_graph_view(&spec, &me().into()).unwrap();
    let filter = GraphFilter {
        tags: Some(vec!["b".into()]),
        ..Default::default()
    };
    let view = space
        .set_graph_view(Some(stored.id.clone()), "Later".into(), Some(filter), me())
        .unwrap();
    assert_eq!(view.filter.unwrap().tags, Some(vec!["b".into()]));
    let raw = space.lock().view(&stored.id).unwrap().unwrap();
    assert_eq!(raw.filter().unwrap()["colour"], "red");
}

#[test]
fn builds_a_filtered_graph_with_saved_positions() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let a = space.create_node("A".into(), "[[B]]".into(), me()).unwrap();
    let b = space
        .create_node("B".into(), String::new(), agent())
        .unwrap();
    let c = space.create_node("C".into(), String::new(), me()).unwrap();
    space
        .link(a.id.clone(), c.id.clone(), "related".into(), me())
        .unwrap();

    let all = space.graph(None, None).unwrap();
    assert_eq!(all.nodes.len(), 3);
    assert_eq!(all.edges.len(), 2);
    assert!(all.positions.is_empty());
    assert_eq!(all.link_kinds.len(), 6);

    let by_me = GraphFilter {
        actors: Some(vec!["Ada".into()]),
        ..Default::default()
    };
    let graph = space.graph(None, Some(by_me.clone())).unwrap();
    let ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, HashSet::from([a.id.as_str(), c.id.as_str()]));
    assert_eq!(
        graph.edges,
        [GraphEdge {
            from: a.id.clone(),
            to: c.id.clone(),
            kind: "related".into()
        }]
    );

    let only_mentions = GraphFilter {
        link_kinds: Some(vec!["mentions".into()]),
        ..Default::default()
    };
    assert_eq!(
        space.graph(None, Some(only_mentions)).unwrap().edges.len(),
        1
    );
    let future = GraphFilter {
        since: Some(i64::MAX / 2),
        ..Default::default()
    };
    assert!(space.graph(None, Some(future)).unwrap().nodes.is_empty());

    let view = space
        .set_graph_view(None, "Mine".into(), Some(by_me), me())
        .unwrap();
    let before = changes(&space);
    let placed = |id: &str, x: f64| Position {
        node_id: id.into(),
        x,
        y: 0.0,
        width: None,
        height: None,
        z: 0,
        collapsed: false,
        group_id: None,
    };
    space
        .set_positions(
            view.id.clone(),
            vec![placed(&a.id, 1.0), placed(&b.id, 2.0)],
            me(),
        )
        .unwrap();
    assert_eq!(changes(&space), before, "positions add no change");
    let graph = space.graph(Some(view.id.clone()), None).unwrap();
    assert_eq!(graph.nodes.len(), 2, "the view's own filter applies");
    // B is filtered out, so only A's position is returned.
    assert_eq!(graph.positions, [placed(&a.id, 1.0)]);
}

#[test]
fn arranges_a_canvas_with_positions_and_groups() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let a = space
        .create_node("A".into(), "First line.\n\n[[B]]".into(), me())
        .unwrap();
    let b = space.create_node("B".into(), String::new(), me()).unwrap();
    let canvas = space
        .create_canvas("Board".into(), "Planning.".into(), me())
        .unwrap();
    assert_eq!(canvas.kind, "canvas");
    assert_eq!(canvas.name, "Board");
    let node = canvas.node_id.clone().unwrap();
    assert_eq!(
        space.get_node(node.clone()).unwrap().unwrap().kind,
        "canvas"
    );

    let before = changes(&space);
    let group = space
        .set_group(
            canvas.id.clone(),
            GroupSpec {
                id: None,
                label: "Now".into(),
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 300.0,
                collapsed: false,
            },
            me(),
        )
        .unwrap();
    let positions = vec![
        Position {
            node_id: a.id.clone(),
            x: -50.0,
            y: 20.0,
            width: Some(220.0),
            height: Some(120.0),
            z: 1,
            collapsed: false,
            group_id: Some(group.id.clone()),
        },
        Position {
            node_id: b.id.clone(),
            x: 300.0,
            y: 20.0,
            width: None,
            height: None,
            z: 2,
            collapsed: true,
            group_id: None,
        },
    ];
    space
        .set_positions(canvas.id.clone(), positions.clone(), me())
        .unwrap();
    assert_eq!(changes(&space), before, "a canvas adds no change");

    let board = space.canvas_board(node.clone()).unwrap().unwrap();
    assert_eq!(board.view.id, canvas.id);
    assert_eq!(board.items.len(), 2);
    assert_eq!(board.items[0].position, positions[0]);
    assert_eq!(board.items[0].excerpt, "First line.");
    assert_eq!(board.items[1].title, "B");
    assert_eq!(board.groups, std::slice::from_ref(&group));
    assert_eq!(board.edges.len(), 1);
    assert_eq!(board.edges[0].kind, "mentions");

    assert!(space.remove_group(group.id.clone()).unwrap());
    let board = space.canvas_board(node.clone()).unwrap().unwrap();
    assert!(board.groups.is_empty());
    assert_eq!(
        board.items[0].position.group_id, None,
        "nodes leave the group"
    );

    assert_eq!(
        space
            .remove_positions(canvas.id.clone(), vec![b.id.clone()])
            .unwrap(),
        1
    );
    assert_eq!(space.positions(canvas.id.clone()).unwrap().len(), 1);
    assert!(space.get_node(b.id.clone()).unwrap().is_some());

    let half = Position {
        width: Some(10.0),
        ..positions[0].clone()
    };
    assert!(matches!(
        space.set_positions(canvas.id.clone(), vec![half], me()),
        Err(KnodeError::Invalid { .. })
    ));
    assert!(space.canvas_board(a.id.clone()).unwrap().is_none());

    // Deleting the canvas's node hides it; the board is kept for a restore.
    space.delete_node(node.clone(), me()).unwrap();
    assert!(space.views().unwrap().is_empty());
}

/// Builds a large space and times the graph read. Run with
/// `cargo test --release -p knode-ffi -- --ignored --nocapture graph_of_ten_thousand`.
#[test]
#[ignore]
fn graph_of_ten_thousand_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let space = new_space(&dir);
    let count = 10_000;
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let body = if i > 0 {
            format!("Links to [[Node {}]].", i / 2)
        } else {
            String::new()
        };
        ids.push(
            space
                .create_node(format!("Node {i}"), body, me())
                .unwrap()
                .id,
        );
    }
    for i in (0..count).step_by(3) {
        let to = ids[(i * 7 + 13) % count].clone();
        if to != ids[i] {
            space
                .link(ids[i].clone(), to, "related".into(), me())
                .unwrap();
        }
    }
    let start = std::time::Instant::now();
    let graph = space.graph(None, None).unwrap();
    let elapsed = start.elapsed();
    println!(
        "graph: {} nodes, {} edges in {elapsed:?}",
        graph.nodes.len(),
        graph.edges.len()
    );
    assert_eq!(graph.nodes.len(), count);
}

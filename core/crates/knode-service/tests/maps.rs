//! Link kinds, labelled links, views, canvases, attachments and data exports: a real service,
//! the person and agents as clients.

use std::path::{Path, PathBuf};
use std::time::Duration;

use knode_format::{
    GrantLevel, GraphFilter, GroupSpec, LinkDirection, LinkKindSpec, NewNode, Provenance,
    ViewDisplay, Viewport,
};
use knode_service::keys::{fingerprint, generate_key};
use knode_service::protocol::codes;
use knode_service::{
    ChangeKind, Client, Config, Error, Event, MAX_INLINE_ATTACHMENT, PositionInput, ServiceHandle,
    SpaceEvent, SpaceEventKind, Subscription, start,
};
use serde_json::json;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    _service: ServiceHandle,
    person: Client,
}

fn me() -> Provenance {
    Provenance::person("Ada")
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        let person = Client::connect(service.socket_path()).unwrap();
        let fixture = Fixture {
            dir,
            _service: service,
            person,
        };
        fixture
            .person
            .create_space(fixture.space(), "Work")
            .unwrap();
        fixture
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/service.sock")
    }

    fn space(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path())
            .unwrap()
            .join("Work.knode")
    }

    /// An agent with `level` in the space, connected.
    fn agent(&self, name: &str, level: GrantLevel) -> Client {
        let key = generate_key().unwrap();
        self.person
            .add_agent(name, "claude-code", &fingerprint(&key), &me())
            .unwrap();
        self.person
            .set_grant(name, self.space(), level, &me())
            .unwrap();
        Client::connect_as_agent(self.socket(), &key).unwrap()
    }
}

fn rpc(result: Result<impl std::fmt::Debug, Error>) -> knode_service::RpcError {
    match result {
        Err(Error::Rpc(e)) => e,
        other => panic!("expected an error from the service, got {other:?}"),
    }
}

fn next_space_event(events: &Subscription) -> SpaceEvent {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Space(event)) => return event,
            Some(_) => continue,
            None => panic!("no space event"),
        }
    }
}

fn next_change(events: &Subscription) -> knode_service::Change {
    loop {
        match events.recv_timeout(WAIT) {
            Some(Event::Changed(change)) => return change,
            Some(_) => continue,
            None => panic!("no changed notification"),
        }
    }
}

#[test]
fn link_kinds_and_labelled_links() {
    let f = Fixture::new();
    let space = f.space();
    let other = Client::connect(f.socket()).unwrap();
    let events = other.subscribe(Some(&space)).unwrap();

    let kinds = f.person.link_kinds(&space).unwrap();
    assert!(kinds.iter().any(|k| k.kind == "answers" && k.built_in));

    let spec = LinkKindSpec::directed("depends-on", "depends on", "needed by");
    let kind = f.person.set_link_kind(&space, &spec, &me()).unwrap();
    assert_eq!(kind.label, "depends on");
    let event = next_space_event(&events);
    assert_eq!(event.what, SpaceEventKind::LinkKinds);
    assert_eq!(event.link_kind.as_deref(), Some("depends-on"));
    // Setting it again the same way changes nothing and says nothing.
    f.person.set_link_kind(&space, &spec, &me()).unwrap();

    let app = f
        .person
        .create_node(&space, NewNode::note("App", ""), &me())
        .unwrap();
    let db = f
        .person
        .create_node(&space, NewNode::note("Database", ""), &me())
        .unwrap();
    f.person
        .link(&space, &app.id, &db.id, "depends-on", &me())
        .unwrap();

    let links = f.person.node_links(&space, &app.id).unwrap();
    assert_eq!(links.len(), 1);
    assert!(links[0].outgoing);
    assert_eq!(links[0].label, "depends on");
    let back = f.person.labeled_backlinks(&space, &db.id).unwrap();
    assert_eq!(back[0].label, "needed by");
    assert_eq!(back[0].title, "App");

    // A kind in use cannot be removed.
    let refused = rpc(f.person.remove_link_kind(&space, "depends-on", &me()));
    assert_eq!(refused.code, codes::INVALID);
    f.person
        .unlink(&space, &app.id, &db.id, "depends-on", &me())
        .unwrap();
    assert!(
        f.person
            .remove_link_kind(&space, "depends-on", &me())
            .unwrap()
    );
    assert!(
        !f.person
            .remove_link_kind(&space, "depends-on", &me())
            .unwrap()
    );
    let event = loop {
        let event = next_space_event(&events);
        if event.what == SpaceEventKind::LinkKinds {
            break event;
        }
    };
    assert_eq!(event.link_kind.as_deref(), Some("depends-on"));
    assert!(
        !f.person
            .link_kinds(&space)
            .unwrap()
            .iter()
            .any(|k| k.kind == "depends-on")
    );
    let symmetric = LinkKindSpec::symmetric("sibling-of", "sibling of");
    let kind = f.person.set_link_kind(&space, &symmetric, &me()).unwrap();
    assert_eq!(kind.direction, LinkDirection::Symmetric);
}

#[test]
fn graph_views_canvases_positions_and_groups() {
    let f = Fixture::new();
    let space = f.space();
    let other = Client::connect(f.socket()).unwrap();
    let events = other.subscribe(Some(&space)).unwrap();

    let plan = f
        .person
        .create_node(
            &space,
            NewNode::note("Plan", "[[Idea]]").with_kind("decision"),
            &me(),
        )
        .unwrap();
    let idea = f
        .person
        .create_node(&space, NewNode::note("Idea", "A thought."), &me())
        .unwrap();

    let view = f
        .person
        .set_graph_view(
            &space,
            None,
            "Decisions",
            Some(json!({ "node_kinds": ["decision"] })),
            &me(),
        )
        .unwrap();
    assert_eq!(view.kind, "graph");
    let event = next_space_event(&events);
    assert_eq!(event.what, SpaceEventKind::Views);
    assert_eq!(event.view.as_deref(), Some(view.id.as_str()));
    assert_eq!(
        f.person.view(&space, &view.id).unwrap().unwrap().name,
        "Decisions"
    );
    assert!(f.person.view(&space, "nope").unwrap().is_none());
    let bad = rpc(f
        .person
        .set_graph_view(&space, None, "Bad", Some(json!({ "tags": 3 })), &me()));
    assert_eq!(bad.code, codes::INVALID);

    f.person
        .set_positions(
            &space,
            &view.id,
            &[PositionInput::at(plan.id, 10.0, 20.0)],
            &me(),
        )
        .unwrap();
    assert_eq!(next_space_event(&events).what, SpaceEventKind::Views);
    f.person
        .set_viewport(
            &space,
            &view.id,
            Viewport {
                x: 1.0,
                y: 2.0,
                zoom: 1.5,
            },
            &me(),
        )
        .unwrap();
    let saved = f.person.view(&space, &view.id).unwrap().unwrap();
    assert_eq!(saved.viewport().unwrap().zoom, 1.5);

    let graph = f.person.graph(&space, Some(&view.id), None).unwrap();
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.positions[0].x, 10.0);
    let all = f.person.graph(&space, None, None).unwrap();
    assert_eq!(all.nodes.len(), 2);
    assert_eq!(all.edges.len(), 1);
    let filter = GraphFilter {
        link_kinds: Some(vec!["related".into()]),
        ..Default::default()
    };
    assert!(
        f.person
            .graph(&space, None, Some(&filter))
            .unwrap()
            .edges
            .is_empty()
    );

    // A canvas is a node and a view.
    let canvas = f
        .person
        .create_canvas(&space, "Launch", "What ships when.", &me())
        .unwrap();
    let node = canvas.node_id.unwrap();
    assert_eq!(canvas.name, "Launch");
    let change = next_change(&events);
    assert_eq!((change.node_id, change.op), (node, ChangeKind::Create));
    let group = f
        .person
        .set_group(
            &space,
            &canvas.id,
            &GroupSpec {
                id: None,
                label: "Now".into(),
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 200.0,
                display: ViewDisplay::Open,
            },
            &me(),
        )
        .unwrap();
    let mut card = PositionInput::at(plan.id, 5.0, 5.0);
    card.width = Some(120.0);
    card.height = Some(80.0);
    card.group_id = Some(group.id.clone());
    f.person
        .set_positions(
            &space,
            &canvas.id,
            &[card, PositionInput::at(idea.id, 200.0, 0.0)],
            &me(),
        )
        .unwrap();
    let mut half = PositionInput::at(idea.id, 0.0, 0.0);
    half.width = Some(1.0);
    assert_eq!(
        rpc(f.person.set_positions(&space, &canvas.id, &[half], &me())).code,
        codes::INVALID
    );

    let board = f.person.canvas_board(&space, &node).unwrap().unwrap();
    assert_eq!(board.view.name, "Launch");
    assert_eq!(board.body, "What ships when.");
    assert_eq!(board.items.len(), 2);
    assert_eq!(board.groups.len(), 1);
    assert_eq!(board.edges.len(), 1);
    assert!(f.person.canvas_board(&space, &plan.id).unwrap().is_none());
    assert_eq!(
        f.person.groups(&space, &canvas.id).unwrap(),
        std::slice::from_ref(&group)
    );
    assert_eq!(f.person.positions(&space, &canvas.id).unwrap().len(), 2);

    let names: Vec<String> = f
        .person
        .views(&space)
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(names, ["Decisions", "Launch"]);

    // A group is only removed from the canvas named.
    assert!(
        !f.person
            .remove_group(&space, &view.id, &group.id, &me())
            .unwrap()
    );
    assert!(
        f.person
            .remove_group(&space, &canvas.id, &group.id, &me())
            .unwrap()
    );
    assert_eq!(
        f.person
            .remove_positions(&space, &canvas.id, &[idea.id], &me())
            .unwrap(),
        1
    );
    assert!(f.person.remove_view(&space, &view.id, &me()).unwrap());
    assert!(!f.person.remove_view(&space, &view.id, &me()).unwrap());
    assert_eq!(
        rpc(f.person.remove_view(&space, &canvas.id, &me())).code,
        codes::INVALID
    );
}

#[test]
fn attachments_travel_as_base64_or_by_path() {
    let f = Fixture::new();
    let space = f.space();
    let other = Client::connect(f.socket()).unwrap();
    let events = other.subscribe(Some(&space)).unwrap();
    let node = f
        .person
        .create_node(&space, NewNode::note("Beds", ""), &me())
        .unwrap();
    next_change(&events);

    let bytes = b"\x89PNG\r\n\x1a\nnot really an image";
    let attachment = f
        .person
        .attach(&space, &node.id, "beds.png", bytes, &me())
        .unwrap();
    assert_eq!(attachment.media_type, "image/png");
    assert_eq!(attachment.size, bytes.len() as u64);
    let change = next_change(&events);
    assert_eq!((change.node_id, change.op), (node.id, ChangeKind::Update));

    let (found, content) = f
        .person
        .read_attachment(&space, &node.id, "beds.png")
        .unwrap()
        .unwrap();
    assert_eq!(found, attachment);
    assert_eq!(content, bytes);
    assert!(
        f.person
            .read_attachment(&space, &node.id, "none.png")
            .unwrap()
            .is_none()
    );

    let file = f.dir.path().join("plan.txt");
    std::fs::write(&file, "Plan B").unwrap();
    let from_disk = f
        .person
        .attach_file(&space, &node.id, &file, &me())
        .unwrap();
    assert_eq!(from_disk.name, "plan.txt");
    let names: Vec<String> = f
        .person
        .attachments(&space, &node.id)
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["beds.png", "plan.txt"]);
    let path = f
        .person
        .attachment_path(&space, &node.id, "plan.txt")
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Plan B");

    let raw = f.person.call(
        "attachment.add",
        json!({ "space": space, "id": node.id, "name": "x", "content": "@@@", "by": me() }),
    );
    assert_eq!(rpc(raw).code, codes::INVALID_PARAMS);
    let big = vec![0u8; MAX_INLINE_ATTACHMENT as usize + 1];
    assert_eq!(
        rpc(f.person.attach(&space, &node.id, "big.bin", &big, &me())).code,
        codes::INVALID
    );

    assert!(
        f.person
            .detach(&space, &node.id, "beds.png", &me())
            .unwrap()
    );
    assert!(
        !f.person
            .detach(&space, &node.id, "beds.png", &me())
            .unwrap()
    );
}

#[test]
fn agents_read_and_write_maps_and_attachments_by_their_grant() {
    let f = Fixture::new();
    let space = f.space();
    let node = f
        .person
        .create_node(&space, NewNode::note("Plan", ""), &me())
        .unwrap();
    let canvas = f.person.create_canvas(&space, "Board", "", &me()).unwrap();
    f.person
        .attach(&space, &node.id, "a.txt", b"hello", &me())
        .unwrap();

    let reader = f.agent("reader", GrantLevel::Read);
    assert!(!reader.link_kinds(&space).unwrap().is_empty());
    assert!(reader.node_links(&space, &node.id).unwrap().is_empty());
    assert_eq!(reader.views(&space).unwrap().len(), 1);
    assert_eq!(reader.graph(&space, None, None).unwrap().nodes.len(), 2);
    assert!(
        reader
            .canvas_board(&space, &canvas.node_id.unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(reader.attachments(&space, &node.id).unwrap().len(), 1);
    let (_, bytes) = reader
        .read_attachment(&space, &node.id, "a.txt")
        .unwrap()
        .unwrap();
    assert_eq!(bytes, b"hello");
    let refused =
        rpc(reader.set_link_kind(&space, &LinkKindSpec::symmetric("near", "near"), &me()));
    assert_eq!(refused.code, codes::FORBIDDEN);
    assert_eq!(
        rpc(reader.attach(&space, &node.id, "b.txt", b"x", &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(reader.create_canvas(&space, "Mine", "", &me())).code,
        codes::FORBIDDEN
    );

    let writer = f.agent("writer", GrantLevel::Write);
    let attached = writer
        .attach(&space, &node.id, "b.txt", b"x", &me())
        .unwrap();
    assert_eq!(attached.created_by.actor, "writer");
    let kind = writer
        .set_link_kind(&space, &LinkKindSpec::symmetric("near", "near"), &me())
        .unwrap();
    assert_eq!(kind.created_by.unwrap().actor, "writer");
    writer
        .set_positions(
            &space,
            &canvas.id,
            &[PositionInput::at(node.id, 0.0, 0.0)],
            &me(),
        )
        .unwrap();

    // Methods that name files on this machine are for people only.
    let file = f.dir.path().join("secret.txt");
    std::fs::write(&file, "secret").unwrap();
    assert_eq!(
        rpc(writer.attach_file(&space, &node.id, &file, &me())).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.attachment_path(&space, &node.id, "a.txt")).code,
        codes::FORBIDDEN
    );
    assert_eq!(
        rpc(writer.import_conversations(&space, &file, &me())).code,
        codes::FORBIDDEN
    );
}

fn export_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../knode-ingest/tests/fixtures/exports")
        .join(name)
}

#[test]
fn data_exports_import_once() {
    let f = Fixture::new();
    let space = f.space();
    let export = export_fixture("chatgpt");
    let first = f
        .person
        .import_conversations(&space, &export, &me())
        .unwrap();
    assert_eq!(first.agent, "chatgpt");
    assert_eq!(first.format, "ChatGPT export");
    assert!(first.created > 0);
    let again = f
        .person
        .import_conversations(&space, &export, &me())
        .unwrap();
    assert_eq!((again.created, again.updated), (0, 0));
    assert_eq!(again.unchanged, first.created + first.updated);

    let not_export = f.dir.path().join("notes");
    std::fs::create_dir(&not_export).unwrap();
    let refused = rpc(f.person.import_conversations(&space, &not_export, &me()));
    assert_eq!(refused.code, codes::INVALID);
    assert!(
        refused.message.contains("is not a Claude, ChatGPT"),
        "{}",
        refused.message
    );
}

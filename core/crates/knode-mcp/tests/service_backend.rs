//! The MCP tools over Knode Service: a real service on a temporary Knode home.

#![cfg(unix)]

use std::path::PathBuf;

use knode_mcp::{Backend, BackendError, ServiceBackend, tools};
use knode_service::{Client, Config, start};
use serde_json::{Map, Value, json};

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn run(backend: &ServiceBackend, name: &str, value: Value) -> tools::ToolOutput {
    tools::call(backend, "claude-code", name, args(value)).expect("a known tool")
}

fn backend_for(socket: PathBuf, space: PathBuf) -> ServiceBackend {
    ServiceBackend::new(space, move || Client::connect(&socket)).unwrap()
}

#[test]
fn the_tools_write_and_read_through_the_service() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let service = start(Config::new(&home)).unwrap();
    let socket = service.socket_path().to_path_buf();
    let space = dir.path().join("Work.knode");
    Client::connect(&socket)
        .unwrap()
        .create_space(&space, "Work")
        .unwrap();

    let watcher = Client::connect(&socket).unwrap();
    let changes = watcher.subscribe(None).unwrap();

    let backend = backend_for(socket.clone(), space.clone());
    let out = run(
        &backend,
        "write_node",
        json!({"title": "Plan", "body": "Ship v0.1, see [[Roadmap]]."}),
    );
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(out.structured.as_ref().unwrap()["result"], "created");
    // Every client of the service hears about it.
    assert!(
        changes
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_some()
    );

    // Writing the same title again updates the node found through node.find_by_title.
    let out = run(
        &backend,
        "write_node",
        json!({"title": "plan", "body": "More.", "append": true}),
    );
    assert_eq!(out.structured.as_ref().unwrap()["result"], "updated");
    run(&backend, "write_node", json!({"title": "Roadmap"}));
    let out = run(
        &backend,
        "link_nodes",
        json!({"from": "Roadmap", "to": "Plan"}),
    );
    assert!(!out.is_error, "{}", out.text);

    let out = run(&backend, "read_node", json!({"title": "Plan"}));
    assert!(out.text.contains("Ship v0.1"), "{}", out.text);
    assert!(out.text.contains("More."), "{}", out.text);
    let out = run(&backend, "search", json!({"query": "ship"}));
    assert!(out.text.contains("Plan"), "{}", out.text);
    let out = run(&backend, "recent", json!({}));
    assert!(out.text.contains("Roadmap"), "{}", out.text);

    // The writes carry the agent's name.
    let found = backend.find_by_title("Plan").unwrap();
    let node = backend.get_node(&found[0].id).unwrap().unwrap();
    assert_eq!(node.updated_by.actor, "claude-code");

    // Errors say what is wrong.
    let out = run(
        &backend,
        "read_node",
        json!({"id": "01K83N6V4W5D2XQJ0Y7RZ9T1AB"}),
    );
    assert!(out.is_error);

    // After the service restarts, the next call connects again.
    drop((changes, watcher));
    service.stop().unwrap();
    assert!(matches!(
        backend.recent(5),
        Err(BackendError::Unavailable(_))
    ));
    let service = start(Config::new(&home)).unwrap();
    assert_eq!(backend.recent(5).unwrap().len(), 2);
    service.stop().unwrap();
}

#[test]
fn a_missing_space_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let service = start(Config::new(dir.path().join("home"))).unwrap();
    let backend = backend_for(
        service.socket_path().to_path_buf(),
        dir.path().join("Missing.knode"),
    );
    match backend.recent(5) {
        Err(BackendError::Unavailable(message)) => {
            assert!(message.contains("Missing.knode"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        ServiceBackend::new("/x.knode", || Client::connect("/nonexistent/service.sock")).is_err()
    );
}

#[test]
fn history_and_undo_go_through_the_service() {
    let dir = tempfile::tempdir().unwrap();
    let service = start(Config::new(dir.path().join("home"))).unwrap();
    let socket = service.socket_path().to_path_buf();
    let space = dir.path().join("Work.knode");
    Client::connect(&socket)
        .unwrap()
        .create_space(&space, "Work")
        .unwrap();
    let backend = backend_for(socket, space);

    run(
        &backend,
        "write_node",
        json!({"title": "Plan", "body": "One."}),
    );
    run(
        &backend,
        "write_node",
        json!({"title": "Plan", "body": "Two."}),
    );
    let out = run(&backend, "history", json!({"node": "Plan"}));
    assert!(!out.is_error, "{}", out.text);
    let update = out.structured.unwrap()["changes"][0]["id"]
        .as_i64()
        .unwrap();
    let out = run(&backend, "undo", json!({"change": update}));
    assert!(!out.is_error, "{}", out.text);
    let plan = backend.find_by_title("Plan").unwrap()[0].id;
    assert_eq!(backend.get_node(&plan).unwrap().unwrap().body, "One.");

    // Errors of the service reach the agent as tool errors that say what to do.
    let out = run(&backend, "undo", json!({"change": update}));
    assert!(out.is_error);
    assert!(out.text.contains("already undone"), "{}", out.text);
    assert_eq!(backend.history(&plan).unwrap().len(), 3);
    service.stop().unwrap();
}

fn space_with_service() -> (
    tempfile::TempDir,
    knode_service::ServiceHandle,
    PathBuf,
    PathBuf,
) {
    let dir = tempfile::tempdir().unwrap();
    let service = start(Config::new(dir.path().join("home"))).unwrap();
    let socket = service.socket_path().to_path_buf();
    let space = dir.path().join("Work.knode");
    Client::connect(&socket)
        .unwrap()
        .create_space(&space, "Work")
        .unwrap();
    (dir, service, socket, space)
}

#[test]
fn a_rewrite_from_an_old_version_merges_with_what_the_person_typed() {
    let (_dir, _service, socket, space) = space_with_service();
    let backend = backend_for(socket.clone(), space.clone());
    let person = Client::connect(&socket).unwrap();
    let ada = knode_format::Provenance::person("Ada");
    let out = run(
        &backend,
        "write_node",
        json!({"title": "Plan", "body": "Ship v0.3."}),
    );
    let id = out.structured.unwrap()["node"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let node_id = knode_format::NodeId::parse(&id).unwrap();

    let read = run(&backend, "read_node", json!({"id": id}));
    let version = read.structured.unwrap()["version"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(read.text.contains("- version: `"), "{}", read.text);

    // The person types while the agent works on its rewrite.
    let open = person.open_doc(&space, &node_id).unwrap();
    let typed = knode_service::DocSplice {
        at: 0,
        delete: 0,
        insert: "Soon: ".into(),
    };
    person
        .edit_doc(&space, &node_id, Some(&open.version), &[typed], &ada)
        .unwrap();

    let out = run(
        &backend,
        "write_node",
        json!({"id": id, "body": "Ship v0.3 on Friday.", "version": version}),
    );
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("merged"), "{}", out.text);
    assert_eq!(out.structured.unwrap()["merged"], true);
    let body = |b: &ServiceBackend| b.get_node(&node_id).unwrap().unwrap().body;
    assert_eq!(body(&backend), "Soon: Ship v0.3 on Friday.");

    // Without a version, the one the agent read last is used.
    let read = run(&backend, "read_node", json!({"id": id}));
    assert!(!read.is_error);
    let open = person.open_doc(&space, &node_id).unwrap();
    let typed = knode_service::DocSplice {
        at: 0,
        delete: 0,
        insert: "Now. ".into(),
    };
    person
        .edit_doc(&space, &node_id, Some(&open.version), &[typed], &ada)
        .unwrap();
    let out = run(
        &backend,
        "write_node",
        json!({"id": id, "body": "Soon: Ship v0.3 on Friday. Then rest."}),
    );
    assert_eq!(out.structured.unwrap()["merged"], true);
    assert_eq!(body(&backend), "Now. Soon: Ship v0.3 on Friday. Then rest.");

    // Appending adds to the end of the text as it is now.
    let out = run(
        &backend,
        "write_node",
        json!({"id": id, "body": "Notes.", "append": true}),
    );
    assert_eq!(out.structured.unwrap()["merged"], false);
    assert_eq!(
        body(&backend),
        "Now. Soon: Ship v0.3 on Friday. Then rest.\n\nNotes."
    );
}

#[test]
fn two_agents_rewriting_and_a_person_typing_lose_nothing() {
    let (_dir, _service, socket, space) = space_with_service();
    let setup = backend_for(socket.clone(), space.clone());
    let out = run(
        &setup,
        "write_node",
        json!({"title": "Log", "body": "# Log"}),
    );
    let id = out.structured.unwrap()["node"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let node_id = knode_format::NodeId::parse(&id).unwrap();
    let rounds = 10;

    let agents: Vec<_> = ["claude-code", "codex"]
        .into_iter()
        .map(|name| {
            let backend = backend_for(socket.clone(), space.clone());
            let id = id.clone();
            std::thread::spawn(move || {
                for i in 0..rounds {
                    let read =
                        tools::call(&backend, name, "read_node", args(json!({"id": id}))).unwrap();
                    let text = read.structured.unwrap()["node"]["body"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    let out = tools::call(
                        &backend,
                        name,
                        "write_node",
                        args(json!({"id": id, "body": format!("{text}\n- {name} {i}")})),
                    )
                    .unwrap();
                    assert!(!out.is_error, "{}", out.text);
                }
            })
        })
        .collect();
    let person = Client::connect(&socket).unwrap();
    let ada = knode_format::Provenance::person("Ada");
    let mut base = person.open_doc(&space, &node_id).unwrap().version;
    for i in 0..rounds {
        let typed = knode_service::DocSplice {
            at: 0,
            delete: 0,
            insert: format!("p{i} "),
        };
        let edited = person
            .edit_doc(&space, &node_id, Some(&base), &[typed], &ada)
            .unwrap();
        base = edited.seen;
    }
    for agent in agents {
        agent.join().unwrap();
    }
    let body = setup.get_node(&node_id).unwrap().unwrap().body;
    for i in 0..rounds {
        assert!(body.contains(&format!("p{i} ")), "{body}");
        assert!(body.contains(&format!("- claude-code {i}")), "{body}");
        assert!(body.contains(&format!("- codex {i}")), "{body}");
    }
}

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

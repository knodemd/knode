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

/// Agents search by words and meaning through the service, which indexes in the background.
#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
#[test]
fn search_modes_through_the_service() {
    use std::time::{Duration, Instant};

    use knode_service::IndexOptions;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let model = home.join("models/multilingual-e5-small");
    std::fs::create_dir_all(&model).unwrap();
    knode_embed::test_support::write_tiny_model(&model);
    let options = IndexOptions {
        pause_on_battery: false,
        debounce: Duration::from_millis(20),
        download: false,
    };
    let service = start(Config::new(&home).with_index(options)).unwrap();
    let socket = service.socket_path().to_path_buf();
    let space = dir.path().join("Work.knode");
    let client = Client::connect(&socket).unwrap();
    client.create_space(&space, "Work").unwrap();
    let backend = backend_for(socket.clone(), space.clone());
    for (title, body) in [("Cat", "the cat sat"), ("Dog", "a dog ran far away")] {
        let out = run(
            &backend,
            "write_node",
            json!({"title": title, "body": body}),
        );
        assert!(!out.is_error, "{}", out.text);
    }

    let space = std::fs::canonicalize(&space).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = client.index_status().unwrap();
        if status
            .spaces
            .iter()
            .any(|s| s.space == space && s.nodes == 2 && s.waiting == 0 && !s.indexing)
        {
            break;
        }
        assert!(Instant::now() < deadline, "not indexed: {status:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }

    let out = run(&backend, "search", json!({"query": "cat"}));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("(by words and meaning)"), "{}", out.text);
    let found = out.structured.unwrap();
    assert_eq!(found["mode"], "hybrid");
    assert!(found.get("note").is_none(), "{found}");
    assert_eq!(found["hits"].as_array().unwrap().len(), 2);
    assert_eq!(found["hits"][0]["node"]["title"], "Cat");

    let out = run(
        &backend,
        "search",
        json!({"query": "far", "mode": "meaning"}),
    );
    let found = out.structured.unwrap();
    assert_eq!(found["mode"], "meaning");
    assert!(found["hits"][0]["word_rank"].is_null());

    let out = run(&backend, "search", json!({"query": "cat", "mode": "words"}));
    let found = out.structured.unwrap();
    assert_eq!(found["mode"], "words");
    assert_eq!(found["hits"].as_array().unwrap().len(), 1);
    drop(backend);
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

#[test]
fn canvases_kinds_and_attachments_go_through_the_service_as_the_agent() {
    use knode_format::{GrantLevel, LinkKindSpec, NewNode, Provenance};
    use knode_service::FolderInput;
    use knode_service::keys::{fingerprint, generate_key};

    let dir = tempfile::tempdir().unwrap();
    let service = start(Config::new(dir.path().join("home"))).unwrap();
    let socket = service.socket_path().to_path_buf();
    let space = std::fs::canonicalize(dir.path())
        .unwrap()
        .join("Work.knode");
    let person = Client::connect(&socket).unwrap();
    let me = Provenance::person("Ada");
    person.create_space(&space, "Work").unwrap();
    let notes = std::fs::canonicalize(dir.path()).unwrap().join("notes");
    std::fs::create_dir(&notes).unwrap();
    std::fs::write(notes.join("sketch.svg"), "<svg/>").unwrap();
    person
        .set_folder(
            &space,
            &FolderInput {
                id: None,
                kind: "notes".into(),
                path: notes.to_string_lossy().into_owned(),
                capture: true,
                rules: json!({}),
            },
            &me,
        )
        .unwrap();
    person
        .set_link_kind(
            &space,
            &LinkKindSpec::symmetric("sibling-of", "sibling of"),
            &me,
        )
        .unwrap();
    let canvas = person.create_canvas(&space, "Launch", "", &me).unwrap();
    let plan = person
        .create_node(&space, NewNode::note("Plan", ""), &me)
        .unwrap();
    person
        .set_positions(
            &space,
            &canvas.id,
            &[knode_service::PositionInput::at(plan.id, 0.0, 0.0)],
            &me,
        )
        .unwrap();

    let key = generate_key().unwrap();
    person
        .add_agent("claude-code", "claude-code", &fingerprint(&key), &me)
        .unwrap();
    person
        .set_grant("claude-code", &space, GrantLevel::Write, &me)
        .unwrap();
    let agent = {
        let socket = socket.clone();
        ServiceBackend::new(space.clone(), move || {
            Client::connect_as_agent(&socket, &key)
        })
        .unwrap()
    };

    let out = run(&agent, "link_kinds", json!({}));
    assert!(
        out.text.contains("`sibling-of`: sibling of (both ways)"),
        "{}",
        out.text
    );
    let out = run(&agent, "read_canvas", json!({ "canvas": "Launch" }));
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("[[Plan]] (note"), "{}", out.text);

    let path = notes.join("sketch.svg");
    let out = run(
        &agent,
        "attach_file",
        json!({ "node": "Plan", "path": path.to_string_lossy() }),
    );
    assert!(!out.is_error, "{}", out.text);
    let attachment = &out.structured.unwrap()["attachment"];
    assert_eq!(attachment["created_by"]["actor"], "claude-code");
    let out = run(
        &agent,
        "read_attachment",
        json!({ "node": "Plan", "name": "sketch.svg" }),
    );
    assert_eq!(out.structured.unwrap()["text"], "<svg/>");
    // The agent never names a file to the service: attaching by path is for people.
    let refused = agent.attach_file(&plan.id, &path, &me);
    assert!(
        matches!(refused, Err(BackendError::Invalid(_))),
        "{refused:?}"
    );
    service.stop().unwrap();
}

//! Integration tests: a real service on a temporary Knode home, with real clients.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{NewNode, NodeUpdate, Provenance};
use knode_service::protocol::codes;
use knode_service::{
    ChangeKind, Client, Config, Error, Event, ServiceHandle, Subscription, connect_or_spawn_with,
    start, stop_service,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: TempDir,
    service: Option<ServiceHandle>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let service = start(Config::new(dir.path().join("home"))).unwrap();
        Fixture {
            dir,
            service: Some(service),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn socket(&self) -> PathBuf {
        self.home().join("service.sock")
    }

    fn client(&self) -> Client {
        Client::connect(self.socket()).unwrap()
    }

    fn space_path(&self, name: &str) -> PathBuf {
        self.dir.path().join(format!("{name}.knode"))
    }

    fn stop(&mut self) {
        self.service.take().unwrap().stop().unwrap();
    }
}

fn changes(subscription: &Subscription, count: usize) -> Vec<Event> {
    let deadline = Instant::now() + WAIT;
    let mut events = Vec::new();
    while events.len() < count {
        let left = deadline.saturating_duration_since(Instant::now());
        match subscription.recv_timeout(left) {
            Some(event) => events.push(event),
            None => panic!("got {} of {count} events", events.len()),
        }
    }
    events
}

fn rpc_code(result: Result<impl std::fmt::Debug, Error>) -> (i64, Option<String>) {
    match result {
        Err(Error::Rpc(e)) => (e.code, e.kind().map(str::to_string)),
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

#[test]
fn many_clients_write_at_once_through_one_writer() {
    let fixture = Fixture::new();
    let admin = fixture.client();
    let space = admin
        .create_space(fixture.space_path("Shared"), "Shared")
        .unwrap()
        .path;
    let watcher = fixture.client();
    let events = watcher.subscribe(Some(&space)).unwrap();

    const WRITERS: usize = 8;
    const NODES: usize = 25;
    let shared = admin
        .create_node(
            &space,
            NewNode::note("Shared", "start"),
            &Provenance::system(),
        )
        .unwrap();
    let space = Arc::new(space);
    let threads: Vec<_> = (0..WRITERS)
        .map(|w| {
            let space = space.clone();
            let socket = fixture.socket();
            thread::spawn(move || {
                let client = Client::connect(socket).unwrap();
                let by = Provenance::agent(format!("agent-{w}"));
                let mut ids = Vec::new();
                for n in 0..NODES {
                    let node = client
                        .create_node(&*space, NewNode::note(format!("Note {w}-{n}"), "text"), &by)
                        .unwrap();
                    ids.push(node.id);
                    let update = NodeUpdate {
                        body: Some(format!("written by {w}, step {n}")),
                        ..Default::default()
                    };
                    client
                        .update_node(&*space, &shared.id, update, &by)
                        .unwrap();
                }
                ids
            })
        })
        .collect();
    let created: Vec<_> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();

    // Every node is there, once.
    let all = admin.recent(&*space, 1000).unwrap();
    assert_eq!(all.len(), WRITERS * NODES + 1);
    let unique: HashSet<_> = created.iter().collect();
    assert_eq!(unique.len(), WRITERS * NODES);

    // The shared node kept every version, in one order with no gaps or duplicates.
    let history = admin.history(&*space, &shared.id).unwrap();
    assert_eq!(history.len(), 1 + WRITERS * NODES);
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));
    let bodies: HashSet<_> = history.iter().map(|h| h.body.clone()).collect();
    assert_eq!(bodies.len(), 1 + WRITERS * NODES);

    // The subscriber heard about every write, in the order they were made.
    let total = 1 + 2 * WRITERS * NODES;
    let events = changes(&events, total);
    let mut heard = HashSet::new();
    let mut updates = 0;
    for event in &events {
        let Event::Changed(change) = event else {
            panic!("unexpected {event:?}")
        };
        assert_eq!(change.space, *space);
        match change.op {
            ChangeKind::Create => {
                heard.insert(change.node_id);
            }
            ChangeKind::Update => {
                assert_eq!(change.node_id, shared.id);
                updates += 1;
            }
            op => panic!("unexpected {op:?}"),
        }
    }
    assert_eq!(updates, WRITERS * NODES);
    assert!(created.iter().all(|id| heard.contains(id)));
}

#[test]
fn every_subscriber_hears_each_write() {
    let fixture = Fixture::new();
    let writer = fixture.client();
    let a = writer
        .create_space(fixture.space_path("A"), "A")
        .unwrap()
        .path;
    let b = writer
        .create_space(fixture.space_path("B"), "B")
        .unwrap()
        .path;
    let me = Provenance::person("Ada");

    let everything = fixture.client();
    let all = everything.subscribe(None).unwrap();
    let only_a_client = fixture.client();
    let only_a = only_a_client.subscribe(Some(&a)).unwrap();

    let in_b = writer
        .create_node(&b, NewNode::note("In B", ""), &me)
        .unwrap();
    let target = writer
        .create_node(&a, NewNode::note("Target", ""), &me)
        .unwrap();
    let source = writer
        .create_node(&a, NewNode::note("Source", "See [[Target]]."), &me)
        .unwrap();
    writer
        .link(&a, &source.id, &target.id, "related", &me)
        .unwrap();
    assert!(
        writer
            .unlink(&a, &source.id, &target.id, "related", &me)
            .unwrap()
    );
    // Renaming the target rewrites the source's body: both change.
    let rename = NodeUpdate {
        title: Some("Goal".into()),
        ..Default::default()
    };
    writer.update_node(&a, &target.id, rename, &me).unwrap();
    writer.delete_node(&a, &source.id, &me).unwrap();

    let expected_a = vec![
        (target.id, ChangeKind::Create),
        (source.id, ChangeKind::Create),
        (source.id, ChangeKind::Link),
        (source.id, ChangeKind::Unlink),
        (target.id, ChangeKind::Update),
        (source.id, ChangeKind::Update),
        (source.id, ChangeKind::Delete),
    ];
    let pairs = |events: Vec<Event>| -> Vec<_> {
        events
            .into_iter()
            .map(|e| match e {
                Event::Changed(c) => (c.space, c.node_id, c.op),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    };
    let seen_a = pairs(changes(&only_a, expected_a.len()));
    assert!(seen_a.iter().all(|(space, _, _)| *space == a));
    let seen_a: Vec<_> = seen_a.into_iter().map(|(_, id, op)| (id, op)).collect();
    assert_eq!(seen_a, expected_a);
    assert!(only_a.recv_timeout(Duration::from_millis(200)).is_none());

    let seen_all = pairs(changes(&all, expected_a.len() + 1));
    assert_eq!(seen_all[0], (b.clone(), in_b.id, ChangeKind::Create));

    // The new body of the source was written by the rename.
    let history = writer.history(&a, &source.id).unwrap();
    assert!(history.iter().any(|h| h.body == "See [[Goal]]."));
}

#[test]
fn errors_have_codes_and_clear_messages() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let space = client
        .create_space(fixture.space_path("Errors"), "Errors")
        .unwrap()
        .path;
    let me = Provenance::person("Ada");
    let node = client
        .create_node(&space, NewNode::note("Here", ""), &me)
        .unwrap();
    client.delete_node(&space, &node.id, &me).unwrap();

    assert_eq!(client.get_node(&space, &node.id).unwrap(), None);
    let (code, kind) = rpc_code(client.update_node(&space, &node.id, NodeUpdate::default(), &me));
    assert_eq!(
        (code, kind.as_deref()),
        (codes::NOT_FOUND, Some("not_found"))
    );
    let (code, _) = rpc_code(client.open_space(fixture.space_path("Missing")));
    assert_eq!(code, codes::NOT_A_SPACE);
    let (code, _) = rpc_code(client.create_space(&space, "Again"));
    assert_eq!(code, codes::ALREADY_EXISTS);
    let (code, _) = rpc_code(client.create_node(&space, NewNode::note(" ", ""), &me));
    assert_eq!(code, codes::INVALID);
    let (code, _) = rpc_code(client.call("node.get", json!({ "space": "Relative.knode" })));
    assert_eq!(code, codes::INVALID_PARAMS);
    let (code, _) = rpc_code(client.call("node.get", json!({ "space": space, "id": "not an id" })));
    assert_eq!(code, codes::INVALID_PARAMS);
    let err = client
        .call(
            "node.create",
            json!({ "space": space, "title": "No provenance" }),
        )
        .unwrap_err();
    assert!(err.to_string().contains("by"), "{err}");
    let (code, _) = rpc_code(client.call("node.explode", json!({})));
    assert_eq!(code, codes::METHOD_NOT_FOUND);

    // Raw protocol errors, straight on the socket.
    let mut raw = UnixStream::connect(fixture.socket()).unwrap();
    let mut lines = BufReader::new(raw.try_clone().unwrap()).lines();
    let mut ask = |line: &str| -> Value {
        writeln!(raw, "{line}").unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    let parse = ask("{not json");
    assert_eq!(parse["error"]["code"], codes::PARSE_ERROR);
    assert_eq!(parse["id"], Value::Null);
    let batch = ask("[]");
    assert_eq!(batch["error"]["code"], codes::INVALID_REQUEST);
    // Nothing but `auth` is answered before the connection authenticates.
    let refused = ask(r#"{"jsonrpc":"2.0","id":"s","method":"service.status"}"#);
    assert_eq!(refused["error"]["code"], codes::UNAUTHENTICATED);
    assert_eq!(refused["error"]["data"]["kind"], "unauthenticated");
    let token = std::fs::read_to_string(fixture.home().join("owner.token")).unwrap();
    let wrong = ask(r#"{"jsonrpc":"2.0","id":"a","method":"auth","params":{"owner":"nope"}}"#);
    assert_eq!(wrong["error"]["code"], codes::UNAUTHENTICATED);
    let me = ask(&format!(
        r#"{{"jsonrpc":"2.0","id":"a","method":"auth","params":{{"owner":"{}"}}}}"#,
        token.trim()
    ));
    assert_eq!(me["result"], json!({ "as": "person" }));
    let status = ask(r#"{"jsonrpc":"2.0","id":"s","method":"service.status"}"#);
    assert_eq!(status["id"], "s");
    assert_eq!(status["result"]["pid"], std::process::id());
}

#[test]
fn only_one_service_runs_per_home() {
    let mut fixture = Fixture::new();
    let socket = fixture.socket();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    match start(Config::new(fixture.home())) {
        Err(Error::AlreadyRunning { pid }) => assert_eq!(pid, Some(std::process::id())),
        other => panic!("a second service started: {other:?}"),
    }
    // The first service still answers.
    assert_eq!(fixture.client().status().unwrap().clients, 1);

    // Stopping removes the socket and releases the lock.
    let client = fixture.client();
    let events = client.subscribe(None).unwrap();
    fixture.stop();
    assert!(!socket.exists());
    assert!(events.recv_timeout(WAIT).is_none());
    assert!(matches!(
        client.status(),
        Err(Error::Disconnected) | Err(Error::Io(_))
    ));

    // A stale socket left behind does not stop the next service.
    std::fs::write(&socket, "stale").unwrap();
    let again = start(Config::new(fixture.home())).unwrap();
    assert!(Client::connect(&socket).unwrap().status().is_ok());
    again.stop().unwrap();
}

#[test]
fn status_lists_open_spaces() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let path = fixture.space_path("Status");
    client.create_space(&path, "Status").unwrap();
    let other = fixture.client();
    let info = other.open_space(&path).unwrap();
    assert_eq!(info.name, "Status");
    assert_eq!(info.nodes, 0);
    let status = other.status().unwrap();
    assert_eq!(status.spaces, vec![std::fs::canonicalize(&path).unwrap()]);
    assert_eq!(status.clients, 2);
    assert_eq!(status.version, knode_service::version());
}

/// Run by `connect_or_spawn_starts_the_service` in a child process: the service itself.
#[test]
#[ignore = "runs only as the child of connect_or_spawn_starts_the_service"]
fn spawned_service_child() {
    if std::env::var_os("KNODE_SERVICE_TEST_CHILD").is_some() {
        knode_service::run(Config::from_env().unwrap()).unwrap();
    }
}

#[test]
fn connect_or_spawn_starts_the_service() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let socket = home.join("service.sock");
    let command = || {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["spawned_service_child", "--exact", "--ignored"])
            .env("KNODE_HOME", &home)
            .env("KNODE_SERVICE_TEST_CHILD", "1");
        command
    };
    let client = connect_or_spawn_with(&socket, command(), WAIT).unwrap();
    let pid = client.status().unwrap().pid;
    assert_ne!(pid, std::process::id());

    // Already running: connects to the same service.
    let second = connect_or_spawn_with(&socket, command(), WAIT).unwrap();
    assert_eq!(second.status().unwrap().pid, pid);

    // stop_service stops it gracefully: the socket goes away and the lock is free.
    drop((client, second));
    assert_eq!(stop_service(&socket, WAIT).unwrap(), Some(pid));
    assert!(!socket.exists());
    assert_eq!(stop_service(&socket, WAIT).unwrap(), None);
    // So a new one starts at once.
    let again = start(Config::new(&home)).unwrap();
    again.stop().unwrap();
}

#[test]
fn connect_or_spawn_gives_up_when_nothing_listens() {
    let dir = tempfile::tempdir().unwrap();
    let socket: &Path = &dir.path().join("service.sock");
    let err = connect_or_spawn_with(socket, Command::new("true"), Duration::from_millis(300))
        .unwrap_err();
    assert!(matches!(err, Error::StartTimeout { .. }), "{err}");
}

#[test]
fn finds_nodes_by_title_and_lists_unresolved_links() {
    let fixture = Fixture::new();
    let client = fixture.client();
    let path = fixture.space_path("Titles");
    client.create_space(&path, "Titles").unwrap();
    let me = Provenance::person("tester");
    let first = client
        .create_node(
            &path,
            NewNode::note("Plan", "See [[Roadmap]] and [[Ideas]]."),
            &me,
        )
        .unwrap();
    let second = client
        .create_node(&path, NewNode::note("plan", "Another one."), &me)
        .unwrap();
    client
        .create_node(&path, NewNode::note("Ideas", ""), &me)
        .unwrap();

    let found = client.find_by_title(&path, "  PLAN ").unwrap();
    let ids: Vec<_> = found.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![first.id, second.id]);
    assert!(client.find_by_title(&path, "Nothing").unwrap().is_empty());

    assert_eq!(
        client.unresolved_links(&path, &first.id).unwrap(),
        vec!["Roadmap".to_string()]
    );
    assert!(
        client
            .unresolved_links(&path, &second.id)
            .unwrap()
            .is_empty()
    );

    // Over the wire, with the documented names.
    let raw = client
        .call(
            "node.find_by_title",
            json!({ "space": path, "title": "ideas" }),
        )
        .unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 1);
    assert_eq!(raw[0]["title"], "Ideas");
    let (code, kind) = rpc_code(client.call("node.find_by_title", json!({ "space": path })));
    assert_eq!(code, codes::INVALID_PARAMS);
    assert_eq!(kind.as_deref(), Some("invalid_params"));
}

#[test]
fn connect_or_spawn_stops_waiting_when_the_service_exits() {
    let dir = tempfile::tempdir().unwrap();
    let socket: &Path = &dir.path().join("service.sock");
    let started = Instant::now();
    let err = connect_or_spawn_with(socket, Command::new("false"), WAIT).unwrap_err();
    assert!(matches!(err, Error::StartFailed { .. }), "{err}");
    assert!(started.elapsed() < WAIT);
}

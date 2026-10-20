//! `knode mcp`, driven by a real client over standard input and output.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::Sandbox;
use serde_json::{Value, json};

/// An MCP client speaking JSON-RPC, one message per line, to a `knode mcp` child process.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl Client {
    fn start(sb: &Sandbox, extra: &[&str]) -> Client {
        Client::start_with_env(sb, extra, &[])
    }

    fn start_with_env(sb: &Sandbox, extra: &[&str], env: &[(&str, &str)]) -> Client {
        let mut child = sb
            .cmd()
            .arg("mcp")
            .args(extra)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(sb.path("mcp-stderr.log")).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Client {
            child,
            stdin,
            lines,
            next_id: 1,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(10))
                .expect("a response from knode mcp");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout must be JSON-RPC only ({e}): {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "{message}");
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    fn initialize(&mut self, version: &str) -> Value {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0.0" }
            }),
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        response
    }

    /// Calls a tool and returns its result, failing on a protocol error.
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    /// Closes standard input and waits for the server to exit.
    fn finish(mut self) {
        drop(self.stdin.take());
        for _ in 0..100 {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "knode mcp exited with {status}");
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("knode mcp did not exit when its input closed");
    }
}

fn text(result: &Value) -> &str {
    let content = result["content"].as_array().expect("content");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().unwrap()
}

#[test]
fn a_client_uses_every_tool() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let agent = sb.connect_agent();
    let mut client = Client::start(&sb, &["--agent", agent]);

    let init = client.initialize("2025-06-18");
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(result["serverInfo"]["name"], "knode");
    assert!(result["capabilities"]["tools"].is_object(), "{init}");
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("write_node")
    );

    let tools = client.request("tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "search",
            "read_node",
            "write_node",
            "link_nodes",
            "recent",
            "history",
            "undo",
            "link_kinds",
            "read_canvas",
            "attach_file",
            "read_attachment"
        ]
    );
    for tool in &tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().unwrap().len() > 80);
    }

    // write_node creates, as the client that connected.
    let result = client.call(
        "write_node",
        json!({ "title": "Pricing decision", "body": "We charge **per seat**.", "kind": "decision", "tags": ["business"] }),
    );
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["result"], "created");
    let pricing = result["structuredContent"]["node"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text(&result).starts_with("Created decision [[Pricing decision]]"));

    let result = client.call(
        "write_node",
        json!({ "title": "Roadmap", "body": "Ship v0.1. See [[Pricing decision]]." }),
    );
    let roadmap = result["structuredContent"]["node"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // search
    let result = client.call("search", json!({ "query": "seat", "limit": 5 }));
    let hits = result["structuredContent"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{result}");
    assert_eq!(hits[0]["node"]["id"], pricing.as_str());
    assert!(text(&result).contains("**Pricing decision** (decision"));

    // read_node, by id and by title
    let result = client.call("read_node", json!({ "id": pricing }));
    let node = &result["structuredContent"]["node"];
    assert_eq!(node["body"], "We charge **per seat**.");
    assert_eq!(node["tags"], json!(["business"]));
    assert_eq!(
        node["created_by"],
        json!({ "kind": "agent", "actor": "gemini-cli" })
    );
    assert_eq!(
        result["structuredContent"]["backlinks"][0]["from"],
        roadmap.as_str()
    );
    assert!(text(&result).starts_with("# Pricing decision\n\nWe charge **per seat**."));
    let result = client.call("read_node", json!({ "title": "roadmap" }));
    assert_eq!(result["structuredContent"]["node"]["id"], roadmap.as_str());

    // link_nodes, by title and id
    let result = client.call(
        "link_nodes",
        json!({ "from": "Roadmap", "to": pricing, "kind": "part-of" }),
    );
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["link"]["kind"], "part-of");
    assert_eq!(
        result["structuredContent"]["link"]["created_by"]["actor"],
        "gemini-cli"
    );

    // write_node updates by id, appending
    let result = client.call(
        "write_node",
        json!({ "id": pricing, "body": "Teams get a discount.", "append": true }),
    );
    assert_eq!(result["structuredContent"]["result"], "updated");

    // recent
    let result = client.call("recent", json!({}));
    let nodes = result["structuredContent"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["id"], pricing.as_str());
    // Recorded under the name of the agent its key belongs to, not the client's own name.
    assert_eq!(nodes[0]["updated_by"]["actor"], "gemini-cli");

    // history and undo: the agent takes back a note of its own.
    client.call(
        "write_node",
        json!({ "title": "Scratch", "body": "Not needed." }),
    );
    let result = client.call("history", json!({ "limit": 1 }));
    assert_eq!(result["isError"], false, "{result}");
    let change = &result["structuredContent"]["changes"][0];
    assert_eq!(change["subject_title"], "Scratch");
    let result = client.call("undo", json!({ "change": change["id"] }));
    assert_eq!(result["isError"], false, "{result}");
    assert!(
        text(&result).contains("the node it created was deleted"),
        "{result}"
    );
    let result = client.call("history", json!({ "node": pricing }));
    assert_eq!(
        result["structuredContent"]["changes"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{result}"
    );

    // A tool error is a result the model can read, not a protocol error.
    let result = client.call("read_node", json!({ "title": "Nothing like this" }));
    assert_eq!(result["isError"], true);
    assert!(text(&result).contains("Use search"));

    // An unknown tool is a protocol error.
    let response = client.request("tools/call", json!({ "name": "nope", "arguments": {} }));
    assert_eq!(response["error"]["code"], -32602, "{response}");

    client.finish();

    // The writes are in the space, made by the agent.
    let space = knode_format::Space::open(sb.space("work")).unwrap();
    let id = knode_format::NodeId::parse(&pricing).unwrap();
    let node = space.get_node(&id).unwrap().unwrap();
    assert_eq!(
        node.body,
        "We charge **per seat**.\n\nTeams get a discount."
    );
    assert_eq!(
        node.updated_by,
        knode_format::Provenance::agent("gemini-cli")
    );
}

#[test]
fn negotiates_a_newer_protocol_version() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let mut client = Client::start(&sb, &[]);
    let init = client.initialize("2025-11-25");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    let tools = client.request("tools/list", json!({}));
    assert_eq!(
        tools["result"]["tools"].as_array().unwrap().len(),
        knode_mcp::tools::TOOL_NAMES.len()
    );
    client.finish();
}

#[test]
fn without_a_space_the_tools_say_what_to_do() {
    let sb = Sandbox::new();
    let mut client = Client::start(&sb, &[]);
    client.initialize("2025-06-18");
    let result = client.call("recent", json!({}));
    assert_eq!(result["isError"], true);
    assert!(text(&result).contains("knode init <name>"), "{result}");
    client.finish();
}

#[test]
fn the_space_flag_picks_the_space() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "other"]);
    let other = sb.space("other");
    let agent = sb.connect_agent();
    sb.ok(&["agents", "grant", agent, "other", "write"]);
    let mut client = Client::start(&sb, &["--agent", agent, "--space", other.to_str().unwrap()]);
    client.initialize("2025-06-18");
    let result = client.call("write_node", json!({ "title": "In other" }));
    assert_eq!(result["isError"], false, "{result}");
    client.finish();
    let space = knode_format::Space::open(&other).unwrap();
    assert_eq!(space.node_count().unwrap(), 1);
    let work = knode_format::Space::open(sb.space("work")).unwrap();
    assert_eq!(work.node_count().unwrap(), 0);
}

#[test]
fn the_tools_go_through_knode_service() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let agent = sb.connect_agent();
    // knode mcp starts the service again by itself.
    sb.ok(&["service", "stop"]);
    let mut client = Client::start(&sb, &["--agent", agent]);
    client.initialize("2025-06-18");
    let result = client.call(
        "write_node",
        json!({ "title": "Through the service", "body": "One writer for everyone." }),
    );
    assert_eq!(result["isError"], false, "{result}");

    // The service has the space open and the MCP server as its client.
    let status = sb.ok(&["service", "status"]);
    assert!(status.contains("is running"), "{status}");
    let space = std::fs::canonicalize(sb.space("work")).unwrap();
    assert!(
        status.contains(&format!("  {}", space.display())),
        "{status}"
    );
    assert!(status.contains("Clients: 1"), "{status}");

    // A write from the agent is seen by the CLI at once, and the other way round.
    let found = sb.ok(&["search", "writer"]);
    assert!(found.contains("Through the service"), "{found}");
    std::fs::write(
        sb.path("note.md"),
        "# From the CLI\n\nImported while the agent works.\n",
    )
    .unwrap();
    sb.ok(&["import", sb.path("note.md").to_str().unwrap()]);
    let result = client.call("search", json!({ "query": "imported" }));
    assert!(text(&result).contains("From the CLI"), "{result}");
    let result = client.call("read_node", json!({ "title": "Through the service" }));
    assert!(
        text(&result).contains("One writer for everyone."),
        "{result}"
    );
    client.finish();

    let stderr = std::fs::read_to_string(sb.path("mcp-stderr.log")).unwrap();
    assert!(!stderr.contains("warning"), "{stderr}");
    let space = knode_format::Space::open(sb.space("work")).unwrap();
    let node = &space.find_by_title("Through the service").unwrap()[0];
    let node = space.get_node(&node.id).unwrap().unwrap();
    assert_eq!(node.created_by.actor, "gemini-cli");
}

#[test]
#[cfg(unix)]
fn without_the_service_agents_reach_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let agent = sb.connect_agent();
    sb.ok(&["service", "stop"]);
    // The service cannot make its socket in a Knode home it cannot write to.
    let home = sb.knode_home();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut client = Client::start(&sb, &["--agent", agent]);
    client.initialize("2025-06-18");
    let result = client.call("write_node", json!({ "title": "Direct" }));
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        text(&result).contains("Knode Service cannot run"),
        "{result}"
    );
    client.finish();
    // An agent never opens a space by itself, so nothing bypassed its grants.
    let space = knode_format::Space::open(sb.space("work")).unwrap();
    assert_eq!(space.node_count().unwrap(), 0);
}

#[test]
fn an_agent_set_up_without_a_key_is_told_to_connect_again() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    // As `knode connect` set agents up before keys: `knode mcp` and nothing else.
    let mut client = Client::start(&sb, &[]);
    client.initialize("2025-06-18");
    let result = client.call("recent", json!({}));
    assert_eq!(result["isError"], true);
    assert!(text(&result).contains("its own key"), "{result}");
    assert!(text(&result).contains("knode connect"), "{result}");
    client.finish();

    // An agent name whose key is not on this machine.
    let mut client = Client::start(&sb, &["--agent", "codex"]);
    client.initialize("2025-06-18");
    let result = client.call("recent", json!({}));
    assert!(text(&result).contains("knode connect codex"), "{result}");
    client.finish();

    // A key the service does not know.
    let mut child = Client::start_with_env(
        &sb,
        &[],
        &[(
            "KNODE_AGENT_KEY",
            "knk_0000000000000000000000000000000000000000000000000000000000000000",
        )],
    );
    child.initialize("2025-06-18");
    let result = child.call("recent", json!({}));
    assert!(
        text(&result).contains("Knode refused this agent"),
        "{result}"
    );
    child.finish();
}

#[test]
fn grants_and_revocation_reach_the_tools() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "personal"]);
    let agent = sb.connect_agent();

    // Write on the default space only: the personal space is invisible.
    let personal = sb.space("personal");
    let mut client = Client::start(
        &sb,
        &["--agent", agent, "--space", personal.to_str().unwrap()],
    );
    client.initialize("2025-06-18");
    let result = client.call("recent", json!({}));
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        text(&result).contains("cannot open the Knode space"),
        "{result}"
    );
    client.finish();

    // Read only: reading works, writing does not.
    sb.ok(&["agents", "grant", "gemini", "work", "read"]);
    let mut client = Client::start(&sb, &["--agent", agent]);
    client.initialize("2025-06-18");
    let result = client.call("recent", json!({}));
    assert_eq!(result["isError"], false, "{result}");
    let result = client.call("write_node", json!({ "title": "Nope" }));
    assert_eq!(result["isError"], true, "{result}");
    assert!(text(&result).contains("only read"), "{result}");

    // Revoked: the next call of the running server is refused.
    sb.ok(&["agents", "revoke", "gemini"]);
    let result = client.call("recent", json!({}));
    assert_eq!(result["isError"], true, "{result}");
    assert!(text(&result).contains("knode connect"), "{result}");
    client.finish();
}

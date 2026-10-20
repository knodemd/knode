//! The `knode` command, run as a person would, in a sandbox.

mod common;

use std::fs;

use common::{Sandbox, ingest_fixture};
use serde_json::Value;

#[test]
fn prints_its_version() {
    let sb = Sandbox::new();
    let out = sb.ok(&["--version"]);
    assert_eq!(out.trim(), format!("knode {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn init_creates_a_space_and_sets_the_default_once() {
    let sb = Sandbox::new();
    let out = sb.ok(&["init", "work"]);
    assert!(out.contains("Created space \"work\""), "{out}");
    assert!(out.contains("It is now your default space."), "{out}");
    assert!(sb.space("work").join("manifest.json").is_file());
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(config.contains("work.knode"), "{config}");

    let out = sb.ok(&["init", "personal"]);
    assert!(out.contains("Your default space is still"), "{out}");
    // Still the default, and known: listed among the spaces.
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(config.starts_with("default_space = "), "{config}");
    assert!(config.contains("work.knode"), "{config}");
    assert!(config.contains("spaces = ["), "{config}");
    assert!(config.contains("personal.knode"), "{config}");

    let dir = sb.path("elsewhere");
    let out = sb.ok(&["init", "side", "--dir", dir.to_str().unwrap(), "--default"]);
    assert!(out.contains("It is now your default space."), "{out}");
    assert!(dir.join("side.knode").is_dir());

    let err = sb.fails(&["init", "work"]);
    assert!(err.contains("already exists"), "{err}");
    let err = sb.fails(&["init", "a/b"]);
    assert!(err.contains("cannot be a space name"), "{err}");

    let out = sb.ok(&["spaces"]);
    let line = |name: &str| {
        out.lines()
            .find(|l| l.contains(&format!("{name}.knode")))
            .unwrap_or_else(|| panic!("no {name} in {out}"))
            .to_string()
    };
    assert!(line("side").starts_with("* "), "{out}");
    assert!(line("side").ends_with("(default)"), "{out}");
    assert!(line("work").starts_with("    work "), "{out}");
    assert!(line("personal").starts_with("    personal "), "{out}");
}

#[test]
fn spaces_without_any() {
    let sb = Sandbox::new();
    assert!(sb.ok(&["spaces"]).contains("No spaces yet"));
}

#[test]
fn commands_without_a_space_say_how_to_make_one() {
    let sb = Sandbox::new();
    let err = sb.fails(&["search", "x"]);
    assert!(err.starts_with("error: no space to use"), "{err}");
    assert!(err.contains("knode init <name>"), "{err}");

    let err = sb.fails(&["search", "x", "--space", "/nowhere/x.knode"]);
    assert!(
        err.contains("there is no space at /nowhere/x.knode"),
        "{err}"
    );

    let err = sb
        .cmd()
        .args(["search", "x"])
        .env("KNODE_SPACE", "/nowhere/y.knode")
        .output()
        .unwrap();
    let err = String::from_utf8(err.stderr).unwrap();
    assert!(err.contains("(from KNODE_SPACE)"), "{err}");
}

#[test]
fn imports_a_folder_of_markdown_and_searches_it() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let notes = sb.path("notes");
    fs::create_dir_all(notes.join("sub")).unwrap();
    fs::create_dir_all(notes.join(".obsidian")).unwrap();
    fs::write(
        notes.join("pricing.md"),
        "# Pricing decision\n\nWe charge per seat.\n",
    )
    .unwrap();
    fs::write(notes.join("sub/ideas.md"), "No heading, about seats too.\n").unwrap();
    fs::write(notes.join(".obsidian/hidden.md"), "# Hidden\n").unwrap();
    fs::write(notes.join("image.png"), [0u8, 1, 2]).unwrap();

    let out = sb.ok(&["import", notes.to_str().unwrap()]);
    let dir = notes.display();
    assert_eq!(
        out.trim(),
        format!(
            "Imported {dir}: 2 new, 0 updated, 0 unchanged.\nLeft out 1 file that is not Markdown, such as images."
        )
    );
    let out = sb.ok(&["import", notes.to_str().unwrap(), "--vault"]);
    assert!(
        out.starts_with(&format!(
            "Imported the vault {dir}: 0 new, 0 updated, 2 unchanged."
        )),
        "{out}"
    );

    // Titles of a folder come from the file names, as in Obsidian.
    let out = sb.ok(&["search", "seat"]);
    assert!(out.contains("pricing  (note, "), "{out}");
    assert!(out.contains("by tester)"), "{out}");
    assert!(out.contains("ideas  (note, "), "{out}");

    let out = sb.ok(&["search", "per", "seat", "--json", "-n", "5"]);
    let found: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(found["mode"], "words");
    // Off in the sandbox, or not built on this platform: either way the note says why.
    assert!(found["note"].is_string(), "{found}");
    let hits = found["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["node"]["title"], "pricing");
    assert_eq!(hits[0]["node"]["updated_by"]["kind"], "person");
    assert_eq!(hits[0]["node"]["updated_by"]["actor"], "tester");

    assert!(sb.ok(&["search", "zebra"]).contains("Nothing matches"));

    // One file keeps its first heading as its title.
    let single = sb.path("single.md");
    fs::write(&single, "# Single note\n\nAlone.\n").unwrap();
    assert_eq!(
        sb.ok(&["import", single.to_str().unwrap()]).trim(),
        "Imported 1 note."
    );
    let err = sb.fails(&["import", single.to_str().unwrap(), "--vault"]);
    assert!(err.contains("not a folder"), "{err}");

    let err = sb.fails(&["import", sb.path("missing").to_str().unwrap()]);
    assert!(err.contains("does not exist"), "{err}");
    fs::write(sb.path("data.csv"), "a,b").unwrap();
    let err = sb.fails(&["import", sb.path("data.csv").to_str().unwrap()]);
    assert!(
        err.contains("not a data export, a Markdown file or a session"),
        "{err}"
    );
}

#[test]
fn imports_agent_sessions_as_conversations() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let claude = ingest_fixture(
        "claude/projects/-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666.jsonl",
    );
    let out = sb.ok(&["import", claude.to_str().unwrap()]);
    assert!(
        out.starts_with("Imported the Claude Code conversation"),
        "{out}"
    );
    let out = sb.ok(&["import", claude.to_str().unwrap()]);
    assert!(out.contains("already imported and unchanged"), "{out}");

    let codex = ingest_fixture(
        "codex/sessions/2026/01/02/rollout-2026-01-02T11-00-00-0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee.jsonl",
    );
    let out = sb.ok(&["import", codex.to_str().unwrap()]);
    assert!(out.starts_with("Imported the Codex conversation"), "{out}");

    // Both are conversations written as their agent.
    let space = knode_format::Space::open(sb.space("work")).unwrap();
    let recent = space.recent(10).unwrap();
    assert_eq!(recent.len(), 2);
    for node in &recent {
        assert_eq!(node.kind, "conversation");
        assert_eq!(node.updated_by.kind, knode_format::ActorKind::Agent);
    }
    let actors: Vec<&str> = recent.iter().map(|n| n.updated_by.actor.as_str()).collect();
    assert!(actors.contains(&"claude-code") && actors.contains(&"codex"));
    let node = space.get_node(&recent[0].id).unwrap().unwrap();
    assert!(node.updated_by.session.is_some());
    assert!(!node.body.starts_with("# "), "{}", node.body);

    fs::write(sb.path("other.jsonl"), "{\"hello\": 1}\n").unwrap();
    let err = sb.fails(&["import", sb.path("other.jsonl").to_str().unwrap()]);
    assert!(err.contains("not a Claude Code or Codex session"), "{err}");
}

#[test]
fn space_flag_and_env_pick_the_space() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "other"]);
    let note = sb.path("note.md");
    fs::write(&note, "# Only in other\n").unwrap();
    let other = sb.space("other");
    sb.ok(&[
        "import",
        note.to_str().unwrap(),
        "--space",
        other.to_str().unwrap(),
    ]);
    assert!(sb.ok(&["search", "only"]).contains("Nothing matches"));
    let out = sb
        .cmd()
        .args(["search", "only"])
        .env("KNODE_SPACE", &other)
        .output()
        .unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("Only in other")
    );
}

#[test]
fn connect_claude_code_without_claude_prints_the_command() {
    let sb = Sandbox::new();
    let out = sb.ok(&["connect", "claude-code"]);
    assert!(out.contains("not on your PATH"), "{out}");
    assert!(
        out.contains("claude mcp add --scope user knode -- /"),
        "{out}"
    );
    assert!(
        out.trim_end().ends_with(" mcp --agent claude-code"),
        "{out}"
    );
    assert!(out.contains("There is no space yet"), "{out}");
}

#[cfg(unix)]
#[test]
fn connect_claude_code_runs_claude_once() {
    let sb = Sandbox::new();
    sb.fake_program("claude", 1);
    let out = sb.ok(&["connect", "claude-code", "--print"]);
    assert!(
        out.starts_with("Would give claude-code its own key"),
        "{out}"
    );
    assert!(out.contains("Would run:"), "{out}");
    assert_eq!(sb.log("claude"), "");
    assert!(!sb.knode_home().join("agent-keys").exists());

    let out = sb.ok(&["connect", "claude-code"]);
    assert!(out.contains("Added Knode to Claude Code"), "{out}");
    let log = sb.log("claude");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "mcp get knode");
    assert!(
        lines[1].starts_with("mcp add --scope user knode -- /")
            && lines[1].ends_with(" mcp --agent claude-code"),
        "{log}"
    );

    // Added before agent keys: replaced, so it runs with its key.
    let sb = Sandbox::new();
    sb.fake_program("claude", 0);
    let out = sb.ok(&["connect", "claude-code"]);
    assert!(out.contains("Added Knode to Claude Code"), "{out}");
    let log = sb.log("claude");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "mcp get knode");
    assert_eq!(lines[1], "mcp remove --scope user knode");
    assert!(lines[2].ends_with(" mcp --agent claude-code"), "{log}");
}

#[cfg(unix)]
#[test]
fn connect_codex_uses_codex_when_installed() {
    let sb = Sandbox::new();
    sb.fake_program("codex", 1);
    let out = sb.ok(&["connect", "codex"]);
    assert!(out.contains("Added Knode to Codex"), "{out}");
    let log = sb.log("codex");
    assert!(log.starts_with("mcp add knode -- /"), "{log}");
    assert!(!sb.path("codex/config.toml").exists());
}

#[test]
fn connect_codex_edits_its_config_otherwise() {
    let sb = Sandbox::new();
    let file = sb.path("codex/config.toml");
    let out = sb.ok(&["connect", "codex", "--print"]);
    assert!(out.contains("[mcp_servers.knode]"), "{out}");
    assert!(!file.exists());

    let out = sb.ok(&["connect", "codex"]);
    assert!(out.contains("Added Knode to Codex"), "{out}");
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("[mcp_servers.knode]\ncommand = \"/"),
        "{text}"
    );
    assert!(
        text.ends_with("args = [\"mcp\", \"--agent\", \"codex\"]\n"),
        "{text}"
    );
    assert!(sb.ok(&["connect", "codex"]).contains("already in Codex"));

    fs::write(
        &file,
        format!("model = \"o3\"\n\n{}", text.replace("/", "/old/")),
    )
    .unwrap();
    let out = sb.ok(&["connect", "codex"]);
    assert!(out.contains("Backed up"), "{out}");
    let backup = fs::read_to_string(sb.path("codex/config.toml.knode-backup")).unwrap();
    assert!(backup.contains("/old/"), "{backup}");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("model = \"o3\"\n"), "{text}");
    assert!(!text.contains("/old/"), "{text}");

    fs::write(&file, "not = [toml").unwrap();
    let err = sb.fails(&["connect", "codex"]);
    assert!(err.contains("not valid TOML"), "{err}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "not = [toml");
}

#[test]
fn connect_gemini_and_claude_desktop_edit_json_safely() {
    let sb = Sandbox::new();
    let gemini = sb.home().join(".gemini/settings.json");
    fs::create_dir_all(gemini.parent().unwrap()).unwrap();
    fs::write(
        &gemini,
        "{\"theme\": \"Dark\", \"mcpServers\": {\"x\": {\"command\": \"y\"}}}",
    )
    .unwrap();

    let out = sb.ok(&["connect", "gemini"]);
    assert!(out.contains("Added Knode to Gemini CLI"), "{out}");
    let value: Value = serde_json::from_str(&fs::read_to_string(&gemini).unwrap()).unwrap();
    assert_eq!(value["theme"], "Dark");
    assert_eq!(value["mcpServers"]["x"]["command"], "y");
    assert_eq!(
        value["mcpServers"]["knode"]["args"],
        serde_json::json!(["mcp", "--agent", "gemini-cli"])
    );
    assert!(
        sb.home()
            .join(".gemini/settings.json.knode-backup")
            .is_file()
    );
    assert!(
        sb.ok(&["connect", "gemini"])
            .contains("already in Gemini CLI")
    );

    let desktop = if cfg!(target_os = "macos") {
        sb.home()
            .join("Library/Application Support/Claude/claude_desktop_config.json")
    } else {
        sb.path("config/Claude/claude_desktop_config.json")
    };
    let out = sb.ok(&["connect", "claude-desktop", "--print"]);
    assert!(out.contains("\"mcpServers\""), "{out}");
    assert!(!desktop.exists());
    let out = sb.ok(&["connect", "claude-desktop"]);
    assert!(out.contains("Quit and reopen Claude Desktop"), "{out}");
    let value: Value = serde_json::from_str(&fs::read_to_string(&desktop).unwrap()).unwrap();
    assert!(
        value["mcpServers"]["knode"]["command"]
            .as_str()
            .unwrap()
            .starts_with('/')
    );

    fs::write(&desktop, "{broken").unwrap();
    let err = sb.fails(&["connect", "claude-desktop"]);
    assert!(err.contains("not valid JSON"), "{err}");
    assert_eq!(fs::read_to_string(&desktop).unwrap(), "{broken");
}

#[test]
fn unknown_agents_are_rejected() {
    let sb = Sandbox::new();
    let err = sb.fails(&["connect", "notepad"]);
    assert!(err.contains("claude-code"), "{err}");
}

#[cfg(unix)]
#[test]
fn connect_gives_an_agent_its_own_key_and_the_default_space_only() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    sb.ok(&["init", "personal"]);
    let out = sb.ok(&["connect", "gemini"]);
    assert!(out.contains("Gave gemini-cli its own key"), "{out}");
    assert!(out.contains("gemini-cli can read and write work"), "{out}");
    assert!(out.contains("cannot see any other space"), "{out}");

    // The key is in a file only its owner can read, and nowhere else.
    let file = sb.knode_home().join("agent-keys/gemini-cli.key");
    let key = fs::read_to_string(&file).unwrap();
    assert!(key.starts_with("knk_") && key.len() == 68, "{key}");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let settings = fs::read_to_string(sb.home().join(".gemini/settings.json")).unwrap();
    assert!(!settings.contains("knk_"), "{settings}");
    let registry = fs::read_to_string(sb.knode_home().join("agents.json")).unwrap();
    assert!(!registry.contains(&key), "{registry}");
    for space in ["work", "personal"] {
        let path = sb.space(space);
        for entry in walk(&path) {
            let bytes = fs::read(&entry).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(&key),
                "{}",
                entry.display()
            );
        }
    }

    // Connecting again keeps the key and the access.
    let out = sb.ok(&["connect", "gemini"]);
    assert!(!out.contains("Gave"), "{out}");
    assert_eq!(fs::read_to_string(&file).unwrap(), key);

    let list = sb.ok(&["agents", "list"]);
    assert!(
        list.contains("gemini-cli (gemini-cli, has a key)"),
        "{list}"
    );
    assert!(list.contains("work  "), "{list}");
    assert!(list.contains("write"), "{list}");
    assert!(!list.contains("personal"), "{list}");

    let out = sb.ok(&["agents", "grant", "gemini", "personal", "read"]);
    assert!(out.contains("gemini-cli now can read personal"), "{out}");
    let list = sb.ok(&["agents", "list"]);
    assert!(list.contains("read"), "{list}");
    let err = sb.fails(&["agents", "grant", "nobody", "work", "read"]);
    assert!(err.contains("no agent named"), "{err}");
    let err = sb.fails(&["agents", "grant", "gemini", "work", "admin"]);
    assert!(err.contains("possible values"), "{err}");

    let out = sb.ok(&["agents", "revoke", "gemini"]);
    assert!(out.contains("knode connect gemini"), "{out}");
    assert!(!file.exists());
    let list = sb.ok(&["agents", "list"]);
    assert!(list.contains("revoked"), "{list}");
    assert!(list.contains("none"), "{list}");

    // Connected again: a new key, and the default space once more.
    let out = sb.ok(&["connect", "gemini"]);
    assert!(out.contains("Gave gemini-cli its own key"), "{out}");
    assert_ne!(fs::read_to_string(&file).unwrap(), key);
    assert!(out.contains("can read and write work"), "{out}");
}

#[test]
fn agents_list_offers_to_connect_agents_set_up_without_a_key() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let gemini = sb.home().join(".gemini/settings.json");
    fs::create_dir_all(gemini.parent().unwrap()).unwrap();
    fs::write(
        &gemini,
        r#"{"mcpServers": {"knode": {"command": "/old/knode", "args": ["mcp"]}}}"#,
    )
    .unwrap();
    let list = sb.ok(&["agents", "list"]);
    assert!(list.contains("No agents yet"), "{list}");
    assert!(
        list.contains("stopped until connected again: gemini-cli"),
        "{list}"
    );
    assert!(list.contains("`knode connect gemini`"), "{list}");
    sb.ok(&["connect", "gemini"]);
    let list = sb.ok(&["agents", "list"]);
    assert!(!list.contains("stopped until"), "{list}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

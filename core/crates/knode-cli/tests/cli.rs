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
    let config = fs::read_to_string(sb.knode_home().join("config.toml")).unwrap();
    assert!(!config.contains("personal"), "{config}");

    let dir = sb.path("elsewhere");
    let out = sb.ok(&["init", "side", "--dir", dir.to_str().unwrap(), "--default"]);
    assert!(out.contains("It is now your default space."), "{out}");
    assert!(dir.join("side.knode").is_dir());

    let err = sb.fails(&["init", "work"]);
    assert!(err.contains("already exists"), "{err}");
    let err = sb.fails(&["init", "a/b"]);
    assert!(err.contains("cannot be a space name"), "{err}");

    let out = sb.ok(&["spaces"]);
    assert!(out.contains("* side"), "{out}");
    assert!(out.contains("(default)"), "{out}");
    assert!(out.contains("  work  "), "{out}");
    assert!(out.contains("  personal  "), "{out}");
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
    assert_eq!(out.trim(), "Imported 2 notes.");
    let out = sb.ok(&["import", notes.to_str().unwrap()]);
    assert_eq!(out.trim(), "Imported 0 notes, 2 already there.");

    let out = sb.ok(&["search", "seat"]);
    assert!(out.contains("Pricing decision  (note, "), "{out}");
    assert!(out.contains("by tester)"), "{out}");
    assert!(out.contains("ideas  (note, "), "{out}");

    let out = sb.ok(&["search", "per", "seat", "--json", "-n", "5"]);
    let hits: Value = serde_json::from_str(&out).unwrap();
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["node"]["title"], "Pricing decision");
    assert_eq!(hits[0]["node"]["updated_by"]["kind"], "person");
    assert_eq!(hits[0]["node"]["updated_by"]["actor"], "tester");

    assert!(sb.ok(&["search", "zebra"]).contains("Nothing matches"));

    let err = sb.fails(&["import", sb.path("missing").to_str().unwrap()]);
    assert!(err.contains("does not exist"), "{err}");
    fs::write(sb.path("data.csv"), "a,b").unwrap();
    let err = sb.fails(&["import", sb.path("data.csv").to_str().unwrap()]);
    assert!(err.contains("not a Markdown file or a session"), "{err}");
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
    assert!(out.trim_end().ends_with(" mcp"), "{out}");
}

#[cfg(unix)]
#[test]
fn connect_claude_code_runs_claude_once() {
    let sb = Sandbox::new();
    sb.fake_program("claude", 1);
    let out = sb.ok(&["connect", "claude-code", "--print"]);
    assert!(out.starts_with("Would run:"), "{out}");
    assert_eq!(sb.log("claude"), "");

    let out = sb.ok(&["connect", "claude-code"]);
    assert!(out.contains("Added Knode to Claude Code"), "{out}");
    let log = sb.log("claude");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], "mcp get knode");
    assert!(
        lines[1].starts_with("mcp add --scope user knode -- /") && lines[1].ends_with(" mcp"),
        "{log}"
    );

    let sb = Sandbox::new();
    sb.fake_program("claude", 0);
    let out = sb.ok(&["connect", "claude-code"]);
    assert!(out.contains("already in Claude Code"), "{out}");
    assert_eq!(sb.log("claude").trim(), "mcp get knode");
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
    assert!(text.ends_with("args = [\"mcp\"]\n"), "{text}");
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
        serde_json::json!(["mcp"])
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

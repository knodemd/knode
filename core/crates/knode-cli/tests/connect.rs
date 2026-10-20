//! `knode connect` for every agent whose settings file it edits, in a sandboxed home and XDG
//! config folder: the real settings of the person running the tests are never read or touched.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::Sandbox;
use serde_json::{Value, json};

/// Where Electron editors keep their data in the sandbox.
fn app_data(sb: &Sandbox) -> PathBuf {
    if cfg!(target_os = "macos") {
        sb.home().join("Library/Application Support")
    } else {
        sb.path("config")
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Reads JSON with comments and trailing commas.
fn read_jsonc(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap();
    jsonc_parser::cst::CstRootNode::parse(&text, &Default::default())
        .unwrap()
        .value()
        .and_then(|v| v.to_serde_value())
        .unwrap()
}

fn args(agent: &str) -> Value {
    json!(["mcp", "--agent", agent])
}

/// Connects `agent` with `--print` (which must change nothing), then for real, then again
/// (which must change nothing either). Returns the output of the real run.
fn connect_checked(sb: &Sandbox, agent: &str, files: &[&Path]) -> String {
    let before: Vec<Option<String>> = files.iter().map(|f| fs::read_to_string(f).ok()).collect();
    let out = sb.ok(&["connect", agent, "--print"]);
    assert!(out.contains("Would write"), "{out}");
    let after: Vec<Option<String>> = files.iter().map(|f| fs::read_to_string(f).ok()).collect();
    assert_eq!(before, after, "--print changed a file");

    let out = sb.ok(&["connect", agent]);
    for (file, before) in files.iter().zip(&before) {
        assert!(
            out.contains(&format!("in {}", file.display())),
            "{agent}: {out}"
        );
        let mut backup = file.as_os_str().to_owned();
        backup.push(".knode-backup");
        let backup = PathBuf::from(backup);
        match before {
            Some(text) => assert_eq!(&fs::read_to_string(&backup).unwrap(), text),
            None => assert!(!backup.exists()),
        }
    }
    let written: Vec<String> = files
        .iter()
        .map(|f| fs::read_to_string(f).unwrap())
        .collect();
    let again = sb.ok(&["connect", agent]);
    assert!(!again.contains("Gave"), "{again}");
    assert!(again.contains("Knode is already in"), "{again}");
    let still: Vec<String> = files
        .iter()
        .map(|f| fs::read_to_string(f).unwrap())
        .collect();
    assert_eq!(written, still, "connecting twice changed a file");
    out
}

#[test]
fn cursor() {
    let sb = Sandbox::new();
    sb.ok(&["init", "work"]);
    let file = sb.home().join(".cursor/mcp.json");
    write(
        &file,
        r#"{"mcpServers": {"other": {"command": "x"}, "knode": {"command": "/old"}}}"#,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let out = connect_checked(&sb, "cursor", &[&file]);
    assert!(out.contains("Restart Cursor to use it."), "{out}");
    let value = read_json(&file);
    assert_eq!(value["mcpServers"]["other"]["command"], "x");
    assert_eq!(value["mcpServers"]["knode"]["type"], "stdio");
    assert_eq!(value["mcpServers"]["knode"]["args"], args("cursor"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Cursor reads plain JSON: a file with comments is refused and left alone.
    write(&file, "// mine\n{}");
    let err = sb.fails(&["connect", "cursor"]);
    assert!(err.contains("it has comments"), "{err}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "// mine\n{}");
}

#[test]
fn windsurf_uses_every_folder_it_has() {
    let sb = Sandbox::new();
    let current = sb.path("config/devin/mcp_config.json");
    let legacy = sb.home().join(".codeium/windsurf/mcp_config.json");

    // Nothing there yet: the location in the current docs.
    let out = sb.ok(&["connect", "windsurf"]);
    assert!(out.contains("Refresh the MCP servers in Cascade"), "{out}");
    assert_eq!(
        read_json(&current)["mcpServers"]["knode"]["args"],
        args("windsurf")
    );
    assert!(!legacy.exists());

    // An older Windsurf as well: both.
    write(&legacy, r#"{"mcpServers": {}}"#);
    connect_checked(&sb, "windsurf", &[&legacy]);
    assert_eq!(
        read_json(&legacy)["mcpServers"]["knode"]["args"],
        args("windsurf")
    );
}

#[test]
fn cline_in_every_editor_and_its_cli() {
    let sb = Sandbox::new();
    // Not installed anywhere: refused before the agent gets a key.
    let err = sb.fails(&["connect", "cline"]);
    assert!(err.contains("Cline was not found"), "{err}");
    assert!(!sb.knode_home().join("agent-keys/cline.key").exists());

    let storage = |host: &str| {
        app_data(&sb)
            .join(host)
            .join("User/globalStorage/saoudrizwan.claude-dev")
    };
    fs::create_dir_all(storage("Code")).unwrap();
    let cursor = storage("Cursor").join("settings/cline_mcp_settings.json");
    write(
        &cursor,
        "{\n  \"mcpServers\": {\n    \"x\": {\"command\": \"y\", \"disabled\": true}\n  }\n}\n",
    );
    fs::create_dir_all(sb.home().join(".cline")).unwrap();
    let code = storage("Code").join("settings/cline_mcp_settings.json");
    let cli = sb
        .home()
        .join(".cline/data/settings/cline_mcp_settings.json");
    let out = connect_checked(&sb, "cline", &[&code, &cursor, &cli]);
    assert!(out.contains("Added Knode to Cline in VS Code"), "{out}");
    assert!(out.contains("Added Knode to Cline in Cursor"), "{out}");
    assert!(out.contains("Added Knode to Cline CLI"), "{out}");
    for file in [&code, &cursor, &cli] {
        let knode = &read_json(file)["mcpServers"]["knode"];
        assert_eq!(knode["args"], args("cline"));
        assert_eq!(knode["disabled"], false);
    }
    assert_eq!(read_json(&cursor)["mcpServers"]["x"]["disabled"], true);
}

#[test]
fn roo_code() {
    let sb = Sandbox::new();
    let err = sb.fails(&["connect", "roo-code"]);
    assert!(err.contains("Roo Code was not found"), "{err}");
    let dir = app_data(&sb).join("VSCodium/User/globalStorage/rooveterinaryinc.roo-cline");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("settings/mcp_settings.json");
    let out = connect_checked(&sb, "roo", &[&file]);
    assert!(out.contains("Roo Code in VSCodium"), "{out}");
    assert_eq!(
        read_json(&file)["mcpServers"]["knode"],
        json!({
            "command": read_json(&file)["mcpServers"]["knode"]["command"],
            "args": args("roo-code"),
        })
    );
}

#[test]
fn copilot_cli() {
    let sb = Sandbox::new();
    let file = sb.home().join(".copilot/mcp-config.json");
    connect_checked(&sb, "copilot-cli", &[&file]);
    let knode = &read_json(&file)["mcpServers"]["knode"];
    assert_eq!(knode["type"], "local");
    assert_eq!(knode["args"], args("copilot-cli"));
    assert_eq!(knode["tools"], json!(["*"]));
}

#[test]
fn opencode_keeps_comments() {
    let sb = Sandbox::new();
    let file = sb.path("config/opencode/opencode.jsonc");
    write(
        &file,
        "{\n  // my model\n  \"model\": \"anthropic/claude\",\n  \"mcp\": {},\n}\n",
    );
    connect_checked(&sb, "opencode", &[&file]);
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("{\n  // my model\n  \"model\": \"anthropic/claude\",\n"),
        "{text}"
    );
    let knode = &read_jsonc(&file)["mcp"]["knode"];
    assert_eq!(knode["type"], "local");
    assert_eq!(knode["enabled"], true);
    let command = knode["command"].as_array().unwrap();
    assert!(command[0].as_str().unwrap().ends_with("knode"));
    assert_eq!(json!(command[1..]), args("opencode"));
    assert!(!sb.path("config/opencode/opencode.json").exists());
}

#[test]
fn amp() {
    let sb = Sandbox::new();
    let file = sb.home().join(".config/amp/settings.json");
    write(&file, "{\n  \"amp.notifications.enabled\": false\n}\n");
    connect_checked(&sb, "amp", &[&file]);
    let value = read_json(&file);
    assert_eq!(value["amp.notifications.enabled"], false);
    assert_eq!(value["amp.mcpServers"]["knode"]["args"], args("amp"));
}

#[test]
fn vscode_keeps_comments() {
    let sb = Sandbox::new();
    let file = app_data(&sb).join("Code/User/mcp.json");
    write(
        &file,
        "{\n  // servers I use\n  \"servers\": {\n    \"other\": { \"type\": \"stdio\", \"command\": \"x\" } // mine\n  }\n}\n",
    );
    let insiders = app_data(&sb).join("Code - Insiders/User");
    fs::create_dir_all(&insiders).unwrap();
    let insiders = insiders.join("mcp.json");
    let out = connect_checked(&sb, "vscode", &[&file, &insiders]);
    assert!(out.contains("Reload VS Code to use it."), "{out}");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("// servers I use"), "{text}");
    assert!(text.contains("// mine"), "{text}");
    for file in [&file, &insiders] {
        let knode = &read_jsonc(file)["servers"]["knode"];
        assert_eq!(knode["type"], "stdio");
        assert_eq!(knode["args"], args("vscode"));
    }
}

#[test]
fn zed_keeps_comments() {
    let sb = Sandbox::new();
    let file = if cfg!(target_os = "macos") {
        sb.home().join(".config/zed/settings.json")
    } else {
        sb.path("config/zed/settings.json")
    };
    let original =
        "// Zed settings\n{\n  \"theme\": \"One Dark\", // mine\n  \"ui_font_size\": 16,\n}\n";
    write(&file, original);
    connect_checked(&sb, "zed", &[&file]);
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("// Zed settings\n{\n  \"theme\": \"One Dark\", // mine\n"),
        "{text}"
    );
    let knode = &read_jsonc(&file)["context_servers"]["knode"];
    assert_eq!(knode["args"], args("zed"));
    assert_eq!(knode["env"], json!({}));

    // Something no JSON reader accepts is refused, and the file is left alone.
    write(&file, "{ 'theme': 'One Dark' }");
    let err = sb.fails(&["connect", "zed"]);
    assert!(err.contains("not valid JSON"), "{err}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "{ 'theme': 'One Dark' }"
    );
}

#[test]
fn aider_gets_guidance_and_no_key() {
    let sb = Sandbox::new();
    let out = sb.ok(&["connect", "aider"]);
    assert!(out.contains("Aider has no MCP client"), "{out}");
    assert!(out.contains("knode markdown folder"), "{out}");
    assert!(out.contains("/run knode search"), "{out}");
    assert!(!sb.knode_home().join("agent-keys").exists());
    assert!(!sb.knode_home().join("agents.json").exists());
}

#[test]
fn list_shows_every_agent_and_what_is_here() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.home().join(".cursor")).unwrap();
    write(
        &sb.path("config/opencode/opencode.json"),
        r#"{"mcp": {"knode": {"type": "local", "command": ["knode"]}}}"#,
    );
    let out = sb.ok(&["connect", "--list"]);
    for agent in [
        "claude-code",
        "codex",
        "gemini",
        "claude-desktop",
        "cursor",
        "windsurf",
        "cline",
        "roo-code",
        "copilot-cli",
        "opencode",
        "amp",
        "aider",
        "vscode",
        "zed",
    ] {
        assert!(
            out.lines().any(|l| l.starts_with(&format!("{agent} "))),
            "{agent}: {out}"
        );
    }
    let row = |agent: &str| {
        out.lines()
            .find(|l| l.starts_with(&format!("{agent} ")))
            .unwrap()
            .to_string()
    };
    assert!(row("cursor").contains(" yes "), "{out}");
    assert!(row("cursor").contains("~/.cursor/mcp.json"), "{out}");
    assert!(row("zed").contains(" no "), "{out}");
    assert!(row("opencode").contains("(has Knode)"), "{out}");
    assert!(row("cline").contains("not installed"), "{out}");
    // Listing starts nothing and changes nothing.
    assert!(!sb.knode_home().exists());
    assert!(!sb.home().join(".cursor/mcp.json").exists());

    let err = sb.fails(&["connect"]);
    assert!(err.contains("<AGENT>"), "{err}");
}

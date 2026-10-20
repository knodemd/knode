//! `knode connect`: give an agent its own key and add the Knode MCP server to it.
//!
//! The agent gets a key of its own, kept in the machine's credential store (never in the
//! agent's settings), and, the first time, write access to the default space only. Its MCP
//! server runs `knode mcp --agent <name>`, which reads the key from the store.
//!
//! Claude Code and Codex are configured with their own commands when they are installed.
//! Every other agent, and Codex when it is not installed, has its settings file edited: only the
//! `knode` entry is added or replaced, the file is backed up first, its permissions and (where
//! the agent allows them) its comments are kept, and running it again changes nothing. Aider has
//! no MCP client, so it gets guidance instead.

mod json;
mod places;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use knode_config::Paths;
use knode_format::GrantLevel;
use knode_service::keys::{KeyStore, default_key_store, fingerprint, generate_key};
use serde_json::Value;

use places::Place;

/// The name of the MCP server in every agent's settings.
const SERVER: &str = "knode";
/// The suffix of the backup made before a settings file is changed.
const BACKUP_SUFFIX: &str = "knode-backup";

/// An agent Knode can be added to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    /// Claude Code, through `claude mcp add`.
    ClaudeCode,
    /// Codex, through `codex mcp add` or ~/.codex/config.toml.
    Codex,
    /// Gemini CLI, through ~/.gemini/settings.json.
    #[value(alias = "gemini-cli")]
    Gemini,
    /// Claude Desktop, through claude_desktop_config.json.
    ClaudeDesktop,
    /// Cursor, through ~/.cursor/mcp.json.
    Cursor,
    /// Windsurf (Devin Desktop), through mcp_config.json.
    Windsurf,
    /// Cline, in every VS Code family editor it is installed in, and Cline CLI.
    Cline,
    /// Roo Code, in every VS Code family editor it is installed in.
    #[value(alias = "roo")]
    RooCode,
    /// GitHub Copilot CLI, through ~/.copilot/mcp-config.json.
    #[value(alias = "copilot")]
    CopilotCli,
    /// OpenCode, through ~/.config/opencode/opencode.json.
    #[value(name = "opencode")]
    OpenCode,
    /// Amp, through ~/.config/amp/settings.json.
    Amp,
    /// Aider has no MCP client: prints what to do instead.
    Aider,
    /// GitHub Copilot in VS Code (agent mode), through the user mcp.json.
    #[value(name = "vscode", alias = "vs-code")]
    VsCode,
    /// Zed, through context_servers in its settings.json.
    Zed,
}

impl Agent {
    /// The agent's name in Knode, the actor of its changes, and its kind.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Gemini => "gemini-cli",
            other => other.arg(),
        }
    }

    /// What to type after `knode connect` for it.
    pub fn arg(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
            Agent::ClaudeDesktop => "claude-desktop",
            Agent::Cursor => "cursor",
            Agent::Windsurf => "windsurf",
            Agent::Cline => "cline",
            Agent::RooCode => "roo-code",
            Agent::CopilotCli => "copilot-cli",
            Agent::OpenCode => "opencode",
            Agent::Amp => "amp",
            Agent::Aider => "aider",
            Agent::VsCode => "vscode",
            Agent::Zed => "zed",
        }
    }

    pub const ALL: [Agent; 14] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::Gemini,
        Agent::ClaudeDesktop,
        Agent::Cursor,
        Agent::Windsurf,
        Agent::Cline,
        Agent::RooCode,
        Agent::CopilotCli,
        Agent::OpenCode,
        Agent::Amp,
        Agent::Aider,
        Agent::VsCode,
        Agent::Zed,
    ];

    /// The agent with this name or `knode connect` argument.
    pub fn find(name: &str) -> Option<Agent> {
        Agent::ALL
            .into_iter()
            .find(|a| a.name() == name || a.arg() == name)
    }
}

/// The arguments the agent runs `knode` with.
fn mcp_args(agent: Agent, space: Option<&Path>) -> Vec<String> {
    let mut args = vec!["mcp".to_string(), "--agent".into(), agent.name().into()];
    if let Some(space) = space {
        args.push("--space".into());
        args.push(space.to_string_lossy().into_owned());
    }
    args
}

/// A settings file about to be changed: what it holds now and what it will hold, or `None`
/// when Knode is already there.
struct Change {
    place: Place,
    current: Option<String>,
    updated: Option<String>,
}

/// `knode connect <agent>`. `space` is the space given with `--space`, if any; the agent gets
/// access to it, or to the default space, the first time.
pub fn run(paths: &Paths, agent: Agent, print: bool, space: Option<&Path>) -> Result<()> {
    if agent == Agent::Aider {
        print_aider();
        return Ok(());
    }
    let knode = knode_path()?;
    let flag = space.map(crate::absolute).transpose()?;
    let args = mcp_args(agent, flag.as_deref());
    // Every settings file is read and checked before the agent gets a key, so a file that
    // cannot be changed leaves nothing half done.
    let changes = places::places(agent)?
        .into_iter()
        .map(|place| {
            let current = read_optional(&place.file)?;
            let entry = places::entry(agent, &knode, &args);
            let updated = json::set_entry(
                current.as_deref().unwrap_or(""),
                place.syntax,
                place.container,
                SERVER,
                &entry,
            )
            .with_context(|| format!("cannot add Knode to {}", place.file.display()))?;
            Ok(Change {
                place,
                current,
                updated,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let target = crate::resolve(paths, flag.as_deref())
        .ok()
        .map(|r| r.path)
        .filter(|p| p.exists());
    if print {
        println!(
            "Would give {} its own key, kept in {}.",
            agent.name(),
            store(paths).describe()
        );
        if let Some(space) = &target {
            println!(
                "Would let it read and write {} if it has no access yet.",
                space.display()
            );
        }
        println!();
    } else {
        issue_key(paths, agent, target.as_deref())?;
    }
    match agent {
        Agent::ClaudeCode => claude_code(&knode, &args, print),
        Agent::Codex => codex(&knode, &args, print),
        _ => {
            for (i, change) in changes.into_iter().enumerate() {
                if i > 0 {
                    println!();
                }
                finish_edit(
                    &change.place.label,
                    &change.place.file,
                    change.current,
                    change.updated,
                    print,
                    &change.place.restart,
                )?;
            }
            Ok(())
        }
    }
}

/// What to do for Aider, which cannot run MCP servers.
fn print_aider() {
    println!(
        "Aider has no MCP client yet, so Knode cannot be added to it, and nothing was changed.\n\
         \n\
         To give Aider what is in Knode:\n\
         \n\
         - Keep a live Markdown copy of a space, then add notes to the chat as read-only files:\n\
         \n\
         \x20   knode markdown folder ~/Knode/markdown\n\
         \x20   aider --read ~/Knode/markdown/<note>.md\n\
         \n\
         - Or, inside Aider, search Knode and add the results to the chat:\n\
         \n\
         \x20   /run knode search <words>\n\
         \n\
         Both read your default space (or the one in --space) as you, not as an agent."
    );
}

/// `knode connect --list`: every agent, whether it is on this machine and where Knode goes.
pub fn list() -> Result<()> {
    let home = home().ok();
    let short = |path: &Path| -> String {
        match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    };
    let rows: Vec<(Agent, bool, String)> = Agent::ALL
        .into_iter()
        .map(|agent| {
            let detected = places::detected(agent);
            let how = match agent {
                Agent::ClaudeCode => "claude mcp add --scope user".to_string(),
                Agent::Codex if which("codex").is_some() => "codex mcp add".to_string(),
                Agent::Codex => codex_config().map_or_else(|e| e.to_string(), |f| short(&f)),
                Agent::Aider => "no MCP client; prints what to do instead".to_string(),
                _ => match places::places(agent) {
                    Ok(found) => found
                        .iter()
                        .map(|p| {
                            let known = if places::has_knode(p) {
                                " (has Knode)"
                            } else {
                                ""
                            };
                            format!("{}{known}", short(&p.file))
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    Err(_) => "not installed in any editor".to_string(),
                },
            };
            (agent, detected, how)
        })
        .collect();
    let width = rows
        .iter()
        .map(|(a, _, _)| a.arg().len())
        .max()
        .unwrap_or(0);
    println!("{:width$}  {:8}  Where", "Agent", "Detected");
    for (agent, detected, how) in rows {
        let detected = if detected { "yes" } else { "no" };
        println!("{:width$}  {detected:8}  {how}", agent.arg());
    }
    println!("\nConnect one with `knode connect <agent>`; `--print` shows what it would change.");
    Ok(())
}

/// The credential store of this machine.
pub fn store(paths: &Paths) -> Box<dyn KeyStore> {
    default_key_store(&paths.home)
}

/// Makes sure the agent has a valid key in the credential store and in Knode Service, and,
/// when it has no access to any space, gives it write access to `space`. Prints what it can do.
fn issue_key(paths: &Paths, agent: Agent, space: Option<&Path>) -> Result<()> {
    let client = crate::service::connect().context("Knode Service could not start")?;
    let store = store(paths);
    let name = agent.name();
    let known = client.agents()?.into_iter().find(|a| a.name == name);
    let stored = store
        .get(name)
        .with_context(|| format!("cannot read the key of {name} from {}", store.describe()))?;
    let valid = match (&known, &stored) {
        (Some(known), Some(key)) => {
            let fingerprint = fingerprint(key);
            known
                .keys
                .iter()
                .any(|k| k.fingerprint == fingerprint && k.revoked_at.is_none())
        }
        _ => false,
    };
    let info = if valid {
        known.expect("a valid key belongs to a known agent")
    } else {
        let key = generate_key().context("cannot make a key")?;
        store
            .set(name, &key)
            .with_context(|| format!("cannot keep the key of {name} in {}", store.describe()))?;
        let info = client.add_agent(name, name, &fingerprint(&key), &crate::person())?;
        println!(
            "Gave {name} its own key, kept in {}. Its old keys, if any, no longer work.",
            store.describe()
        );
        info
    };
    let has_access = info
        .spaces
        .iter()
        .any(|s| s.level.is_some_and(|l| l > GrantLevel::None));
    if !has_access {
        match space {
            Some(space) => {
                client.set_grant(name, space, GrantLevel::Write, &crate::person())?;
            }
            None => {
                println!(
                    "There is no space yet, so {name} cannot see anything. Create one with \
                     `knode init <name>`, then `knode agents grant {name} <space> write`."
                );
                return Ok(());
            }
        }
    }
    let info = client
        .agents()?
        .into_iter()
        .find(|a| a.name == name)
        .context("the agent was not saved")?;
    crate::agents::print_access(&info);
    println!(
        "It cannot see any other space. Change this with `knode agents grant {name} <space> \
         none|read|write`."
    );
    Ok(())
}

/// The agents whose settings run `knode mcp` without an agent key: set up before keys. They
/// stop working until they are connected again.
pub fn without_key() -> Vec<Agent> {
    let mut found = Vec::new();
    let json_args = |file: &Path| -> Option<Vec<String>> {
        let text = fs::read_to_string(file).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let args = value
            .get("mcpServers")?
            .get(SERVER)?
            .get("args")?
            .as_array()?;
        Some(
            args.iter()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect(),
        )
    };
    let unkeyed =
        |args: Option<Vec<String>>| args.is_some_and(|a| !a.iter().any(|a| a == "--agent"));
    if let Ok(home) = home() {
        let mut claude = vec![home.join(".claude.json")];
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            claude.push(PathBuf::from(dir).join(".claude.json"));
        }
        if claude.iter().any(|f| unkeyed(json_args(f))) {
            found.push(Agent::ClaudeCode);
        }
        let codex = codex_config().ok().and_then(|file| {
            let doc: toml_edit::DocumentMut = fs::read_to_string(file).ok()?.parse().ok()?;
            let args = doc
                .get("mcp_servers")?
                .get(SERVER)?
                .get("args")?
                .as_array()?;
            Some(
                args.iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect(),
            )
        });
        if unkeyed(codex) {
            found.push(Agent::Codex);
        }
        if unkeyed(json_args(&home.join(".gemini/settings.json"))) {
            found.push(Agent::Gemini);
        }
    }
    if claude_desktop_config().is_ok_and(|f| unkeyed(json_args(&f))) {
        found.push(Agent::ClaudeDesktop);
    }
    found
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("cannot find your home folder: HOME is not set")
}

fn claude_desktop_config() -> Result<PathBuf> {
    let home = home()?;
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"))
            .join("Claude")
    };
    Ok(dir.join("claude_desktop_config.json"))
}

fn codex_config() -> Result<PathBuf> {
    let dir = match std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".codex"),
    };
    Ok(dir.join("config.toml"))
}

/// The path agents should run: the `knode` found on PATH when it is this program (a stable
/// link that survives upgrades), else this program's own path.
pub(crate) fn knode_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find the knode program")?;
    if let Some(found) = which("knode")
        && same_file(&found, &exe)
    {
        return Ok(found);
    }
    Ok(exe)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Finds a program on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A command line as a person would type it, quoting what needs it.
fn shell_line<I, S>(program: &str, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut line = program.to_string();
    for arg in args {
        let arg = arg.as_ref().to_string_lossy();
        line.push(' ');
        let plain = !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c));
        if plain {
            line.push_str(&arg);
        } else {
            line.push('\'');
            line.push_str(&arg.replace('\'', r"'\''"));
            line.push('\'');
        }
    }
    line
}

fn run_program(program: &Path, args: &[&OsStr], shown: &str) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("cannot run `{shown}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        bail!("`{shown}` failed: {}", detail.trim());
    }
    Ok(())
}

fn claude_code(knode: &Path, mcp: &[String], print: bool) -> Result<()> {
    let mut args: Vec<&OsStr> = [
        OsStr::new("mcp"),
        OsStr::new("add"),
        OsStr::new("--scope"),
        OsStr::new("user"),
        OsStr::new(SERVER),
        OsStr::new("--"),
        knode.as_os_str(),
    ]
    .to_vec();
    args.extend(mcp.iter().map(OsStr::new));
    let line = shell_line("claude", &args);
    if print {
        println!("Would run:\n\n  {line}");
        return Ok(());
    }
    let Some(claude) = which("claude") else {
        println!(
            "Claude Code (`claude`) is not on your PATH. Once it is installed, run:\n\n  {line}"
        );
        return Ok(());
    };
    let existing = Command::new(&claude)
        .args(["mcp", "get", SERVER])
        .output()
        .with_context(|| "cannot run `claude mcp get knode`".to_string())?;
    if existing.status.success() {
        let shown = String::from_utf8_lossy(&existing.stdout);
        if shown.contains(&mcp.join(" ")) {
            println!("Knode is already in Claude Code.");
            return Ok(());
        }
        // Set up before agent keys, or for another space: replace it.
        let remove = [
            OsStr::new("mcp"),
            OsStr::new("remove"),
            OsStr::new("--scope"),
            OsStr::new("user"),
            OsStr::new(SERVER),
        ];
        run_program(
            &claude,
            &remove,
            &shell_line("claude", remove.iter().copied()),
        )?;
    }
    run_program(&claude, &args, &line)?;
    println!("Added Knode to Claude Code. Start a new Claude Code session to use it.");
    Ok(())
}

fn codex(knode: &Path, mcp: &[String], print: bool) -> Result<()> {
    if let Some(codex) = which("codex") {
        let mut args: Vec<&OsStr> = [
            OsStr::new("mcp"),
            OsStr::new("add"),
            OsStr::new(SERVER),
            OsStr::new("--"),
            knode.as_os_str(),
        ]
        .to_vec();
        args.extend(mcp.iter().map(OsStr::new));
        let line = shell_line("codex", &args);
        if print {
            println!("Would run:\n\n  {line}");
            return Ok(());
        }
        run_program(&codex, &args, &line)?;
        println!("Added Knode to Codex. Start a new Codex session to use it.");
        return Ok(());
    }
    let file = codex_config()?;
    let current = read_optional(&file)?;
    let updated = codex_toml(current.as_deref().unwrap_or(""), knode, mcp)
        .with_context(|| format!("{} is not valid TOML; fix it and try again", file.display()))?;
    finish_edit(
        "Codex",
        &file,
        current,
        updated,
        print,
        "Start a new Codex session to use it",
    )
}

/// `config.toml` for Codex with `[mcp_servers.knode]` running `knode` with `mcp`, or `None`
/// when it already does.
fn codex_toml(text: &str, knode: &Path, mcp: &[String]) -> Result<Option<String>> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let command = knode.to_string_lossy().into_owned();
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            toml_edit::Item::Table(table)
        })
        .as_table_like_mut()
        .context("mcp_servers must be a table")?;
    let same = servers
        .get(SERVER)
        .and_then(|s| s.as_table_like())
        .is_some_and(|s| {
            let args: Option<Vec<&str>> = s
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect());
            s.get("command").and_then(|c| c.as_str()) == Some(command.as_str())
                && args.is_some_and(|a| a.iter().eq(mcp.iter()))
        });
    if same {
        return Ok(None);
    }
    let mut table = toml_edit::Table::new();
    table.insert("command", toml_edit::value(command));
    let mut args = toml_edit::Array::new();
    for arg in mcp {
        args.push(arg.as_str());
    }
    table.insert("args", toml_edit::value(args));
    servers.insert(SERVER, toml_edit::Item::Table(table));
    Ok(Some(doc.to_string()))
}

fn read_optional(file: &Path) -> Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

fn backup_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(BACKUP_SUFFIX);
    file.with_file_name(name)
}

fn finish_edit(
    name: &str,
    file: &Path,
    current: Option<String>,
    updated: Option<String>,
    print: bool,
    restart: &str,
) -> Result<()> {
    let Some(updated) = updated else {
        println!("Knode is already in {name} ({}).", file.display());
        return Ok(());
    };
    if print {
        println!("Would write {}:\n\n{updated}", file.display());
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if current.is_some() {
        let backup = backup_path(file);
        fs::copy(file, &backup)
            .with_context(|| format!("cannot back up {} first", file.display()))?;
        println!("Backed up {} to {}", file.display(), backup.display());
    }
    write_atomic(file, &updated)?;
    println!("Added Knode to {name} in {}. {restart}.", file.display());
    Ok(())
}

/// Replaces a file in one step, keeping its permissions.
fn write_atomic(file: &Path, text: &str) -> Result<()> {
    let mut tmp_name = file.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".knode-tmp");
    let tmp = file.with_file_name(tmp_name);
    fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(file) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, file).with_context(|| format!("cannot write {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_needs_it() {
        assert_eq!(
            shell_line(
                "claude",
                ["mcp", "--", "/Applications/Knode.app/knode", "mcp"]
            ),
            "claude mcp -- /Applications/Knode.app/knode mcp"
        );
        assert_eq!(
            shell_line("x", ["/Users/a b/knode", "it's"]),
            r"x '/Users/a b/knode' 'it'\''s'"
        );
    }

    fn mcp() -> Vec<String> {
        mcp_args(Agent::Gemini, None)
    }

    #[test]
    fn json_keeps_other_servers_and_order() {
        let knode = Path::new("/bin/knode");
        let entry = places::entry(Agent::Gemini, knode, &mcp());
        let set =
            |text: &str| json::set_entry(text, json::Syntax::Json, "mcpServers", SERVER, &entry);
        let text = r#"{"theme": "dark", "mcpServers": {"other": {"command": "x"}}, "a": 1}"#;
        let out = set(text).unwrap().unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "x");
        assert_eq!(value["mcpServers"]["knode"]["command"], "/bin/knode");
        assert_eq!(
            value["mcpServers"]["knode"]["args"],
            serde_json::json!(["mcp", "--agent", "gemini-cli"])
        );
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["theme", "mcpServers", "a"]);
        assert_eq!(set(&out).unwrap(), None);
        assert!(set("").unwrap().is_some());
        assert!(set("[1]").is_err());
        assert!(set("{nope").is_err());
        assert!(set(r#"{"mcpServers": 3}"#).is_err());
    }

    #[test]
    fn every_agent_has_a_distinct_name() {
        for agent in Agent::ALL {
            assert_eq!(Agent::find(agent.arg()), Some(agent));
            assert_eq!(Agent::find(agent.name()), Some(agent));
            let value = agent.to_possible_value().unwrap();
            assert_eq!(value.get_name(), agent.arg());
        }
    }

    #[test]
    fn toml_keeps_the_rest_of_the_file() {
        let knode = Path::new("/bin/knode");
        let mcp = &mcp_args(Agent::Codex, None);
        let text = "# my settings\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let out = codex_toml(text, knode, mcp).unwrap().unwrap();
        assert!(out.starts_with("# my settings\nmodel = \"o3\"\n"), "{out}");
        assert!(
            out.contains("[mcp_servers.other]\ncommand = \"x\""),
            "{out}"
        );
        assert!(
            out.contains(
                "[mcp_servers.knode]\ncommand = \"/bin/knode\"\nargs = [\"mcp\", \"--agent\", \"codex\"]"
            ),
            "{out}"
        );
        assert_eq!(codex_toml(&out, knode, mcp).unwrap(), None);
        let fresh = codex_toml("", knode, mcp).unwrap().unwrap();
        assert_eq!(
            fresh,
            "[mcp_servers.knode]\ncommand = \"/bin/knode\"\nargs = [\"mcp\", \"--agent\", \"codex\"]\n"
        );
        assert!(codex_toml("mcp_servers = 3", knode, mcp).is_err());
    }
}

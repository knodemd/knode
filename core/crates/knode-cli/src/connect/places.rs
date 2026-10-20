//! Where each agent keeps its MCP settings on macOS and Linux, and whether it is on this machine.
//!
//! Every location here comes from the agent's own documentation or source code, except where a
//! comment says it is unverified.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::json::Syntax;
use super::{Agent, SERVER, home, which};

/// A settings file `knode connect` edits for an agent.
#[derive(Debug, Clone)]
pub struct Place {
    /// How the file is named to people: "Cursor", "Cline in VS Code".
    pub label: String,
    pub file: PathBuf,
    pub syntax: Syntax,
    /// The top-level key that holds the MCP servers.
    pub container: &'static str,
    /// What to do for the agent to see the change, as an imperative sentence.
    pub restart: String,
}

/// An editor built on VS Code, with its folder name under the application support folder.
struct Host {
    label: &'static str,
    dir: &'static str,
}

/// The VS Code family, where Cline and Roo Code run.
const HOSTS: [Host; 5] = [
    Host {
        label: "VS Code",
        dir: "Code",
    },
    Host {
        label: "VS Code Insiders",
        dir: "Code - Insiders",
    },
    Host {
        label: "VSCodium",
        dir: "VSCodium",
    },
    Host {
        label: "Cursor",
        dir: "Cursor",
    },
    Host {
        label: "Windsurf",
        dir: "Windsurf",
    },
];

/// Cline's extension id, as its globalStorage folder is named (saoudrizwan.claude-dev).
const CLINE_EXTENSION: &str = "saoudrizwan.claude-dev";
/// Roo Code's extension id, lowercased as VS Code names its globalStorage folder.
const ROO_EXTENSION: &str = "rooveterinaryinc.roo-cline";

/// `$XDG_CONFIG_HOME` when it is set to an absolute path, else `~/.config`.
pub fn config_home() -> Result<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => Ok(dir),
        None => Ok(home()?.join(".config")),
    }
}

/// Where Electron apps keep their data: Application Support on macOS, the XDG config folder on
/// Linux.
fn app_data() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home()?.join("Library/Application Support"))
    } else {
        config_home()
    }
}

/// The `User` folder of a VS Code family editor.
fn user_dir(host: &Host) -> Result<PathBuf> {
    Ok(app_data()?.join(host.dir).join("User"))
}

/// The files an agent's settings are edited in, in the order they are edited. Empty for agents
/// configured through their own command (Claude Code, Codex when installed) or not at all
/// (Aider). Fails when the agent is not on this machine and there is no sensible file to create.
pub fn places(agent: Agent) -> Result<Vec<Place>> {
    let one = |label: &str, file: PathBuf, syntax, container, restart: &str| {
        Ok(vec![Place {
            label: label.to_string(),
            file,
            syntax,
            container,
            restart: restart.to_string(),
        }])
    };
    match agent {
        Agent::ClaudeCode | Agent::Codex | Agent::Aider => Ok(Vec::new()),
        Agent::Gemini => one(
            "Gemini CLI",
            home()?.join(".gemini/settings.json"),
            Syntax::Json,
            "mcpServers",
            "Start a new Gemini CLI session to use it",
        ),
        Agent::ClaudeDesktop => one(
            "Claude Desktop",
            super::claude_desktop_config()?,
            Syntax::Json,
            "mcpServers",
            "Quit and reopen Claude Desktop to use it",
        ),
        // https://cursor.com/docs/context/mcp: ~/.cursor/mcp.json. Comments are not documented,
        // so the file must be plain JSON.
        Agent::Cursor => one(
            "Cursor",
            home()?.join(".cursor/mcp.json"),
            Syntax::Json,
            "mcpServers",
            "Restart Cursor to use it",
        ),
        Agent::Windsurf => {
            // Current docs (docs.devin.ai/desktop/cascade/mcp, after Windsurf became Devin
            // Desktop): $XDG_CONFIG_HOME/devin/mcp_config.json, else ~/.config/devin. Earlier
            // Windsurf releases read ~/.codeium/windsurf/mcp_config.json; that location is no
            // longer in the official docs (unverified for current releases). Both are edited
            // when they exist; with neither, the current one is created.
            let current = config_home()?.join("devin/mcp_config.json");
            let legacy = home()?.join(".codeium/windsurf/mcp_config.json");
            let mut files: Vec<PathBuf> = [current.clone(), legacy]
                .into_iter()
                .filter(|f| f.parent().is_some_and(Path::is_dir))
                .collect();
            if files.is_empty() {
                files.push(current);
            }
            Ok(files
                .into_iter()
                .map(|file| Place {
                    label: "Windsurf".into(),
                    file,
                    syntax: Syntax::Json,
                    container: "mcpServers",
                    restart: "Refresh the MCP servers in Cascade, or restart Windsurf, to use it"
                        .into(),
                })
                .collect())
        }
        Agent::Cline => {
            // VS Code extension: <globalStorage>/settings/cline_mcp_settings.json (cline/cline,
            // apps/vscode/src/core/storage/disk.ts). Cline CLI: ~/.cline/data/settings/
            // cline_mcp_settings.json, or CLINE_MCP_SETTINGS_PATH (docs.cline.bot). Both read
            // it with JSON.parse: plain JSON.
            let mut found = extension_places(
                CLINE_EXTENSION,
                "cline_mcp_settings.json",
                "Cline",
                "Cline picks it up on its own; if it does not, restart",
            )?;
            if let Some(file) = cline_cli_settings()? {
                found.push(Place {
                    label: "Cline CLI".into(),
                    file,
                    syntax: Syntax::Json,
                    container: "mcpServers",
                    restart: "Start a new Cline session to use it".into(),
                });
            }
            if found.is_empty() {
                bail!(
                    "Cline was not found on this machine: install it in VS Code (or VS Code \
                     Insiders, VSCodium, Cursor or Windsurf), open it once, then run this again"
                );
            }
            Ok(found)
        }
        Agent::RooCode => {
            // <globalStorage>/settings/mcp_settings.json (RooCodeInc/Roo-Code,
            // src/shared/globalFileNames.ts and src/services/mcp/McpHub.ts), read with
            // JSON.parse: plain JSON.
            let found = extension_places(
                ROO_EXTENSION,
                "mcp_settings.json",
                "Roo Code",
                "Roo Code picks it up on its own; if it does not, restart",
            )?;
            if found.is_empty() {
                bail!(
                    "Roo Code was not found on this machine: install it in VS Code (or VS Code \
                     Insiders, VSCodium, Cursor or Windsurf), open it once, then run this again"
                );
            }
            Ok(found)
        }
        // docs.github.com, "Adding MCP servers for GitHub Copilot CLI": $COPILOT_HOME/
        // mcp-config.json, else ~/.copilot/mcp-config.json. VS Code's "Copilot Global" servers
        // are kept in the same file.
        Agent::CopilotCli => one(
            "Copilot CLI",
            copilot_home()?.join("mcp-config.json"),
            Syntax::Json,
            "mcpServers",
            "Start a new Copilot CLI session to use it",
        ),
        // opencode.ai/docs/config: ~/.config/opencode/opencode.json, JSON or JSONC. The
        // folder follows XDG_CONFIG_HOME (OpenCode uses xdg-basedir). An opencode.jsonc is
        // edited when there is one.
        Agent::OpenCode => {
            let dir = config_home()?.join("opencode");
            let jsonc = dir.join("opencode.jsonc");
            let file = if jsonc.is_file() {
                jsonc
            } else {
                dir.join("opencode.json")
            };
            one(
                "OpenCode",
                file,
                Syntax::Jsonc,
                "mcp",
                "Start a new OpenCode session to use it",
            )
        }
        // ampcode.com/docs/cli/settings: ~/.config/amp/settings.json or settings.jsonc, on
        // macOS too. Whether Amp follows XDG_CONFIG_HOME is not documented, so ~/.config it is.
        Agent::Amp => {
            let dir = home()?.join(".config/amp");
            let jsonc = dir.join("settings.jsonc");
            let file = if jsonc.is_file() {
                jsonc
            } else {
                dir.join("settings.json")
            };
            one(
                "Amp",
                file,
                Syntax::Jsonc,
                "amp.mcpServers",
                "Start a new Amp session to use it",
            )
        }
        Agent::VsCode => {
            // code.visualstudio.com/docs/agents/reference/mcp-configuration: mcp.json in the
            // user profile, `servers`, JSONC. VS Code and VS Code Insiders when installed;
            // VS Code when neither is.
            let mut files = Vec::new();
            for host in &HOSTS[..2] {
                let dir = user_dir(host)?;
                if dir.is_dir() {
                    files.push((host.label, dir.join("mcp.json")));
                }
            }
            if files.is_empty() {
                files.push((HOSTS[0].label, user_dir(&HOSTS[0])?.join("mcp.json")));
            }
            Ok(files
                .into_iter()
                .map(|(label, file)| Place {
                    label: label.into(),
                    file,
                    syntax: Syntax::Jsonc,
                    container: "servers",
                    restart: format!("Reload {label} to use it"),
                })
                .collect())
        }
        // zed-industries/zed, crates/paths/src/paths.rs: ~/.config/zed on macOS, the XDG config
        // folder on Linux. Settings are JSONC; servers go in `context_servers`
        // (zed.dev/docs/ai/mcp).
        Agent::Zed => {
            let dir = if cfg!(target_os = "macos") {
                home()?.join(".config/zed")
            } else {
                config_home()?.join("zed")
            };
            one(
                "Zed",
                dir.join("settings.json"),
                Syntax::Jsonc,
                "context_servers",
                "Zed picks it up on its own; if it does not, restart Zed",
            )
        }
    }
}

/// The Cline or Roo Code settings of every VS Code family editor it is installed in.
fn extension_places(extension: &str, file: &str, name: &str, restart: &str) -> Result<Vec<Place>> {
    let mut found = Vec::new();
    for host in &HOSTS {
        let dir = user_dir(host)?.join("globalStorage").join(extension);
        if dir.is_dir() {
            found.push(Place {
                label: format!("{name} in {}", host.label),
                file: dir.join("settings").join(file),
                syntax: Syntax::Json,
                container: "mcpServers",
                restart: format!("{restart} {}", host.label),
            });
        }
    }
    Ok(found)
}

/// Cline CLI's MCP settings, when Cline CLI has been used on this machine.
fn cline_cli_settings() -> Result<Option<PathBuf>> {
    if let Some(path) = std::env::var_os("CLINE_MCP_SETTINGS_PATH").filter(|p| !p.is_empty()) {
        return Ok(Some(PathBuf::from(path)));
    }
    let dir = match std::env::var_os("CLINE_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".cline"),
    };
    Ok(dir
        .is_dir()
        .then(|| dir.join("data/settings/cline_mcp_settings.json")))
}

fn copilot_home() -> Result<PathBuf> {
    match std::env::var_os("COPILOT_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(home()?.join(".copilot")),
    }
}

/// The entry for `knode` in an agent's settings, in that agent's format.
pub fn entry(agent: Agent, knode: &Path, args: &[String]) -> Value {
    let command = knode.to_string_lossy();
    match agent {
        // `type` is listed as required in Cursor's field reference.
        Agent::Cursor => json!({ "type": "stdio", "command": command, "args": args }),
        Agent::Cline => json!({ "command": command, "args": args, "disabled": false }),
        Agent::CopilotCli => {
            json!({ "type": "local", "command": command, "args": args, "tools": ["*"] })
        }
        Agent::OpenCode => {
            let mut line = vec![command.into_owned()];
            line.extend(args.iter().cloned());
            json!({ "type": "local", "command": line, "enabled": true })
        }
        Agent::VsCode => json!({ "type": "stdio", "command": command, "args": args }),
        Agent::Zed => json!({ "command": command, "args": args, "env": {} }),
        _ => json!({ "command": command, "args": args }),
    }
}

/// Whether the agent seems to be on this machine: its program is on PATH or its settings
/// folder exists.
pub fn detected(agent: Agent) -> bool {
    let on_path = |name: &str| which(name).is_some();
    let exists = |path: Result<PathBuf>| path.is_ok_and(|p| p.exists());
    let parent_exists = |place: &Place| place.file.parent().is_some_and(Path::is_dir);
    match agent {
        Agent::ClaudeCode => {
            on_path("claude")
                || exists(home().map(|h| h.join(".claude.json")))
                || exists(home().map(|h| h.join(".claude")))
        }
        Agent::Codex => {
            on_path("codex")
                || super::codex_config().is_ok_and(|f| f.parent().is_some_and(Path::is_dir))
        }
        Agent::Gemini => on_path("gemini") || exists(home().map(|h| h.join(".gemini"))),
        Agent::ClaudeDesktop => {
            places(agent).is_ok_and(|p| p.iter().any(parent_exists))
                || (cfg!(target_os = "macos") && Path::new("/Applications/Claude.app").is_dir())
        }
        Agent::Cursor => on_path("cursor") || exists(home().map(|h| h.join(".cursor"))),
        Agent::Windsurf => {
            on_path("windsurf") || places(agent).is_ok_and(|p| p.iter().any(parent_exists))
        }
        // Found only where they are installed: `places` fails otherwise.
        Agent::Cline => on_path("cline") || places(agent).is_ok(),
        Agent::RooCode => places(agent).is_ok(),
        Agent::CopilotCli => on_path("copilot") || exists(copilot_home()),
        Agent::OpenCode => on_path("opencode") || exists(config_home().map(|c| c.join("opencode"))),
        Agent::Amp => on_path("amp") || exists(home().map(|h| h.join(".config/amp"))),
        Agent::Aider => on_path("aider"),
        Agent::VsCode => {
            on_path("code")
                || HOSTS[..2]
                    .iter()
                    .any(|h| user_dir(h).is_ok_and(|d| d.is_dir()))
        }
        Agent::Zed => on_path("zed") || places(agent).is_ok_and(|p| p.iter().any(parent_exists)),
    }
}

/// Whether `knode` is already in this settings file.
pub fn has_knode(place: &Place) -> bool {
    std::fs::read_to_string(&place.file)
        .ok()
        .and_then(|text| super::json::get_entry(&text, place.container, SERVER))
        .is_some()
}

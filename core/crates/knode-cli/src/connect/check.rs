//! `knode connect <agent> --check`: which space an agent's MCP settings make it use, and whether
//! that is the space a project's conversations go to. Reads the agent's settings, the folders of
//! the spaces and the agent's grants; changes nothing, and never gives the agent access.
//!
//! A project folder routes capture: it says which space a project's conversations go to. It does
//! not switch the space an agent's MCP server uses, which is the `--space` in its settings, or
//! `KNODE_SPACE` in their environment, or else the default space at the time the agent starts.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use knode_config::{Config, Paths};
use knode_format::{GrantLevel, Manifest};
use knode_service::AgentInfo;
use knode_service::spaces::{self, MappedFolder};
use serde::Serialize;
use serde_json::Value;

use super::{Agent, SERVER, codex_config, home, json, places};

/// One `knode` MCP entry in an agent's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The settings file.
    pub file: PathBuf,
    /// Which settings these are, for people: "Claude Code (user)".
    pub label: String,
    /// The command and its arguments, in order.
    pub command: Vec<String>,
    /// `KNODE_SPACE` in the entry's environment, if set.
    pub env_space: Option<String>,
}

impl Entry {
    fn flag(&self, name: &str) -> Option<&str> {
        self.command
            .windows(2)
            .find(|w| w[0] == name)
            .map(|w| w[1].as_str())
    }

    fn runs_mcp(&self) -> bool {
        self.command.iter().any(|a| a == "mcp")
    }
}

/// A space, with its name when it can be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Named {
    pub space: PathBuf,
    pub name: Option<String>,
}

impl Named {
    fn of(path: &Path) -> Self {
        let space = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let name = Manifest::read(&space).ok().map(|m| m.name);
        Named { space, name }
    }

    fn shown(&self) -> String {
        match &self.name {
            Some(name) => format!("\"{name}\""),
            None => self.space.display().to_string(),
        }
    }
}

/// Where the space a check compares with comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Meant {
    /// `--space` given to the check.
    Flag,
    /// The project folder that holds `--project`.
    Project,
    /// The default space: no `--space`, and no project folder holds `--project` (or none was
    /// given).
    Default,
}

/// The space an agent is meant to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Intended {
    #[serde(flatten)]
    pub space: Named,
    pub from: Meant,
}

/// What one settings entry makes the agent use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reach {
    #[serde(flatten)]
    pub entry: Entry,
    /// The agent the entry runs as (`--agent`), if any.
    pub runs_as: Option<String>,
    /// The space it uses, or `None` when there is none (no `--space` and no default space).
    pub space: Option<Named>,
    /// Whether it uses the default space, and so follows it when the default changes.
    pub follows_default: bool,
}

/// One thing that stops the agent from using the intended space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// `not_connected`, `no_key`, `other_agent`, `no_space`, `wrong_space`, `unknown_agent`,
    /// `no_access` or `ambiguous_project`.
    pub code: &'static str,
    pub message: String,
}

/// The result of a check, as `--check --json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// What to type after `knode connect`.
    pub agent: &'static str,
    /// The agent's name in Knode.
    pub name: &'static str,
    /// The project folder checked, if any.
    pub project: Option<PathBuf>,
    /// The project folders that hold it, one per space, nearest first.
    pub destinations: Vec<MappedFolder>,
    /// The space the agent is meant to use, if there is one.
    pub intended: Option<Intended>,
    /// Every `knode` entry in the agent's settings, in the order the agent reads them.
    pub settings: Vec<Reach>,
    /// What the agent may do in the space its settings make it use: `none`, `read`, `write`,
    /// or `None` when that is not known (the agent is unknown, or Knode Service did not answer).
    pub access: Option<GrantLevel>,
    /// Whether the agent reaches the intended space with at least read access.
    pub ok: bool,
    pub problems: Vec<Problem>,
    /// Things worth knowing that do not stop it, such as read-only access.
    pub notes: Vec<String>,
}

/// What a check needs, read from this machine: the inputs of [`assess`].
pub struct Inputs<'a> {
    pub agent: Agent,
    pub entries: Vec<Entry>,
    pub config: &'a Config,
    pub project: Option<PathBuf>,
    pub destinations: Vec<MappedFolder>,
    /// The space given with `--space`, if any.
    pub flag: Option<PathBuf>,
    /// The agents Knode Service knows, or `None` when it did not answer.
    pub agents: Option<Vec<AgentInfo>>,
}

/// Decides whether the agent reaches the intended space. Pure: everything comes in `inputs`.
pub fn assess(inputs: Inputs<'_>) -> Report {
    let Inputs {
        agent,
        entries,
        config,
        project,
        destinations,
        flag,
        agents,
    } = inputs;
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let problem = |code, message: String| Problem { code, message };

    let default = config
        .default_space
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty());
    let mut destination_spaces: Vec<&Path> = Vec::new();
    for folder in &destinations {
        if !destination_spaces.contains(&folder.space.as_path()) {
            destination_spaces.push(&folder.space);
        }
    }
    let intended = match (&flag, destination_spaces.as_slice()) {
        (Some(flag), _) => Some(Intended {
            space: Named::of(flag),
            from: Meant::Flag,
        }),
        (None, [one]) => Some(Intended {
            space: Named::of(one),
            from: Meant::Project,
        }),
        (None, []) => default.map(|d| Intended {
            space: Named::of(d),
            from: Meant::Default,
        }),
        (None, several) => {
            let names: Vec<String> = several.iter().map(|s| Named::of(s).shown()).collect();
            problems.push(problem(
                "ambiguous_project",
                format!(
                    "{} is a project folder of several spaces ({}): its conversations are \
                     captured into each, and an agent uses one space. Remove the folder from \
                     the spaces it does not belong to (`knode folders remove`), or check with \
                     --space.",
                    destinations[0].path,
                    names.join(", ")
                ),
            ));
            None
        }
    };
    if let (Some(project), None, Meant::Default) = (
        &project,
        destinations.first(),
        intended.as_ref().map_or(Meant::Flag, |i| i.from),
    ) {
        notes.push(format!(
            "No project folder holds {}: its conversations go to the default space.",
            project.display()
        ));
    }

    let settings: Vec<Reach> = entries
        .into_iter()
        .filter(Entry::runs_mcp)
        .map(|entry| {
            let pinned = entry.flag("--space").map(PathBuf::from);
            let env = entry.env_space.as_deref().map(PathBuf::from);
            let resolved = knode_config::resolve_space(pinned.as_deref(), env.as_deref(), config);
            let follows_default = resolved
                .as_ref()
                .is_ok_and(|r| r.source == knode_config::SpaceSource::Config);
            Reach {
                runs_as: entry.flag("--agent").map(str::to_string),
                space: resolved.ok().map(|r| Named::of(&r.path)),
                follows_default,
                entry,
            }
        })
        .collect();

    // The entry the agent uses: the first, in the order it reads them.
    let used = settings.first();
    let mut access = None;
    match used {
        None => problems.push(problem(
            "not_connected",
            format!(
                "Knode is not in the settings of {}. `knode connect {}` adds it.",
                agent.name(),
                agent.arg()
            ),
        )),
        Some(reach) => {
            match reach.runs_as.as_deref() {
                None => problems.push(problem(
                    "no_key",
                    format!(
                        "{} runs Knode without an agent key ({}), set up before keys: it gets \
                         nothing. `knode connect {}` gives it a key.",
                        agent.name(),
                        reach.entry.file.display(),
                        agent.arg()
                    ),
                )),
                Some(name) if name != agent.name() => problems.push(problem(
                    "other_agent",
                    format!(
                        "{} runs Knode as the agent {name} ({}), with that agent's access. \
                         `knode connect {}` sets it up as itself.",
                        agent.name(),
                        reach.entry.file.display(),
                        agent.arg()
                    ),
                )),
                Some(_) => {}
            }
            match &reach.space {
                None => problems.push(problem(
                    "no_space",
                    "Knode has no default space, and the agent's settings name none: it can \
                     see nothing. `knode init <name>` creates one."
                        .into(),
                )),
                Some(space) if !space.space.exists() => problems.push(problem(
                    "no_space",
                    format!(
                        "The agent uses {}, which does not exist on this machine.",
                        space.space.display()
                    ),
                )),
                Some(space) => {
                    if let Some(intended) = &intended
                        && intended.space.space != space.space
                    {
                        let how = if reach.follows_default {
                            "it follows the default space".to_string()
                        } else {
                            format!("its settings name it in {}", reach.entry.file.display())
                        };
                        let why = match intended.from {
                            Meant::Flag => "you asked to check".to_string(),
                            Meant::Project => format!(
                                "the conversations of {} go to",
                                project
                                    .as_deref()
                                    .map_or_else(String::new, |p| p.display().to_string())
                            ),
                            Meant::Default => "is the default space".to_string(),
                        };
                        problems.push(problem(
                            "wrong_space",
                            format!(
                                "{} uses {} ({how}), not {}, which {why}. A project folder \
                                 does not switch the space an agent uses. `knode connect {} \
                                 --space {}` points it there; it gets access only if it has \
                                 none anywhere, else `knode agents grant {} <space> write`.",
                                agent.name(),
                                space.shown(),
                                intended.space.shown(),
                                agent.arg(),
                                intended.space.space.display(),
                                agent.name()
                            ),
                        ));
                    }
                    match agents
                        .as_ref()
                        .map(|all| all.iter().find(|a| a.name == agent.name()))
                    {
                        None => notes.push(
                            "Knode Service did not answer, so the agent's access is not known."
                                .into(),
                        ),
                        Some(None) => problems.push(problem(
                            "unknown_agent",
                            format!(
                                "Knode does not know the agent {}: it has no key. `knode \
                                 connect {}` gives it one.",
                                agent.name(),
                                agent.arg()
                            ),
                        )),
                        Some(Some(info)) if !info.has_valid_key() => problems.push(problem(
                            "unknown_agent",
                            format!(
                                "The keys of {} were revoked. `knode connect {}` gives it a \
                                 new one.",
                                agent.name(),
                                agent.arg()
                            ),
                        )),
                        Some(Some(info)) => {
                            let level = info
                                .spaces
                                .iter()
                                .find(|g| Named::of(&g.space).space == space.space)
                                .and_then(|g| g.level)
                                .unwrap_or(GrantLevel::None);
                            access = Some(level);
                            match level {
                                GrantLevel::None => problems.push(problem(
                                    "no_access",
                                    format!(
                                        "{} may not see {}, so it can read nothing there. \
                                         Only you can change that: `knode agents grant {} {} \
                                         read|write`.",
                                        agent.name(),
                                        space.shown(),
                                        agent.name(),
                                        space.space.display()
                                    ),
                                )),
                                GrantLevel::Read => notes.push(format!(
                                    "{} may only read {}: it cannot save there.",
                                    agent.name(),
                                    space.shown()
                                )),
                                GrantLevel::Write => {}
                            }
                        }
                    }
                }
            }
        }
    }
    for other in settings.iter().skip(1) {
        if other.space.as_ref().map(|s| &s.space)
            != used.and_then(|u| u.space.as_ref().map(|s| &s.space))
        {
            notes.push(format!(
                "{} also has a Knode entry in {} that uses another space; the agent reads {} \
                 first.",
                agent.name(),
                other.entry.file.display(),
                used.map_or_else(String::new, |u| u.entry.file.display().to_string())
            ));
        }
    }
    Report {
        agent: agent.arg(),
        name: agent.name(),
        project,
        destinations,
        intended,
        ok: problems.is_empty(),
        settings,
        access,
        problems,
        notes,
    }
}

/// The strings of a JSON MCP entry: its command, then its arguments.
fn entry_command(entry: &Value) -> Vec<String> {
    let mut strings = Vec::new();
    for key in ["command", "args"] {
        match entry.get(key) {
            Some(Value::String(s)) => strings.push(s.clone()),
            Some(Value::Array(items)) => {
                strings.extend(items.iter().filter_map(|a| a.as_str().map(str::to_string)))
            }
            _ => {}
        }
    }
    strings
}

fn entry_env_space(entry: &Value) -> Option<String> {
    ["env", "environment"]
        .iter()
        .find_map(|key| entry.get(key)?.get(knode_config::KNODE_SPACE_VAR)?.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn json_entry(file: &Path, label: &str, value: Option<Value>) -> Option<Entry> {
    let entry = value?;
    Some(Entry {
        file: file.to_path_buf(),
        label: label.to_string(),
        command: entry_command(&entry),
        env_space: entry_env_space(&entry),
    })
}

fn read(file: &Path) -> Option<String> {
    fs::read_to_string(file).ok()
}

/// The file Claude Code keeps its user and local scopes in: `.claude.json` in
/// `$CLAUDE_CONFIG_DIR` when it is set, else in the home folder. Claude Code reads only that one.
fn claude_user_file(config_dir: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    match config_dir.filter(|d| !d.is_empty()) {
        Some(dir) => vec![PathBuf::from(dir).join(".claude.json")],
        None => home.map(|h| h.join(".claude.json")).into_iter().collect(),
    }
}

/// The `knode` entries in the agent's settings, in the order the agent reads them: for Claude
/// Code, the local scope of `project`, its `.mcp.json`, then the user scope.
pub fn entries(agent: Agent, project: Option<&Path>) -> Vec<Entry> {
    let mut found = Vec::new();
    match agent {
        Agent::Aider => {}
        Agent::ClaudeCode => {
            let files = claude_user_file(std::env::var_os("CLAUDE_CONFIG_DIR"), home().ok());
            let parsed: Vec<(PathBuf, Value)> = files
                .into_iter()
                .filter_map(|f| {
                    let value = serde_json::from_str(&read(&f)?).ok()?;
                    Some((f, value))
                })
                .collect();
            if let Some(project) = project {
                let key = project.to_string_lossy();
                for (file, value) in &parsed {
                    let entry = value
                        .get("projects")
                        .and_then(|p| p.get(key.as_ref()))
                        .and_then(|p| p.get("mcpServers"))
                        .and_then(|s| s.get(SERVER))
                        .cloned();
                    found.extend(json_entry(file, "Claude Code (local)", entry));
                }
                let shared = project.join(".mcp.json");
                if let Some(text) = read(&shared) {
                    let entry = json::get_entry(&text, "mcpServers", SERVER);
                    found.extend(json_entry(&shared, "Claude Code (project)", entry));
                }
            }
            for (file, value) in &parsed {
                let entry = value.get("mcpServers").and_then(|s| s.get(SERVER)).cloned();
                found.extend(json_entry(file, "Claude Code (user)", entry));
            }
        }
        Agent::Codex => {
            let Ok(file) = codex_config() else {
                return found;
            };
            let Some(doc) = read(&file).and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
            else {
                return found;
            };
            let Some(server) = doc
                .get("mcp_servers")
                .and_then(|s| s.get(SERVER))
                .and_then(|s| s.as_table_like())
            else {
                return found;
            };
            let mut command = Vec::new();
            if let Some(c) = server.get("command").and_then(|c| c.as_str()) {
                command.push(c.to_string());
            }
            if let Some(args) = server.get("args").and_then(|a| a.as_array()) {
                command.extend(args.iter().filter_map(|a| a.as_str().map(str::to_string)));
            }
            let env_space = server
                .get("env")
                .and_then(|e| e.as_table_like())
                .and_then(|e| e.get(knode_config::KNODE_SPACE_VAR))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            found.push(Entry {
                file,
                label: "Codex".into(),
                command,
                env_space,
            });
        }
        _ => {
            for place in places::places(agent).unwrap_or_default() {
                if let Some(text) = read(&place.file) {
                    let entry = json::get_entry(&text, place.container, SERVER);
                    found.extend(json_entry(&place.file, &place.label, entry));
                }
            }
        }
    }
    found
}

/// `knode connect <agent> --check`: prints whether the agent reaches the space meant for it
/// (`--space`, else the space `project`'s conversations go to, else the default space). Fails
/// when it does not, unless `json` asks for the report.
pub fn run(
    paths: &Paths,
    agent: Agent,
    flag: Option<&Path>,
    project: Option<&Path>,
    json: bool,
) -> Result<()> {
    let config = Config::load(paths)?;
    let flag = crate::named_space(paths, flag)?;
    let project = project.map(crate::absolute).transpose()?;
    let client = crate::service::client();
    let destinations = match &project {
        Some(project) => {
            let all = crate::spaces::routed_spaces(paths, client.as_ref(), None)?;
            // As given, as an agent records where it runs; else with links resolved.
            let found = spaces::project_destinations(&all, project);
            match fs::canonicalize(project) {
                Ok(real) if found.is_empty() => spaces::project_destinations(&all, &real),
                _ => found,
            }
        }
        None => Vec::new(),
    };
    let agents = client.as_ref().and_then(|c| c.agents().ok());
    let report = assess(Inputs {
        agent,
        entries: entries(agent, project.as_deref()),
        config: &config,
        project,
        destinations,
        flag,
        agents,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print(&report);
    if !report.ok {
        anyhow::bail!("{} does not reach the space meant for it", report.name);
    }
    Ok(())
}

fn print(report: &Report) {
    for folder in &report.destinations {
        println!(
            "{} is in the project folder {} of \"{}\"{}.",
            report
                .project
                .as_deref()
                .map_or_else(String::new, |p| p.display().to_string()),
            folder.path,
            folder.space_name,
            if folder.capture { "" } else { " (capture off)" }
        );
    }
    if let Some(intended) = &report.intended {
        let from = match intended.from {
            Meant::Flag => "given with --space",
            Meant::Project => "where the project's conversations go",
            Meant::Default => "the default space",
        };
        println!("Meant to use: {} ({from}).", intended.space.shown());
    }
    for reach in &report.settings {
        let uses = reach.space.as_ref().map_or("no space".into(), Named::shown);
        let how = if reach.follows_default {
            ", the default space"
        } else {
            ""
        };
        println!(
            "{} uses {uses}{how} ({}, {}).",
            report.name,
            reach.entry.label,
            reach.entry.file.display()
        );
    }
    if let Some(level) = report.access {
        println!(
            "It may {} there.",
            match level {
                GrantLevel::None => "not see anything",
                GrantLevel::Read => "read",
                GrantLevel::Write => "read and write",
            }
        );
    }
    for note in &report.notes {
        println!("Note: {note}");
    }
    for problem in &report.problems {
        eprintln!("problem: {}", problem.message);
    }
    if report.ok {
        println!(
            "OK: {} reaches the space meant for it. Nothing was changed.",
            report.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knode_service::SpaceGrant;

    #[test]
    fn claude_code_reads_only_the_config_dir_when_it_is_set() {
        let home = Some(PathBuf::from("/home/ada"));
        assert_eq!(
            claude_user_file(Some("/cfg".into()), home.clone()),
            [PathBuf::from("/cfg/.claude.json")]
        );
        assert_eq!(
            claude_user_file(None, home.clone()),
            [PathBuf::from("/home/ada/.claude.json")]
        );
        assert_eq!(
            claude_user_file(Some("".into()), home),
            [PathBuf::from("/home/ada/.claude.json")]
        );
        assert!(claude_user_file(None, None).is_empty());
    }

    fn entry(args: &[&str]) -> Entry {
        let mut command = vec!["/bin/knode".to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        Entry {
            file: PathBuf::from("/home/.gemini/settings.json"),
            label: "Gemini CLI".into(),
            command,
            env_space: None,
        }
    }

    fn agent(grants: &[(&Path, GrantLevel)]) -> AgentInfo {
        AgentInfo {
            id: "01".into(),
            name: "gemini-cli".into(),
            kind: "gemini-cli".into(),
            created_at: 0,
            keys: vec![knode_service::KeyInfo {
                fingerprint: "sha256:x".into(),
                created_at: 0,
                revoked_at: None,
            }],
            spaces: grants
                .iter()
                .map(|(space, level)| SpaceGrant {
                    space: space.to_path_buf(),
                    level: Some(*level),
                })
                .collect(),
        }
    }

    fn mapped(space: &Path) -> MappedFolder {
        MappedFolder {
            space: space.to_path_buf(),
            space_name: "x".into(),
            folder: "f".into(),
            path: "~/code/site".into(),
            capture: true,
            space_capture: true,
        }
    }

    fn codes(report: &Report) -> Vec<&'static str> {
        report.problems.iter().map(|p| p.code).collect()
    }

    #[test]
    fn the_agent_must_use_the_space_of_the_project() {
        let work = Path::new("/nowhere/Work.knode");
        let personal = Path::new("/nowhere/Personal.knode");
        let config = Config {
            default_space: Some(personal.to_path_buf()),
            ..Config::default()
        };
        let check = |entries: Vec<Entry>, grants: &[(&Path, GrantLevel)], dests| {
            assess(Inputs {
                agent: Agent::Gemini,
                entries,
                config: &config,
                project: Some(PathBuf::from("/code/site")),
                destinations: dests,
                flag: None,
                agents: Some(vec![agent(grants)]),
            })
        };
        // Not connected at all.
        let report = check(vec![], &[], vec![mapped(work)]);
        assert_eq!(codes(&report), ["not_connected"]);
        assert_eq!(report.intended.unwrap().from, Meant::Project);

        // Follows the default space (Personal), which does not exist here, and is not Work.
        let report = check(
            vec![entry(&["mcp", "--agent", "gemini-cli"])],
            &[],
            vec![mapped(work)],
        );
        assert!(report.settings[0].follows_default);
        assert_eq!(codes(&report), ["no_space"]);

        // No key, and the default space is not missing when it is a real folder: see the
        // integration tests. Two spaces hold the project: ambiguous.
        let report = check(
            vec![entry(&["mcp"])],
            &[],
            vec![mapped(work), mapped(personal)],
        );
        assert_eq!(codes(&report), ["ambiguous_project", "no_key", "no_space"]);
        assert!(report.intended.is_none());
    }

    #[test]
    fn existing_spaces_are_compared_with_access() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("Work.knode");
        let personal = dir.path().join("Personal.knode");
        fs::create_dir(&work).unwrap();
        fs::create_dir(&personal).unwrap();
        let work = fs::canonicalize(work).unwrap();
        let personal = fs::canonicalize(personal).unwrap();
        let config = Config {
            default_space: Some(personal.clone()),
            ..Config::default()
        };
        let pinned = |space: &Path| {
            entry(&[
                "mcp",
                "--agent",
                "gemini-cli",
                "--space",
                space.to_str().unwrap(),
            ])
        };
        let check = |entries: Vec<Entry>, grants: &[(&Path, GrantLevel)]| {
            assess(Inputs {
                agent: Agent::Gemini,
                entries,
                config: &config,
                project: Some(PathBuf::from("/code/site")),
                destinations: vec![mapped(&work)],
                flag: None,
                agents: Some(vec![agent(grants)]),
            })
        };
        let report = check(vec![pinned(&work)], &[(&work, GrantLevel::Write)]);
        assert!(report.ok, "{report:?}");
        assert_eq!(report.access, Some(GrantLevel::Write));

        // Pinned to another space: wrong space, and access is about the space it uses.
        let report = check(vec![pinned(&personal)], &[(&work, GrantLevel::Write)]);
        assert_eq!(codes(&report), ["wrong_space", "no_access"]);
        assert!(report.problems[0].message.contains("--space"));

        // The right space, but no grant there: never granted by the check.
        let report = check(vec![pinned(&work)], &[(&personal, GrantLevel::Write)]);
        assert_eq!(codes(&report), ["no_access"]);

        // Read only is a note, not a problem.
        let report = check(vec![pinned(&work)], &[(&work, GrantLevel::Read)]);
        assert!(report.ok);
        assert!(report.notes[0].contains("only read"), "{report:?}");

        // KNODE_SPACE in the entry's environment counts like --space.
        let mut by_env = entry(&["mcp", "--agent", "gemini-cli"]);
        by_env.env_space = Some(work.to_string_lossy().into_owned());
        let report = check(vec![by_env], &[(&work, GrantLevel::Write)]);
        assert!(report.ok, "{report:?}");
        assert!(!report.settings[0].follows_default);

        // Runs as another agent.
        let report = check(
            vec![entry(&[
                "mcp",
                "--agent",
                "codex",
                "--space",
                work.to_str().unwrap(),
            ])],
            &[(&work, GrantLevel::Write)],
        );
        assert_eq!(codes(&report), ["other_agent"]);

        // Unknown to Knode Service.
        let report = assess(Inputs {
            agent: Agent::Gemini,
            entries: vec![pinned(&work)],
            config: &config,
            project: None,
            destinations: vec![],
            flag: Some(work.clone()),
            agents: Some(vec![]),
        });
        assert_eq!(codes(&report), ["unknown_agent"]);
        assert_eq!(report.intended.unwrap().from, Meant::Flag);
    }
}

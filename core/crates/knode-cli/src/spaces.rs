//! `knode spaces` and `knode folders`: the spaces Knode knows, their names, icons and capture,
//! and the folders that feed each one, through Knode Service or the space opened directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use knode_config::{Config, Paths};
use knode_format::{Manifest, Provenance, Space, setting_keys};
use knode_service::{Client, FolderInfo, FolderInput, spaces};

use crate::service;

#[derive(Debug, Subcommand)]
pub enum SpacesAction {
    /// Rename a space. Its file keeps its name.
    Rename {
        /// The space: its name or its .knode path.
        #[arg(value_name = "SPACE")]
        target: String,
        /// The new name.
        name: String,
    },
    /// Set the icon of a space, one emoji, or remove it with --clear.
    Icon {
        /// The space: its name or its .knode path.
        #[arg(value_name = "SPACE")]
        target: String,
        /// The emoji, such as 🌱.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        emoji: Option<String>,
        /// Remove the icon.
        #[arg(long)]
        clear: bool,
    },
    /// Make a space the default: commands and agents use it when no space is given, and
    /// conversations of no project are captured into it.
    Default {
        /// The space: its name or its .knode path.
        #[arg(value_name = "SPACE")]
        target: String,
    },
    /// Turn capture of agent conversations into a space on or off.
    Capture {
        /// The space: its name or its .knode path.
        #[arg(value_name = "SPACE")]
        target: String,
        /// on or off.
        state: Switch,
    },
}

#[derive(Debug, Subcommand)]
pub enum FoldersAction {
    /// List the folders that feed the space.
    List {
        /// Print the folders as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add a folder to the space, or change it: a project whose agent conversations are
    /// captured into it, a folder of notes, or a folder where an agent keeps its sessions.
    Add {
        /// The folder.
        path: PathBuf,
        /// What the folder is.
        #[arg(long, value_enum, default_value_t = Kind::Project)]
        kind: Kind,
        /// Whether conversations held in it are captured (for a project: off means they are
        /// stored nowhere), or the folder is read.
        #[arg(long, value_enum, default_value_t = Switch::On)]
        capture: Switch,
        /// Folders inside it to leave out, as patterns such as `node_modules/**`. Repeat for
        /// several.
        #[arg(long, value_name = "PATTERN")]
        exclude: Vec<String>,
        /// For an agent folder: the agent whose sessions it holds, such as claude-code.
        #[arg(long, value_name = "AGENT")]
        agent: Option<String>,
    },
    /// Remove a folder from the space, by its path or id. Nothing captured from it is removed.
    Remove {
        /// The folder's path, or its id from `knode folders list`.
        folder: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    /// A folder people and agents work in, such as a code repository.
    Project,
    /// A folder of Markdown notes.
    Notes,
    /// A folder where an agent keeps its sessions.
    Agent,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Project => "project",
            Kind::Notes => "notes",
            Kind::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// One known space, as `knode spaces` shows it.
#[derive(Debug)]
struct Listed {
    name: Option<String>,
    path: PathBuf,
    icon: Option<String>,
    default: bool,
    capture: bool,
    missing: bool,
}

/// Every space Knode knows: those in `config.toml`, then those in the spaces folder.
fn known(paths: &Paths, config: &Config) -> Result<Vec<PathBuf>> {
    let mut list = config.known_spaces();
    for path in paths.spaces_in_dir()? {
        if !list.contains(&path) {
            list.push(path);
        }
    }
    Ok(list)
}

/// The space named by `arg`: a path to a `.knode` package, or the name of a known space.
pub fn find(paths: &Paths, arg: &str) -> Result<PathBuf> {
    let as_path = Path::new(arg);
    if as_path.exists()
        || as_path
            .extension()
            .is_some_and(|e| e == knode_format::EXTENSION)
    {
        let path = crate::absolute(as_path)?;
        if !path.exists() {
            bail!("there is no space at {}", path.display());
        }
        return Ok(path);
    }
    let config = Config::load(paths)?;
    let wanted = arg.trim().to_lowercase();
    let matches: Vec<PathBuf> = known(paths, &config)?
        .into_iter()
        .filter(|path| {
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase());
            let name = Manifest::read(path).ok().map(|m| m.name.to_lowercase());
            name.as_deref() == Some(wanted.as_str()) || stem.as_deref() == Some(wanted.as_str())
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("Knode knows no space named {arg:?}; see `knode spaces`"),
        _ => bail!("more than one space is named {arg:?}; give its .knode path instead"),
    }
}

pub fn run(paths: &Paths, action: Option<SpacesAction>, json: bool) -> Result<()> {
    let by = crate::person();
    match action {
        None => list(paths, json),
        Some(SpacesAction::Rename { target, name }) => {
            let path = find(paths, &target)?;
            let info = match service::client() {
                Some(client) => client.rename_space(&path, &name, &by)?,
                None => spaces::rename(&mut Space::open(&path)?, &name, &by)?,
            };
            println!(
                "Renamed the space at {} to \"{}\".",
                path.display(),
                info.name
            );
            Ok(())
        }
        Some(SpacesAction::Icon {
            target,
            emoji,
            clear,
        }) => {
            let path = find(paths, &target)?;
            let value = if clear { None } else { emoji };
            set_setting(&path, setting_keys::SPACE_ICON, value.as_deref(), &by)?;
            match value {
                Some(icon) => println!("{} now has the icon {icon}.", name_of(&path)),
                None => println!("{} has no icon now.", name_of(&path)),
            }
            Ok(())
        }
        Some(SpacesAction::Default { target }) => {
            let path = find(paths, &target)?;
            let mut config = Config::load(paths)?;
            config.set_default_space(&path);
            config.save(paths)?;
            println!("{} is now your default space.", name_of(&path));
            Ok(())
        }
        Some(SpacesAction::Capture { target, state }) => {
            let path = find(paths, &target)?;
            let value = match state {
                Switch::On => "on",
                Switch::Off => "off",
            };
            set_setting(&path, setting_keys::CAPTURE_ENABLED, Some(value), &by)?;
            match state {
                Switch::On => println!(
                    "Agent conversations are captured into {} again.",
                    name_of(&path)
                ),
                Switch::Off => println!(
                    "Agent conversations are no longer captured into {}; those of its projects \
                     are stored nowhere.",
                    name_of(&path)
                ),
            }
            Ok(())
        }
    }
}

fn name_of(path: &Path) -> String {
    match Manifest::read(path) {
        Ok(manifest) => format!("\"{}\"", manifest.name),
        Err(_) => path.display().to_string(),
    }
}

fn set_setting(path: &Path, key: &str, value: Option<&str>, by: &Provenance) -> Result<()> {
    match service::client() {
        Some(client) => {
            client.set_setting(path, key, value, by)?;
        }
        None => {
            spaces::set_setting(&mut Space::open(path)?, key, value, by)?;
        }
    }
    Ok(())
}

/// The settings of a space, through the service when it runs.
fn settings_of(client: Option<&Client>, path: &Path) -> Result<BTreeMap<String, String>> {
    Ok(match client {
        Some(client) => client.settings(path)?,
        None => spaces::settings(&Space::open(path)?)?,
    })
}

fn list(paths: &Paths, json: bool) -> Result<()> {
    let config = Config::load(paths)?;
    let all = known(paths, &config)?;
    if all.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No spaces yet. Create one with `knode init <name>`.");
        }
        return Ok(());
    }
    let client = service::client();
    let mut listed = Vec::new();
    for path in all {
        let default = config.default_space.as_ref() == Some(&path);
        let manifest = Manifest::read(&path).ok();
        let settings = match &manifest {
            Some(_) => settings_of(client.as_ref(), &path).unwrap_or_default(),
            None => BTreeMap::new(),
        };
        listed.push(Listed {
            name: manifest.map(|m| m.name),
            icon: settings.get(setting_keys::SPACE_ICON).cloned(),
            capture: settings
                .get(setting_keys::CAPTURE_ENABLED)
                .is_none_or(|v| v == "on"),
            missing: !path.exists(),
            path,
            default,
        });
    }
    if json {
        let list: Vec<serde_json::Value> = listed
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name, "path": s.path, "icon": s.icon, "default": s.default,
                    "capture": s.capture, "missing": s.missing,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    let width = listed
        .iter()
        .map(|s| s.name.as_deref().map_or(0, |n| n.chars().count()))
        .max()
        .unwrap_or(0);
    for space in &listed {
        let marker = if space.default { "*" } else { " " };
        let icon = space.icon.as_deref().unwrap_or(" ");
        let name = match (&space.name, space.missing) {
            (Some(name), _) => name.clone(),
            (None, true) => "(missing)".to_string(),
            (None, false) => "(not a space)".to_string(),
        };
        let mut notes = Vec::new();
        if space.default {
            notes.push("default");
        }
        if !space.capture {
            notes.push("capture off");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        println!(
            "{marker} {icon} {name:<width$}  {}{notes}",
            space.path.display()
        );
    }
    Ok(())
}

pub fn folders(space: &Path, action: FoldersAction) -> Result<()> {
    let by = crate::person();
    let client = service::client();
    match action {
        FoldersAction::List { json } => {
            let folders = list_folders(client.as_ref(), space)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&folders)?);
                return Ok(());
            }
            if folders.is_empty() {
                println!(
                    "{} has no folders. Add a project with `knode folders add <path>`.",
                    name_of(space)
                );
                return Ok(());
            }
            for info in &folders {
                print_folder(info);
            }
            Ok(())
        }
        FoldersAction::Add {
            path,
            kind,
            capture,
            exclude,
            agent,
        } => {
            let path = crate::absolute(&path)?;
            let mut rules = serde_json::Map::new();
            if !exclude.is_empty() {
                rules.insert("exclude".into(), serde_json::json!(exclude));
            }
            match (kind, agent) {
                (Kind::Agent, Some(agent)) => {
                    rules.insert("agent".into(), serde_json::json!(agent));
                }
                (Kind::Agent, None) => {
                    bail!("an agent folder needs --agent, such as --agent claude-code")
                }
                (_, Some(_)) => bail!("--agent is only for --kind agent"),
                (_, None) => {}
            }
            let input = FolderInput {
                id: None,
                kind: kind.as_str().to_string(),
                path: path.to_string_lossy().into_owned(),
                capture: capture == Switch::On,
                rules: serde_json::Value::Object(rules),
            };
            let info = match &client {
                Some(client) => client.set_folder(space, &input, &by)?,
                None => spaces::set_folder(&mut Space::open(space)?, &input, &by)
                    .with_context(|| format!("cannot change the folders of {}", space.display()))?,
            };
            println!("{} is fed by:", name_of(space));
            print_folder(&info);
            if info.missing {
                eprintln!("warning: {} does not exist on this machine", path.display());
            }
            Ok(())
        }
        FoldersAction::Remove { folder } => {
            let folders = list_folders(client.as_ref(), space)?;
            let local = crate::absolute(Path::new(&folder))?;
            let matching: Vec<&FolderInfo> = folders
                .iter()
                .filter(|f| f.folder.id == folder || f.local.as_deref() == Some(local.as_path()))
                .collect();
            let info = match matching.as_slice() {
                [one] => *one,
                [] => bail!(
                    "{} has no folder {folder:?}; see `knode folders list`",
                    name_of(space)
                ),
                _ => bail!("more than one folder is at {folder:?}; remove it by its id"),
            };
            let id = &info.folder.id;
            match &client {
                Some(client) => client.remove_folder(space, id, &by)?,
                None => spaces::remove_folder(&mut Space::open(space)?, id, &by)?,
            };
            println!(
                "Removed the {} folder {} from {}. What was captured from it stays.",
                info.folder.kind,
                info.folder.path,
                name_of(space)
            );
            Ok(())
        }
    }
}

fn list_folders(client: Option<&Client>, space: &Path) -> Result<Vec<FolderInfo>> {
    Ok(match client {
        Some(client) => client.folders(space)?,
        None => spaces::folders(&Space::open(space)?)?,
    })
}

fn print_folder(info: &FolderInfo) {
    let folder = &info.folder;
    let mut notes = vec![if folder.capture {
        "capture on".to_string()
    } else {
        "capture off".to_string()
    }];
    if let Some(agent) = folder.rules.get("agent").and_then(|a| a.as_str()) {
        notes.push(format!("agent {agent}"));
    }
    let exclude = knode_service::spaces::rule_patterns(&folder.rules, "exclude");
    if !exclude.is_empty() {
        notes.push(format!("exclude {}", exclude.join(" ")));
    }
    if info.missing {
        notes.push("missing here".into());
    }
    println!(
        "{}  {:<7}  {}  ({})",
        folder.id,
        folder.kind,
        folder.path,
        notes.join(", ")
    );
}

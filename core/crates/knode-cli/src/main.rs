//! The knode command line.

mod connect;
mod history;
mod import;
mod markdown;
mod service;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use knode_config::{Config, Paths, ResolvedSpace, SpaceSource};
use knode_format::{Manifest, Provenance, Space};
use knode_mcp::Backend;

/// Knode: one local memory every AI agent shares.
#[derive(Debug, Parser)]
#[command(name = "knode", version, long_about = None)]
struct Cli {
    /// The space to use, a .knode path. Defaults to KNODE_SPACE, then the default space.
    #[arg(long, global = true, value_name = "PATH")]
    space: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a space, a <name>.knode file, in ~/Knode or --dir.
    Init {
        /// The name of the space.
        name: String,
        /// The folder to create it in.
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Make it the default space even if there already is one.
        #[arg(long)]
        default: bool,
    },
    /// Search the space and print the matching nodes.
    Search {
        /// The words to look for.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// How many nodes to print at most.
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Import a folder of Markdown files (an export or an Obsidian vault), a Markdown file, or a
    /// Claude Code or Codex session.
    Import {
        /// A folder, a .md file, or a session .jsonl file.
        path: PathBuf,
        /// The folder is an Obsidian vault (only changes the wording of the summary).
        #[arg(long)]
        vault: bool,
    },
    /// Write the space to a folder of Markdown files, one per note, with YAML frontmatter.
    Export {
        /// The folder; created if needed. Files Knode did not write are never touched.
        dir: PathBuf,
    },
    /// Show the latest changes to the space, or the history of one node.
    History {
        /// A node id or exact title. Leave out for the whole space.
        node: Option<String>,
        /// How many changes to show at most.
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
        /// Print the changes as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Undo a change, by its number from `knode history`.
    Undo {
        /// The change.
        change: i64,
        /// Undo it even if later changes touched the same notes; they are overwritten.
        #[arg(long)]
        force: bool,
    },
    /// Put a node back to an earlier version, or recover it after a delete.
    Restore {
        /// A node id or exact title.
        node: String,
        /// The version, from `knode history <node>`.
        version: i64,
    },
    /// The live Markdown folder: a folder Knode Service keeps current with every note.
    Markdown {
        #[command(subcommand)]
        action: MarkdownAction,
    },
    /// Add Knode to an agent: claude-code, codex, gemini or claude-desktop.
    Connect {
        /// The agent.
        agent: connect::Agent,
        /// Only show what would be done.
        #[arg(long)]
        print: bool,
    },
    /// Run the MCP server on standard input and output. Agents start it themselves.
    Mcp,
    /// List the spaces Knode knows about.
    Spaces,
    /// Run, start, stop or inspect Knode Service, or start it at login.
    Service {
        #[command(subcommand)]
        action: service::Action,
    },
}

#[derive(Debug, Subcommand)]
enum MarkdownAction {
    /// Show the live Markdown folder, turn it on at <dir>, or turn it off with --off.
    Folder {
        /// The folder; created if needed.
        #[arg(conflicts_with = "off")]
        dir: Option<PathBuf>,
        /// Turn the live Markdown folder off. Its files stay as they are.
        #[arg(long)]
        off: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::from_env()?;
    match cli.command {
        Command::Init { name, dir, default } => init(&paths, &name, dir, default),
        Command::Search { query, limit, json } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            let backend = service::backend(&space)?;
            search(backend.as_ref(), &query.join(" "), limit, json)
        }
        Command::Import { path, vault } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            if path.is_dir() {
                markdown::import(&space, &absolute(&path)?, vault, &person())
            } else if vault {
                bail!(
                    "{} is not a folder: --vault takes an Obsidian vault folder",
                    path.display()
                )
            } else {
                let backend = service::backend(&space)?;
                import::run(backend.as_ref(), &path, &person())
            }
        }
        Command::Export { dir } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            markdown::export(&absolute(&space)?, &dir)
        }
        Command::History { node, limit, json } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            let backend = service::backend(&space)?;
            history::history(backend.as_ref(), node.as_deref(), limit, json)
        }
        Command::Undo { change, force } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            let backend = service::backend(&space)?;
            history::undo(backend.as_ref(), change, force, &person())
        }
        Command::Restore { node, version } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            let backend = service::backend(&space)?;
            history::restore(backend.as_ref(), &node, version, &person())
        }
        Command::Markdown {
            action: MarkdownAction::Folder { dir, off },
        } => {
            let space = existing_space(&paths, cli.space.as_deref())?;
            markdown::folder(&space, dir.as_deref(), off, &person())
        }
        Command::Connect { agent, print } => connect::run(agent, print),
        Command::Mcp => mcp(&paths, cli.space.as_deref()),
        Command::Spaces => spaces(&paths),
        Command::Service { action } => service::run(&paths, action),
    }
}

/// Who the person using the command is, for the provenance of their changes.
fn person() -> Provenance {
    let name = ["USER", "USERNAME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "me".to_string());
    Provenance::person(name)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))
}

fn resolve(paths: &Paths, flag: Option<&Path>) -> Result<ResolvedSpace> {
    let config = Config::load(paths)?;
    Ok(knode_config::resolve_space_from_env(flag, &config)?)
}

/// The space to use, which must exist.
fn existing_space(paths: &Paths, flag: Option<&Path>) -> Result<PathBuf> {
    let resolved = resolve(paths, flag)?;
    let path = &resolved.path;
    if !path.exists() {
        let from = match resolved.source {
            SpaceSource::Flag => String::new(),
            other => format!(" (from {other})"),
        };
        bail!(
            "there is no space at {}{from}; create one with `knode init <name>`",
            path.display()
        );
    }
    Ok(resolved.path)
}

fn init(paths: &Paths, name: &str, dir: Option<PathBuf>, make_default: bool) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        bail!("{name:?} cannot be a space name: use letters, numbers and spaces, such as `work`");
    }
    let dir = match dir {
        Some(dir) => absolute(&dir)?,
        None => paths.spaces_dir.clone(),
    };
    let path = dir.join(format!("{name}.{}", knode_format::EXTENSION));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    create_space(&path, name)
        .with_context(|| format!("cannot create the space at {}", path.display()))?;
    println!("Created space \"{name}\" at {}", path.display());

    let mut config = Config::load(paths)?;
    let current = config.default_space.clone().filter(|p| p.exists());
    if make_default || current.is_none() {
        config.default_space = Some(path.clone());
        config.save(paths)?;
        println!("It is now your default space.");
    } else if let Some(current) = current {
        println!(
            "Your default space is still {}. Use this one with --space {} or `knode init --default`.",
            current.display(),
            path.display()
        );
    }
    println!("Next: `knode connect claude-code` to give Claude Code this memory.");
    Ok(())
}

/// Creates a space through Knode Service, or directly when the service cannot run.
fn create_space(path: &Path, name: &str) -> Result<()> {
    match service::connect() {
        Ok(client) => {
            client.create_space(path, name)?;
        }
        Err(error) => {
            service::warn_direct(&error);
            Space::create(path, name)?;
        }
    }
    Ok(())
}

fn search(backend: &dyn Backend, query: &str, limit: usize, json: bool) -> Result<()> {
    let hits = backend.search(query, limit.max(1))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("Nothing matches \"{query}\".");
        return Ok(());
    }
    for (i, hit) in hits.iter().enumerate() {
        let node = &hit.node;
        let date = node.updated_at.to_string();
        println!(
            "{}. {}  ({}, {} by {})",
            i + 1,
            node.title,
            node.kind,
            &date[..10],
            node.updated_by.actor
        );
        let snippet = hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        if !snippet.is_empty() {
            println!("   {snippet}");
        }
        println!("   {}", node.id);
    }
    Ok(())
}

fn mcp(paths: &Paths, flag: Option<&Path>) -> Result<()> {
    let backend = match resolve(paths, flag) {
        Ok(resolved) => service::backend(&resolved.path)?,
        // The server still starts, so the agent can tell the person what to do.
        Err(error) => Box::new(knode_mcp::DirectBackend::unavailable(format!(
            "Knode has no space to use: {error:#}. Ask the user to run `knode init <name>` in a terminal."
        ))),
    };
    knode_mcp::run_stdio(backend).context("the MCP server stopped")
}

fn spaces(paths: &Paths) -> Result<()> {
    let config = Config::load(paths)?;
    let mut list: Vec<PathBuf> = config.default_space.iter().cloned().collect();
    for path in paths.spaces_in_dir()? {
        if !list.contains(&path) {
            list.push(path);
        }
    }
    if list.is_empty() {
        println!("No spaces yet. Create one with `knode init <name>`.");
        return Ok(());
    }
    for path in &list {
        let is_default = config.default_space.as_ref() == Some(path);
        let marker = if is_default { "*" } else { " " };
        let name = match Manifest::read(path) {
            Ok(manifest) => manifest.name,
            Err(_) if !path.exists() => "(missing)".to_string(),
            Err(_) => "(not a space)".to_string(),
        };
        let note = if is_default { "  (default)" } else { "" };
        println!("{marker} {name}  {}{note}", path.display());
    }
    Ok(())
}

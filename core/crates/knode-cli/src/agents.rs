//! `knode agents`: list the agents of this machine, change what they may do in each space, and
//! revoke their keys.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use knode_config::Paths;
use knode_format::{GrantLevel, Manifest};
use knode_service::AgentInfo;

use crate::connect::{self, Agent};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// List the agents of this machine and what they may do in each space.
    List,
    /// Set what an agent may do in a space: none (the space is invisible to it), read or write.
    Grant {
        /// The agent, such as claude-code.
        agent: String,
        /// The space: a .knode path, or the name of a space in ~/Knode.
        #[arg(value_name = "SPACE")]
        target: String,
        /// none, read or write.
        level: Level,
    },
    /// Revoke every key of an agent and take away its access to every space.
    Revoke {
        /// The agent, such as claude-code.
        agent: String,
    },
}

/// What an agent may do in a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    /// Nothing: the space is invisible to it.
    None,
    /// Read only.
    Read,
    /// Read and write.
    Write,
}

impl From<Level> for GrantLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::None => GrantLevel::None,
            Level::Read => GrantLevel::Read,
            Level::Write => GrantLevel::Write,
        }
    }
}

pub fn run(paths: &Paths, action: Action) -> Result<()> {
    match action {
        Action::List => list(),
        Action::Grant {
            agent,
            target,
            level,
        } => grant(paths, &agent, &target, level.into()),
        Action::Revoke { agent } => revoke(paths, &agent),
    }
}

/// The agent's name in Knode: `gemini` is `gemini-cli`.
fn agent_name(arg: &str) -> String {
    Agent::find(arg).map_or_else(|| arg.to_string(), |a| a.name().to_string())
}

/// A space given as a path, or as the name of a space in ~/Knode.
fn space_path(paths: &Paths, arg: &str) -> Result<PathBuf> {
    let looks_like_path = arg.contains('/') || arg.ends_with(knode_format::EXTENSION);
    let path = if looks_like_path {
        crate::absolute(Path::new(arg))?
    } else {
        paths.space_path(arg)
    };
    if !path.exists() {
        bail!("there is no space at {}", path.display());
    }
    Ok(path)
}

fn space_name(path: &Path) -> String {
    Manifest::read(path).map_or_else(|_| path.display().to_string(), |m| m.name)
}

fn describe(level: GrantLevel) -> &'static str {
    match level {
        GrantLevel::None => "cannot see",
        GrantLevel::Read => "can read",
        GrantLevel::Write => "can read and write",
    }
}

/// Prints what an agent may do in each space it was given.
pub fn print_access(agent: &AgentInfo) {
    let mut any = false;
    for grant in &agent.spaces {
        let Some(level) = grant.level else {
            continue;
        };
        any = true;
        println!(
            "{} {} {} ({}).",
            agent.name,
            describe(level),
            space_name(&grant.space),
            grant.space.display()
        );
    }
    if !any {
        println!("{} cannot see any space.", agent.name);
    }
}

fn list() -> Result<()> {
    let client = crate::service::connect().context("Knode Service could not start")?;
    let agents = client.agents()?;
    if agents.is_empty() {
        println!("No agents yet. Connect one with `knode connect claude-code`.");
    }
    for agent in &agents {
        let key = if agent.has_valid_key() {
            "has a key"
        } else {
            "revoked"
        };
        println!("{} ({}, {key})", agent.name, agent.kind);
        for grant in &agent.spaces {
            let level = grant.level.map_or("cannot be opened now", |l| l.as_str());
            println!(
                "  {}  {}  {level}",
                space_name(&grant.space),
                grant.space.display()
            );
        }
        if agent.spaces.is_empty() {
            println!("  no spaces");
        }
    }
    let unkeyed: Vec<Agent> = connect::without_key()
        .into_iter()
        .filter(|a| !agents.iter().any(|known| known.name == a.name()))
        .collect();
    if !unkeyed.is_empty() {
        println!();
        println!(
            "Set up before agents had their own keys, and stopped until connected again: {}.",
            unkeyed
                .iter()
                .map(|a| a.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for agent in unkeyed {
            println!(
                "  `knode connect {}` gives it a key and write access to your default space.",
                agent.arg()
            );
        }
    }
    Ok(())
}

fn grant(paths: &Paths, agent: &str, space: &str, level: GrantLevel) -> Result<()> {
    let name = agent_name(agent);
    let path = space_path(paths, space)?;
    let client = crate::service::connect().context("Knode Service could not start")?;
    client.set_grant(&name, &path, level, &crate::person())?;
    println!(
        "{name} now {} {} ({}).",
        describe(level),
        space_name(&path),
        path.display()
    );
    Ok(())
}

fn revoke(paths: &Paths, agent: &str) -> Result<()> {
    let name = agent_name(agent);
    let client = crate::service::connect().context("Knode Service could not start")?;
    client.revoke_agent(&name, &crate::person())?;
    let store = connect::store(paths);
    store
        .delete(&name)
        .with_context(|| format!("cannot remove the key of {name} from {}", store.describe()))?;
    println!(
        "Revoked the keys of {name}: it cannot see any space now. `knode connect {}` connects it \
         again.",
        Agent::find(&name).map_or(name.as_str(), |a| a.arg())
    );
    Ok(())
}

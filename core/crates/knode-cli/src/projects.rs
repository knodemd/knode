//! `knode projects`: the projects of a space, and moving one, with its notes, links and history,
//! to another space. Through Knode Service, or the spaces opened directly when it cannot run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use knode_config::Paths;
use knode_format::{Change, ChangeId, Manifest, MovePreview, NodeId, Project, Provenance, Space};

use crate::service;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// List the projects of the space, each under the project it is part of.
    List {
        /// Print the projects as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Move a project to another space, with the projects inside it, their notes, files, links
    /// and history, and the project folders that hold them. `knode undo` takes the move back.
    Move {
        /// The project: its id or exact title.
        project: String,
        /// The space it moves to: its name or its .knode path.
        #[arg(long, value_name = "SPACE")]
        to: String,
        /// Only show what would move, and what would not.
        #[arg(long)]
        dry_run: bool,
        /// Print what moved, or would move, as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(paths: &Paths, space: &Path, action: Option<Action>, by: &Provenance) -> Result<()> {
    match action.unwrap_or(Action::List { json: false }) {
        Action::List { json } => list(space, json),
        Action::Move {
            project,
            to,
            dry_run,
            json,
        } => {
            let to = crate::spaces::find(paths, &to)?;
            move_project(space, &project, &to, dry_run, json, by)
        }
    }
}

fn list(space: &Path, json: bool) -> Result<()> {
    let projects = service::backend(space)?.projects()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(());
    }
    if projects.is_empty() {
        println!("The space has no projects.");
        return Ok(());
    }
    let mut children: BTreeMap<Option<NodeId>, Vec<&Project>> = BTreeMap::new();
    let ids: Vec<NodeId> = projects.iter().map(|p| p.node.id).collect();
    for project in &projects {
        // A parent that is not a project of the list shows the project at the top.
        let parent = project.parent.filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(project);
    }
    fn print(children: &BTreeMap<Option<NodeId>, Vec<&Project>>, at: Option<NodeId>, depth: usize) {
        for project in children.get(&at).into_iter().flatten() {
            println!(
                "{}{}  ({} notes)  {}",
                "  ".repeat(depth),
                project.node.title,
                project.notes,
                project.node.id
            );
            print(children, Some(project.node.id), depth + 1);
        }
    }
    print(&children, None, 0);
    Ok(())
}

/// The project named by `arg` in the space: its id, or its title compared without regard to
/// case.
fn find_project(space: &Path, arg: &str) -> Result<NodeId> {
    let projects = service::backend(space)?.projects()?;
    let arg = arg.trim();
    if let Ok(id) = NodeId::parse(arg)
        && projects.iter().any(|p| p.node.id == id)
    {
        return Ok(id);
    }
    let wanted = arg.to_lowercase();
    let found: Vec<&Project> = projects
        .iter()
        .filter(|p| p.node.title.to_lowercase() == wanted)
        .collect();
    match found.as_slice() {
        [one] => Ok(one.node.id),
        [] => bail!("the space has no project \"{arg}\"; `knode projects` lists them"),
        many => bail!(
            "more than one project is named \"{arg}\": {}; give its id",
            many.iter()
                .map(|p| p.node.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn open(path: &Path) -> Result<Space> {
    Space::open(path).with_context(|| format!("cannot open the space at {}", path.display()))
}

fn move_project(
    space: &Path,
    project: &str,
    to: &Path,
    dry_run: bool,
    json: bool,
    by: &Provenance,
) -> Result<()> {
    let space = crate::absolute(space)?;
    let id = find_project(&space, project)?;
    let client = service::client();
    let preview = match &client {
        Some(client) => client.preview_move(&space, &id, to, by)?,
        None => open(&space)?.preview_move(&id, &open(to)?)?,
    };
    if dry_run {
        if json {
            println!("{}", serde_json::to_string_pretty(&preview)?);
        } else {
            print_preview(&preview, true);
        }
        return Ok(());
    }
    if !preview.conflicts.is_empty() {
        print_preview(&preview, true);
        bail!("\"{}\" was not moved", preview.project.title);
    }
    let moved = match &client {
        Some(client) => client.move_project(&space, &id, to, by)?,
        None => {
            let mut target = open(to)?;
            open(&space)?.move_project(&id, &mut target, by)?
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&moved)?);
        return Ok(());
    }
    print_preview(&moved.preview, false);
    println!(
        "It is change {} in {} and change {} in {}; `knode undo {} --space {:?}` takes the move \
         back in both.",
        moved.from_change,
        moved.preview.from.name,
        moved.to_change,
        moved.preview.to.name,
        moved.from_change,
        moved.preview.from.name
    );
    Ok(())
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn print_preview(preview: &MovePreview, dry_run: bool) {
    let verb = if dry_run { "Would move" } else { "Moved" };
    println!(
        "{verb} \"{}\" from {} to {}:",
        preview.project.title, preview.from.name, preview.to.name
    );
    let projects: Vec<&str> = preview.projects.iter().map(|p| p.title.as_str()).collect();
    println!(
        "  {} ({}), {}, {}",
        plural(preview.projects.len() as u64, "project", "projects"),
        projects.join(", "),
        plural(preview.notes, "note", "notes"),
        plural(preview.versions, "version", "versions"),
    );
    if preview.attachments > 0 {
        println!(
            "  {} ({} bytes)",
            plural(preview.attachments, "attached file", "attached files"),
            preview.attachment_bytes
        );
    }
    println!(
        "  {} among them, kept",
        plural(preview.links_kept, "link", "links")
    );
    if !preview.crossing.is_empty() {
        println!(
            "  {} to notes that stay, which do not cross spaces ([[links]] stay as text):",
            plural(preview.crossing.len() as u64, "link", "links")
        );
        for link in &preview.crossing {
            println!(
                "    \"{}\" {} \"{}\"",
                link.from_title, link.kind, link.to_title
            );
        }
    }
    for folder in &preview.folders {
        println!(
            "  Project folder {}: its conversations go to {} from now on",
            folder.path, preview.to.name
        );
    }
    for staying in &preview.stays {
        println!("  Stays: {} ({})", staying.what, staying.reason);
    }
    for conflict in &preview.conflicts {
        println!("  Cannot move: {conflict}");
    }
}

/// The space Knode knows with this id.
fn space_with_id(paths: &Paths, id: &str) -> Result<Option<PathBuf>> {
    Ok(crate::spaces::known_spaces(paths)?
        .into_iter()
        .find(|path| Manifest::read(path).is_ok_and(|m| m.id == id)))
}

/// Undoes a move, given its `project.move` change in `space`: in both spaces at once.
pub fn undo_move(paths: &Paths, space: &Path, change: &Change, by: &Provenance) -> Result<()> {
    let space = crate::absolute(space)?;
    let mine = Manifest::read(&space)?.id;
    let side = |key: &str| {
        let at = |field: &str| {
            change.detail[key][field]
                .as_str()
                .map(str::to_string)
                .unwrap_or_default()
        };
        (at("id"), at("name"))
    };
    let (from, to) = (side("from"), side("to"));
    let (other_id, other_name) = if from.0 == mine { to } else { from };
    let other = space_with_id(paths, &other_id)?.ok_or_else(|| {
        anyhow!(
            "this change moved a project between this space and {other_name}, which Knode does \
             not know here; add it to Knode, then undo again"
        )
    })?;
    let id: ChangeId = change.id;
    let undone = match service::client() {
        Some(client) => client.undo_move(&space, id, &other, by)?,
        None => {
            let mut other = open(&other)?;
            open(&space)?.undo_move(id, &mut other, by)?
        }
    };
    println!(
        "Undid the move of {} from {} to {}: it is back in {}, and gone from {} (changes {} and {}).",
        plural(undone.nodes.len() as u64, "node", "nodes"),
        undone.from.name,
        undone.to.name,
        undone.from.name,
        undone.to.name,
        undone.from_change.id,
        undone.to_change.id
    );
    Ok(())
}

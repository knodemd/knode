//! `knode history`, `knode undo` and `knode restore`: the changes of a space, through Knode
//! Service or the space opened directly.

use anyhow::{Result, anyhow, bail};
use knode_format::{Change, ChangeId, HistoryEntry, NodeId, Provenance};
use knode_mcp::{Backend, BackendError};

/// A node by id, even a deleted one, or by its exact title.
fn resolve(backend: &dyn Backend, node: &str) -> Result<NodeId> {
    let node = node.trim();
    if let Ok(id) = NodeId::parse(node) {
        return Ok(id);
    }
    match backend.find_by_title(node)?.first() {
        Some(found) => Ok(found.id),
        None => bail!("no node has the id or title \"{node}\"; `knode search` finds it"),
    }
}

fn who(by: &Provenance) -> String {
    format!("{} ({})", by.actor, by.kind.as_str())
}

/// A node's title for a change subject, or the subject itself.
fn subject(backend: &dyn Backend, change: &Change) -> Result<String> {
    if change.subject.is_empty() {
        return Ok(String::new());
    }
    Ok(match NodeId::parse(&change.subject) {
        Ok(id) => match backend.get_node(&id)? {
            Some(node) => format!("\"{}\"", node.title),
            None => format!("{id} (deleted)"),
        },
        Err(_) => change.subject.clone(),
    })
}

/// Prints the changes of the space, or of one node with the versions they wrote.
pub fn history(backend: &dyn Backend, node: Option<&str>, limit: usize, json: bool) -> Result<()> {
    let limit = limit.clamp(1, 1000);
    let Some(node) = node else {
        let changes = backend.changes(limit, None)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&changes)?);
            return Ok(());
        }
        if changes.is_empty() {
            println!("No changes yet.");
            return Ok(());
        }
        for change in &changes {
            print_change(change, &subject(backend, change)?, None);
        }
        return Ok(());
    };
    let id = resolve(backend, node)?;
    let changes = backend.changes_of(&id.to_string(), limit)?;
    let versions = backend.history(&id)?;
    if json {
        let out = serde_json::json!({ "node": id, "changes": changes, "versions": versions });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if changes.is_empty() && versions.is_empty() {
        bail!("node {id} has no history");
    }
    let title = match backend.get_node(&id)? {
        Some(node) => format!("\"{}\"", node.title),
        None => match versions.last() {
            Some(last) => format!("\"{}\" (deleted)", last.title),
            None => "(purged)".to_string(),
        },
    };
    println!("{title}  {id}");
    for change in &changes {
        let version = versions
            .iter()
            .rfind(|v| v.change_id == Some(change.id) && v.node_id == id);
        print_change(change, "", version);
    }
    if changes.len() == limit {
        println!("(the latest {limit}; -n shows more)");
    }
    println!(
        "Undo a change with `knode undo <change>`; put the node back to a version with \
         `knode restore {id} <version>`."
    );
    Ok(())
}

fn print_change(change: &Change, subject: &str, version: Option<&HistoryEntry>) {
    let date = change.changed_at.to_string();
    let mut line = format!(
        "  change {:<6} {}  {:<12}",
        change.id,
        date[..16].replace('T', " "),
        change.action
    );
    if !subject.is_empty() {
        line.push_str(&format!(" {subject}"));
    }
    let mut line = line.trim_end().to_string();
    line.push_str(&format!("  by {}", who(&change.changed_by)));
    if let Some(version) = version {
        line.push_str(&format!("  version {}", version.seq));
    }
    if let Some(undoes) = change.undoes {
        line.push_str(&format!("  (undoes change {undoes})"));
    }
    println!("{line}");
}

/// Undoes a change, as the person using the command.
pub fn undo(backend: &dyn Backend, change: ChangeId, force: bool, by: &Provenance) -> Result<()> {
    let Some(detail) = backend.change(change)? else {
        bail!("there is no change {change}; `knode history` lists them");
    };
    let undo = backend.undo(change, force, by).map_err(|e| match e {
        BackendError::Invalid(message) if message.contains("would overwrite later changes") => {
            anyhow!(
                "{message}\nThose changes are lost if you undo it: check them with `knode \
                 history`, then run `knode undo {change} --force` to undo it anyway."
            )
        }
        other => other.into(),
    })?;
    let what = subject(backend, &detail.change)?;
    let what = if what.is_empty() {
        String::new()
    } else {
        format!(" of {what}")
    };
    println!(
        "Undid change {change} ({}{what}). The undo is change {}; `knode undo {}` redoes it.",
        detail.change.action, undo.id, undo.id
    );
    Ok(())
}

/// Puts a node back to one of its versions, or recovers it after a delete.
pub fn restore(backend: &dyn Backend, node: &str, version: i64, by: &Provenance) -> Result<()> {
    let id = resolve(backend, node)?;
    let was_deleted = backend.get_node(&id)?.is_none();
    let node = backend.restore(&id, version, by)?;
    if was_deleted {
        println!(
            "Recovered \"{}\" ({id}) from version {version}.",
            node.title
        );
    } else {
        println!("Restored \"{}\" ({id}) to version {version}.", node.title);
    }
    Ok(())
}

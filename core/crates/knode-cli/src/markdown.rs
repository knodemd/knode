//! `knode export` and `knode markdown folder`: Markdown in and out of a
//! space, through Knode Service or the space opened directly.

use std::path::Path;

use anyhow::{Context, Result, bail};
use knode_format::{Provenance, Space};
use knode_service::{DeletedNote, MarkdownFolder};
use knode_vault::ExportReport;

use crate::service;

/// Writes the space to `dir` as Markdown files.
pub fn export(space: &Path, dir: &Path) -> Result<()> {
    let dir = crate::absolute(dir)?;
    if dir.starts_with(space) {
        bail!(
            "{} is inside the space; export to a folder outside it",
            dir.display()
        );
    }
    let report: ExportReport = match service::client() {
        Some(client) => client.export_markdown(space, &dir)?,
        None => knode_vault::export(&Space::open(space)?, &dir)?,
    };
    println!(
        "Exported {} to {}: {} written, {} unchanged, {} removed.",
        plural(report.files, "note"),
        dir.display(),
        report.written.len(),
        report.unchanged,
        report.removed.len()
    );
    Ok(())
}

/// Shows the live Markdown folder of the space, or turns it on at `dir` or off.
pub fn folder(space: &Path, dir: Option<&Path>, off: bool, by: &Provenance) -> Result<()> {
    let dir = dir.map(crate::absolute).transpose()?;
    let change = if off { Some(None) } else { dir.map(Some) };
    let client = service::client();
    let folder: MarkdownFolder = match (&client, &change) {
        (Some(client), None) => client.markdown_folder(space)?,
        (Some(client), Some(dir)) => client.set_markdown_folder(space, dir.as_deref(), by)?,
        (None, None) => knode_service::markdown::live_folder(&Space::open(space)?)?,
        (None, Some(dir)) => {
            let mut opened = Space::open(space)?;
            knode_service::markdown::set_live_folder(&mut opened, dir.as_deref(), by)
                .with_context(|| format!("cannot change the settings of {}", space.display()))?
        }
    };
    match (&folder.path, &change) {
        (None, Some(_)) => println!("The live Markdown folder is off. Its files stay as they are."),
        (None, None) => {
            println!("The live Markdown folder is off. `knode markdown folder <dir>` turns it on.")
        }
        (Some(path), Some(_)) => {
            println!("The live Markdown folder is now {}.", path.display());
            if client.is_some() {
                println!(
                    "Every note is written there as a .md file and kept current while Knode Service runs."
                );
            } else {
                println!("Knode Service writes it when it runs: `knode service start`.");
            }
        }
        (Some(path), None) => println!("The live Markdown folder is {}.", path.display()),
    }
    if let Some(warning) = &folder.warning {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Lists the notes whose file was deleted from the live Markdown folder, or keeps or deletes
/// one of them (by id or title) or `all`.
pub fn deleted(
    space: &Path,
    keep: Option<&str>,
    delete: Option<&str>,
    by: &Provenance,
) -> Result<()> {
    let client = service::client();
    let mut opened = match client {
        Some(_) => None,
        None => Some(Space::open(space)?),
    };
    let notes = match (&client, &opened) {
        (Some(client), _) => client.deleted_notes(space)?,
        (None, Some(opened)) => knode_service::markdown::deleted_notes(opened)?,
        (None, None) => unreachable!(),
    };
    let (target, keeping) = match (keep, delete) {
        (Some(node), _) => (node, true),
        (None, Some(node)) => (node, false),
        (None, None) => {
            if notes.is_empty() {
                println!("No note waits: no file was deleted from the live Markdown folder.");
            }
            for note in &notes {
                println!("{}  {}  (was {})", note.id, note.title, note.path);
            }
            return Ok(());
        }
    };
    let chosen: Vec<&DeletedNote> = if target == "all" {
        notes.iter().collect()
    } else {
        let found: Vec<&DeletedNote> = notes
            .iter()
            .filter(|n| n.id.to_string() == target || n.title.eq_ignore_ascii_case(target))
            .collect();
        if found.is_empty() {
            bail!("no note named {target:?} waits; `knode markdown deleted` lists them");
        }
        found
    };
    for note in &chosen {
        match (&client, &mut opened) {
            (Some(client), _) if keeping => {
                client.keep_deleted(space, &note.id)?;
            }
            (Some(client), _) => client.delete_node(space, &note.id, by)?,
            (None, Some(opened)) if keeping => {
                knode_service::markdown::keep_deleted(opened, &note.id)?;
            }
            (None, Some(opened)) => opened.delete_node(&note.id, by)?,
            (None, None) => unreachable!(),
        }
    }
    if keeping {
        println!(
            "Kept {}; Knode Service writes their files again.",
            plural(chosen.len(), "note")
        );
    } else {
        println!(
            "Deleted {}. Their history keeps them: `knode restore` recovers one.",
            plural(chosen.len(), "note")
        );
    }
    Ok(())
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

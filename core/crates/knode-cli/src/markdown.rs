//! `knode export`, `knode import <folder>` and `knode markdown folder`: Markdown in and out of a
//! space, through Knode Service or the space opened directly.

use std::path::Path;

use anyhow::{Context, Result, bail};
use knode_format::{Provenance, Space};
use knode_service::MarkdownFolder;
use knode_vault::{ExportReport, ImportReport};

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

/// Reads every `.md` file under `dir` into the space: a folder written by `knode export`, or an
/// Obsidian vault with `vault`, which only changes the wording.
pub fn import(space: &Path, dir: &Path, vault: bool, by: &Provenance) -> Result<()> {
    let report: ImportReport = match service::client() {
        Some(client) => client.import_markdown(space, dir, by)?,
        None => knode_vault::import(&mut Space::open(space)?, dir, by)?,
    };
    for skipped in &report.skipped {
        eprintln!("skipped {}: {}", skipped.path, skipped.reason);
    }
    let what = if vault {
        format!("the vault {}", dir.display())
    } else {
        dir.display().to_string()
    };
    let mut summary = format!(
        "Imported {what}: {} new, {} updated, {} unchanged",
        report.created.len(),
        report.updated.len(),
        report.unchanged
    );
    if !report.skipped.is_empty() {
        summary.push_str(&format!(", {} skipped", report.skipped.len()));
    }
    println!("{summary}.");
    if !report.attachments.is_empty() {
        println!(
            "Left out {} that {} not Markdown, such as images.",
            plural(report.attachments.len(), "file"),
            if report.attachments.len() == 1 {
                "is"
            } else {
                "are"
            }
        );
    }
    let read = report.created.len() + report.updated.len() + report.unchanged;
    if read == 0 && !report.skipped.is_empty() {
        bail!("nothing could be imported");
    }
    if read == 0 {
        bail!("{} has no Markdown (.md) files", dir.display());
    }
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

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

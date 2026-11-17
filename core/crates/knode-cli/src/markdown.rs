//! `knode export` and `knode markdown folder`: Markdown in and out of a
//! space, through Knode Service or the space opened directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use knode_format::{Provenance, Space};
use knode_service::{DeletedNote, MarkdownFolder};
use knode_vault::ExportReport;

use crate::service;

/// How many differences of each kind `--compare` lists one by one.
const LISTED: usize = 20;

/// Writes the space to `dir` as Markdown files, then compares them with `compare`, the folder
/// that was imported, when given.
pub fn export(space: &Path, dir: &Path, compare: Option<&Path>, json: bool) -> Result<()> {
    let source = compare.map(crate::absolute).transpose()?;
    if let Some(source) = &source
        && !source.is_dir()
    {
        bail!(
            "{} is not a folder: --compare takes the folder that was imported",
            source.display()
        );
    }
    let dir = crate::absolute(dir)?;
    if dir.starts_with(space) {
        bail!(
            "{} is inside the space; export to a folder outside it",
            dir.display()
        );
    }
    if let Some(source) = &source {
        let (out, vault) = (resolved(&dir), resolved(source));
        if out.starts_with(&vault) || vault.starts_with(&out) {
            bail!(
                "{} is {} the folder to compare with, {}: export to a folder apart from it, so the \
                 export never writes into it",
                dir.display(),
                if out == vault {
                    "the same folder as"
                } else if out.starts_with(&vault) {
                    "inside"
                } else {
                    "a folder that contains"
                },
                source.display()
            );
        }
    }
    let report: ExportReport = match service::client() {
        Some(client) => client.export_markdown(space, &dir)?,
        None => knode_vault::export(&Space::open(space)?, &dir)?,
    };
    let summary = format!(
        "Exported {} to {}: {} written, {} unchanged, {} removed.",
        plural(report.files, "note"),
        dir.display(),
        report.written.len(),
        report.unchanged,
        report.removed.len()
    );
    let Some(source) = source else {
        println!("{summary}");
        return Ok(());
    };
    let comparison = knode_vault::compare(&source, &dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    } else {
        println!("{summary}");
        print_comparison(&comparison);
    }
    Ok(())
}

/// `path` with its symbolic links resolved, as far as it exists: the folders it does not have
/// yet are kept as written.
fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing.iter().rev().fold(real, |p, name| p.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Prints what differs between the imported folder and the export.
fn print_comparison(c: &knode_vault::Comparison) {
    use knode_vault::Difference;
    let n = &c.counts;
    println!(
        "Compared with {}: {} identical, {} with only frontmatter changes, {} changed, {} moved, {} missing; {} only in the export; {} not imported.",
        c.source.display(),
        n.identical,
        n.metadata,
        n.changed,
        n.moved,
        n.missing,
        n.added,
        n.excluded
    );
    let mut metadata_keys = std::collections::BTreeMap::<String, usize>::new();
    for note in c
        .notes
        .iter()
        .filter(|n| n.difference == Difference::Metadata)
    {
        for key in note
            .added_keys
            .iter()
            .chain(&note.removed_keys)
            .chain(&note.changed_keys)
        {
            *metadata_keys.entry(key.clone()).or_default() += 1;
        }
    }
    if !metadata_keys.is_empty() {
        let keys: Vec<String> = metadata_keys
            .iter()
            .map(|(k, n)| format!("{k} ({n})"))
            .collect();
        println!("  Frontmatter keys that differ: {}.", keys.join(", "));
    }
    let listed: Vec<String> = c
        .notes
        .iter()
        .chain(&c.files)
        .filter_map(|f| match f.difference {
            Difference::Changed => Some(match f.first_changed_line {
                Some(line) => format!("changed: {} (from line {line})", f.path),
                None => format!("changed: {}", f.path),
            }),
            Difference::Moved => Some(format!(
                "moved: {} -> {}",
                f.path,
                f.exported.as_deref().unwrap_or("?")
            )),
            Difference::Missing => Some(format!("missing: {}", f.path)),
            _ => None,
        })
        .chain(c.added.iter().map(|p| format!("only in the export: {p}")))
        .chain(c.excluded.iter().map(|e| {
            format!(
                "not imported: {} ({})",
                e.path,
                e.reason.as_deref().unwrap_or("")
            )
        }))
        .collect();
    for line in listed.iter().take(LISTED) {
        println!("  {line}");
    }
    if listed.len() > LISTED {
        println!(
            "  and {} more: use --json for all of them",
            listed.len() - LISTED
        );
    }
    if c.text_preserved() {
        println!("Every note and file of the folder is in the export with the same text.");
    }
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

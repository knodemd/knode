//! `knode import`: anything Knode reads, from a path or an http(s) URL, through Knode Service
//! (`import.run`) or the space opened directly.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use knode_format::{Provenance, Space};
use knode_service::import::{self, ImportResult, ImportSource};
use knode_service::{ImportOutcome, ImportedNode};

use crate::service;

/// The kinds `--from` takes.
pub const FROM: [&str; 12] = [
    "export",
    "markdown",
    "session",
    "pdf",
    "web",
    "kindle",
    "readwise",
    "readwise-api",
    "apple-notes",
    "bear",
    "notion",
    "evernote",
];

/// How many nodes the summary lists one by one.
const LISTED: usize = 20;

/// What `knode import` was asked for.
pub struct Request<'a> {
    /// A path, or an http(s) URL; none for `--from apple-notes` or `--from bear`.
    pub target: Option<&'a str>,
    /// The kind named with `--from`.
    pub from: Option<&'a str>,
    /// `--vault`: a folder that is an Obsidian vault.
    pub vault: bool,
    /// `--json`: print the report as JSON.
    pub json: bool,
    /// The Knode home, where the credential store keeps the Readwise token.
    pub home: &'a Path,
}

/// Imports what `request` names into the space at `space`.
pub fn run(space: &Path, request: Request<'_>, by: &Provenance) -> Result<()> {
    let mut from = request
        .from
        .map(|id| ImportSource::from_id(id).ok_or_else(|| anyhow!("unknown kind {id:?}")))
        .transpose()?;
    let (path, url): (Option<PathBuf>, Option<&str>) = match request.target {
        Some(t) if import::is_url(t) => (None, Some(t.trim())),
        Some(t) => (Some(crate::absolute(Path::new(t))?), None),
        None => (None, None),
    };
    if request.vault {
        match &path {
            Some(dir) if dir.is_dir() => from = Some(ImportSource::Markdown),
            _ => bail!(
                "{} is not a folder: --vault takes an Obsidian vault folder",
                request.target.unwrap_or("the path")
            ),
        }
    }
    // Checked here first, without the space: a wrong kind or a missing file fails at once.
    let mut what = import::prepare(path.as_deref(), url, from).map_err(|e| anyhow!("{e}"))?;
    // The token of this machine; without one, Knode Service reads its own.
    let token = if what.needs_readwise_token() {
        knode_service::readwise::token(request.home)?
    } else {
        None
    };

    let mut result = match service::client() {
        Some(client) => {
            let answer = if what.needs_readwise_token() {
                client.import_readwise_api(space, token.as_deref(), by)
            } else {
                match url {
                    Some(url) => client.import_url(space, url, by),
                    None => client.import_path(space, path.as_deref(), from, by),
                }
            };
            answer.map_err(|e| match e {
                knode_service::Error::Rpc(e) => anyhow!(e.message),
                other => anyhow!(other),
            })?
        }
        None => {
            let mut space = Space::open(space)?;
            what = what.with_readwise_token(token);
            import::run(&mut space, &what, by).map_err(|e| anyhow!("{e}"))?
        }
    };
    if request.vault {
        result.format = "Obsidian vault".into();
    }
    if request.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print(&result);
    }
    if result.read() == 0 && !result.skipped.is_empty() {
        bail!("nothing could be imported");
    }
    Ok(())
}

/// Prints what the import did: one line with the counts, the nodes, what was skipped and why
/// (to standard error) and what else is worth knowing.
fn print(result: &ImportResult) {
    let mut summary = format!(
        "Imported {} ({}): {} new, {} updated, {} unchanged",
        result.from, result.format, result.created, result.updated, result.unchanged
    );
    if result.edited > 0 {
        summary.push_str(&format!(
            ", {} edited in Knode and left as {}",
            result.edited,
            if result.edited == 1 {
                "it is"
            } else {
                "they are"
            }
        ));
    }
    if result.highlights > 0 {
        summary.push_str(&format!(
            ", {} added",
            plural(result.highlights, "highlight")
        ));
    }
    if result.attached > 0 {
        summary.push_str(&format!(", {} attached", plural(result.attached, "file")));
    }
    if !result.skipped.is_empty() {
        summary.push_str(&format!(", {} skipped", result.skipped.len()));
    }
    println!("{summary}.");

    // Reading lists every document; other imports, what changed.
    let reading = matches!(
        result.source,
        ImportSource::Pdf
            | ImportSource::Web
            | ImportSource::Kindle
            | ImportSource::Readwise
            | ImportSource::ReadwiseApi
    );
    let listed: Vec<&ImportedNode> = result
        .nodes
        .iter()
        .filter(|n| reading || n.outcome != ImportOutcome::Unchanged)
        .collect();
    for node in listed.iter().take(LISTED) {
        println!("  {}", line(node));
    }
    if listed.len() > LISTED {
        println!("  and {} more", listed.len() - LISTED);
    }
    for skipped in &result.skipped {
        eprintln!("skipped {}: {}", skipped.item, skipped.reason);
    }
    for note in &result.notes {
        println!("{note}");
    }
}

fn line(node: &ImportedNode) -> String {
    let outcome = match node.outcome {
        ImportOutcome::Created => "new",
        ImportOutcome::Updated => "updated",
        ImportOutcome::Unchanged => "unchanged",
        ImportOutcome::Edited => "edited in Knode, left as it is",
    };
    let mut line = format!("{outcome}: {}", node.title);
    if node.highlights > 0 {
        line.push_str(&format!(
            " ({} added)",
            plural(node.highlights, "highlight")
        ));
    }
    if let Some(note) = &node.note {
        line.push_str(&format!(" - {note}"));
    }
    line
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

//! `knode import`: anything Knode reads, from a path or an http(s) URL, through Knode Service
//! (`import.run`) or the space opened directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use knode_format::{Provenance, Space};
use knode_service::import::{self, ImportResult, ImportSource};
use knode_service::{ImportOutcome, ImportedNode};
use knode_vault::mail::MailOptions;
use knode_vault::migration::{DELETED_REASON, FileStatus, MigrationReport};

use crate::service;

/// The kinds `--from` takes.
pub const FROM: [&str; 19] = [
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
    "mbox",
    "apple-mail",
    "whatsapp",
    "telegram",
    "slack",
    "claude-code-memory",
    "media",
];

/// How many nodes the summary lists one by one.
const LISTED: usize = 20;

/// What `knode import` was asked for.
pub struct Request<'a> {
    /// A path, or an http(s) URL; none for `--from apple-notes`, `--from bear`,
    /// `--from apple-mail` or `--from claude-code-memory`.
    pub target: Option<&'a str>,
    /// The kind named with `--from`.
    pub from: Option<&'a str>,
    /// `--vault`: a folder that is an Obsidian vault.
    pub vault: bool,
    /// `--json`: print the report as JSON.
    pub json: bool,
    /// The Knode home, where the credential store keeps the Readwise token.
    pub home: &'a Path,
    /// `--folder`, `--since`, `--until` and `--attachments`, for mail.
    pub mail: MailOptions,
    /// `--report`: the file to write the migration report of a Markdown folder to.
    pub report: Option<&'a Path>,
    /// `--include-deleted`: import again the notes of a Markdown folder deleted in Knode.
    pub include_deleted: bool,
}

/// The mail options of `knode import`.
pub fn mail_options(
    folders: Vec<String>,
    since: Option<&str>,
    until: Option<&str>,
    attachments: bool,
) -> Result<MailOptions> {
    let day = |text: Option<&str>| {
        text.map(knode_vault::mail::parse_day)
            .transpose()
            .map_err(|e| anyhow!("{e}"))
    };
    Ok(MailOptions {
        folders,
        since: day(since)?,
        until: day(until)?,
        attachments,
    })
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
    let report_file = request.report.map(crate::absolute).transpose()?;
    if report_file.is_some() && !path.as_deref().is_some_and(Path::is_dir) {
        bail!("--report is for a folder of Markdown files, such as an Obsidian vault");
    }
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
    if request.include_deleted && !what.is_markdown_folder() {
        bail!("--include-deleted is for a folder of Markdown files, such as an Obsidian vault");
    }
    let mail = !request.mail.is_default();
    if mail && !what.is_mail() {
        bail!(
            "--folder, --since, --until and --attachments are for mail: --from mbox or apple-mail"
        );
    }
    // Claude Code's memory of this person, where their environment says, not the service's.
    let path = match (&path, what.source()) {
        (None, ImportSource::ClaudeCodeMemory) => Some(PathBuf::from(what.from())),
        _ => path,
    };
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
                    None if request.include_deleted => client.import_folder(
                        space,
                        path.as_deref().expect("a Markdown folder has a path"),
                        from,
                        true,
                        by,
                    ),
                    None if mail => {
                        client.import_mail(space, path.as_deref(), from, &request.mail, by)
                    }
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
            what = what
                .with_readwise_token(token)
                .with_mail_options(request.mail.clone())
                .with_include_deleted(request.include_deleted);
            import::run(&mut space, &what, by).map_err(|e| anyhow!("{e}"))?
        }
    };
    if request.vault {
        result.format = "Obsidian vault".into();
    }
    if let Some(file) = &report_file {
        let report = result
            .report
            .as_ref()
            .ok_or_else(|| anyhow!("--report is for a folder of Markdown files"))?;
        std::fs::write(file, serde_json::to_string_pretty(report)? + "\n")
            .with_context(|| format!("cannot write {}", file.display()))?;
    }
    if request.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print(&result);
        if let Some(report) = &result.report {
            print_report(report, report_file.as_deref());
        }
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

/// Prints the migration report of a Markdown folder: its counts and what did not come through,
/// one line each. Without a report file, nothing when everything came through.
fn print_report(report: &MigrationReport, file: Option<&Path>) {
    let n = &report.counts;
    let mut lines: Vec<String> = Vec::new();
    for f in &report.files {
        let label = match f.status {
            FileStatus::Edited => "edited in both, Knode's kept",
            FileStatus::Failed => "failed",
            FileStatus::Skipped if f.reason.as_deref() == Some(DELETED_REASON) => {
                lines.push(format!("deleted in Knode, not imported again: {}", f.path));
                continue;
            }
            _ => continue,
        };
        lines.push(format!(
            "{label}: {} - {}",
            f.path,
            f.reason.as_deref().unwrap_or("")
        ));
    }
    for l in &report.unresolved_links {
        lines.push(format!("unresolved link in {}: [[{}]]", l.path, l.target));
    }
    for l in &report.ambiguous_links {
        lines.push(format!(
            "ambiguous link in {}: [[{}]] -> {} of {}",
            l.path,
            l.target,
            l.resolved.as_deref().unwrap_or("?"),
            l.candidates.join(", ")
        ));
    }
    for m in &report.missing_attachments {
        lines.push(format!(
            "missing attachment in {}: {} - {}",
            m.path, m.target, m.reason
        ));
    }
    for l in &report.failed_links {
        lines.push(format!("failed typed link in {}: {}", l.path, l.reason));
    }
    // Without --report, only what needs a look.
    if file.is_none() && lines.is_empty() {
        return;
    }
    println!(
        "Report: {} new, {} updated, {} unchanged ({} edited only in Knode), {} edited in both, {} attached, {} skipped, {} failed; {}, {}, {}.",
        n.created,
        n.updated,
        n.unchanged,
        n.kept,
        n.edited,
        n.attached,
        n.skipped,
        n.failed,
        plural(report.unresolved_links.len(), "unresolved link"),
        plural(report.ambiguous_links.len(), "ambiguous link"),
        plural(report.missing_attachments.len(), "missing attachment"),
    );
    for line in lines.iter().take(LISTED) {
        println!("  {line}");
    }
    if lines.len() > LISTED {
        println!("  and {} more", lines.len() - LISTED);
    }
    match file {
        Some(file) => println!(
            "The full report, with the node of every file, is in {}.",
            file.display()
        ),
        None => {
            println!("Use --report <file.json> for the full report, with the node of every file.")
        }
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

/// Run the deterministic Markdown importer against a disposable local space.
pub fn preview(target: Option<&str>, from: Option<&str>, json: bool) -> Result<()> {
    if from.is_some_and(|f| f != "markdown") {
        bail!("--preview currently supports Markdown folders only");
    }
    let target = target.ok_or_else(|| anyhow!("--preview requires a Markdown folder path"))?;
    let path = crate::absolute(Path::new(target))?;
    if !path.is_dir() {
        bail!("--preview requires a Markdown folder path, not a URL or file");
    }
    let report = knode_vault::preview::preview(path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "Preview only: {} notes in a temporary space; source and destination unchanged.",
        report.notes.len()
    );
    println!(
        "{} skipped, {} attached, {} unused files, {} failed typed links.",
        report.import.skipped.len(),
        report.import.attached.len(),
        report.import.attachments.len(),
        report.import.unlinked.len()
    );
    for note in &report.notes {
        println!(
            "  {} → {}",
            note.path.as_deref().unwrap_or("(no path)"),
            note.title
        );
        for target in &note.unresolved {
            println!("    Unresolved: {target}");
        }
    }
    for (title, paths) in &report.duplicate_titles {
        println!("Duplicate title {title:?}: {}", paths.join(", "));
    }
    for item in report.import.skipped.iter().chain(&report.import.unlinked) {
        println!("Review {}: {}", item.path, item.reason);
    }
    for path in &report.import.attachments {
        println!("Not attached: {path}");
    }
    for warning in &report.warnings {
        println!("Review {}: {}", warning.path, warning.reason);
    }
    println!(
        "Use --json for the complete machine-readable report. This is not a round-trip fidelity certificate."
    );
    Ok(())
}

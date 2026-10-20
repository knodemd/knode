//! `knode import <export>`: a Claude, ChatGPT or Gemini (Google Takeout) data export, through
//! Knode Service or the space opened directly.

use std::path::Path;

use anyhow::{Result, bail};
use knode_format::{Provenance, Space};
use knode_service::ConversationImport;

use crate::service;

/// Whether `path` is a data export Knode reads: a `.zip` as downloaded or the folder it was
/// extracted to.
pub fn is_export(path: &Path) -> bool {
    knode_ingest::detect_export(path).is_ok()
}

/// Imports the data export at `path` into `space`.
pub fn import(space: &Path, path: &Path, by: &Provenance) -> Result<()> {
    let path = crate::absolute(path)?;
    let report = match service::client() {
        Some(client) => client.import_conversations(space, &path, by)?,
        None => {
            let mut space = Space::open(space)?;
            ConversationImport::from(&knode_ingest::import_export(&mut space, &path, by)?)
        }
    };
    for skipped in &report.skipped {
        eprintln!("skipped {}: {}", skipped.item, skipped.reason);
    }
    let mut summary = format!(
        "Imported {} from the {} at {}: {} updated, {} unchanged",
        plural(report.created, "new conversation"),
        report.format,
        path.display(),
        report.updated,
        report.unchanged
    );
    if report.projects > 0 {
        summary.push_str(&format!(", {}", plural(report.projects, "project")));
    }
    if report.empty > 0 {
        summary.push_str(&format!(", {} empty left out", report.empty));
    }
    if !report.skipped.is_empty() {
        summary.push_str(&format!(", {} skipped", report.skipped.len()));
    }
    println!("{summary}.");
    if report.created + report.updated + report.unchanged == 0 && !report.skipped.is_empty() {
        bail!("nothing could be imported");
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

//! `knode import`: a Markdown file or an agent session into a space. Folders are imported by
//! [`crate::markdown::import`].

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use knode_format::{NewNode, NodeUpdate, Provenance, kinds};
use knode_ingest::{Agent, SessionReader, to_markdown};
use knode_mcp::Backend;

/// Imports `path` into `space`: one Markdown file, or a Claude Code or Codex session file.
pub fn run(space: &dyn Backend, path: &Path, by: &Provenance) -> Result<()> {
    let path = crate::absolute(path)?;
    if path.is_file() {
        match extension(&path).as_deref() {
            Some("md" | "markdown") => import_markdown(space, &[path], by),
            Some("jsonl") => import_session(space, &path),
            _ => bail!(
                "{} is not a Markdown file or a session: knode import takes a folder of .md \
                 files, a .md file, or a Claude Code or Codex session (.jsonl)",
                path.display()
            ),
        }
    } else {
        bail!("{} does not exist", path.display())
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// The title of a Markdown note: its first heading, else the file name without extension.
pub fn markdown_title(text: &str, path: &Path) -> String {
    first_heading(text).unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Untitled".to_string())
    })
}

/// The text of the first ATX heading (`# Title` to `###### Title`) outside code fences and
/// front matter.
fn first_heading(text: &str) -> Option<String> {
    let mut fence: Option<&str> = None;
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if matches!(line.trim_end(), "---" | "...") {
                break;
            }
        }
    }
    for line in lines {
        let trimmed = line.trim_start();
        if line.len() - trimmed.len() > 3 {
            continue;
        }
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let rest = &trimmed[level..];
        if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            return Some(title.to_string());
        }
    }
    None
}

fn import_markdown(space: &dyn Backend, files: &[PathBuf], by: &Provenance) -> Result<()> {
    let (mut created, mut unchanged, mut failed) = (0, 0, 0);
    for file in files {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("skipped {}: {e}", file.display());
                failed += 1;
                continue;
            }
        };
        let title = markdown_title(&text, file);
        // Importing the same file again does not make a copy.
        let mut same = false;
        for found in space.find_by_title(&title)? {
            if let Some(node) = space.get_node(&found.id)?
                && node.body == text
            {
                same = true;
                break;
            }
        }
        if same {
            unchanged += 1;
            continue;
        }
        let source = by.clone().with_source(file.display().to_string());
        match space.create_node(NewNode::note(title, text), &source) {
            Ok(_) => created += 1,
            Err(e) => {
                eprintln!("skipped {}: {e}", file.display());
                failed += 1;
            }
        }
    }
    let mut summary = format!("Imported {}", plural(created, "note"));
    if unchanged > 0 {
        summary.push_str(&format!(", {unchanged} already there"));
    }
    if failed > 0 {
        summary.push_str(&format!(", {failed} skipped"));
    }
    println!("{summary}.");
    if failed > 0 && created == 0 && unchanged == 0 {
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

/// Which agent wrote a session file, from its first records and its name.
fn detect_agent(path: &Path) -> Result<Agent> {
    let file = fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    for line in BufReader::new(file).lines().take(50) {
        let Ok(line) = line else { break };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if record.get("sessionId").is_some() {
            return Ok(Agent::ClaudeCode);
        }
        if record.get("type").and_then(|t| t.as_str()) == Some("session_meta")
            || (record.get("id").is_some() && record.get("instructions").is_some())
        {
            return Ok(Agent::Codex);
        }
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if name.starts_with("rollout-") {
        return Ok(Agent::Codex);
    }
    bail!(
        "{} is not a Claude Code or Codex session file",
        path.display()
    )
}

/// The session id in a file name: the whole name for Claude Code, the UUID at the end for
/// Codex (`rollout-<time>-<uuid>`).
fn session_id(agent: Agent, path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match agent {
        Agent::Codex => {
            let tail = stem
                .len()
                .checked_sub(36)
                .and_then(|start| stem.get(start..));
            match tail {
                Some(id) if id.chars().filter(|c| *c == '-').count() == 4 => id.to_string(),
                _ => stem,
            }
        }
        _ => stem,
    }
}

/// The Markdown of a conversation without its first line, the `# title` heading, since the
/// title is the node's own.
fn conversation_body(markdown: &str) -> String {
    match markdown.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim_start_matches('\n').to_string(),
        _ => markdown.to_string(),
    }
}

fn import_session(space: &dyn Backend, path: &Path) -> Result<()> {
    let agent = detect_agent(path)?;
    let mut reader = SessionReader::open(agent, path, session_id(agent, path));
    reader
        .poll()
        .with_context(|| format!("cannot read the session {}", path.display()))?;
    let conversation = reader.into_conversation();
    if conversation.is_empty() {
        bail!("{} has no messages to import", path.display());
    }
    let title = conversation.title();
    let body = conversation_body(&to_markdown(&conversation));
    let mut by = Provenance::agent(agent.id()).with_session(conversation.session_id.clone());
    if let Some(project) = &conversation.project {
        by = by.with_source(project.display().to_string());
    }

    // The same session imported again updates its node instead of making a copy.
    for found in space.find_by_title(&title)? {
        let Some(node) = space.get_node(&found.id)? else {
            continue;
        };
        if node.kind == kinds::CONVERSATION
            && node.created_by.session.as_deref() == Some(conversation.session_id.as_str())
        {
            let update = NodeUpdate {
                body: Some(body),
                ..Default::default()
            };
            let before = node.updated_at;
            let node = space.update_node(&node.id, update, &by)?;
            if node.updated_at == before {
                println!("\"{title}\" is already imported and unchanged.");
            } else {
                println!("Updated the {} conversation \"{title}\".", agent.name());
            }
            return Ok(());
        }
    }
    let node = space.create_node(
        NewNode::note(title.clone(), body).with_kind(kinds::CONVERSATION),
        &by,
    )?;
    println!(
        "Imported the {} conversation \"{title}\" ({}).",
        agent.name(),
        node.id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_the_first_heading() {
        let p = Path::new("/x/file name.md");
        assert_eq!(markdown_title("# Hello #\n\ntext", p), "Hello");
        assert_eq!(
            markdown_title("intro\n\n## Second level\n", p),
            "Second level"
        );
        assert_eq!(markdown_title("```\n# not this\n```\n# This\n", p), "This");
        assert_eq!(markdown_title("---\ntitle: x\n---\n# After\n", p), "After");
        assert_eq!(markdown_title("#hashtag\n", p), "file name");
        assert_eq!(markdown_title("    # code\n", p), "file name");
        assert_eq!(markdown_title("", p), "file name");
    }

    #[test]
    fn session_ids_come_from_file_names() {
        assert_eq!(
            session_id(
                Agent::Codex,
                Path::new("rollout-2026-01-02T11-00-00-0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee.jsonl")
            ),
            "0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"
        );
        assert_eq!(
            session_id(Agent::ClaudeCode, Path::new("/p/abc.jsonl")),
            "abc"
        );
    }

    #[test]
    fn conversation_body_drops_the_title() {
        assert_eq!(conversation_body("# T\n\n## User\nhi\n"), "## User\nhi\n");
        assert_eq!(conversation_body("no title"), "no title");
    }
}

//! Finding session files on disk.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::Agent;

/// How deep to look under the Codex sessions folder (`YYYY/MM/DD/file`).
const CODEX_DEPTH: usize = 4;

/// A session file an agent wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    /// The agent that wrote it.
    pub agent: Agent,
    /// Where it is.
    pub path: PathBuf,
    /// The session id taken from the file name. For Codex, the id inside the file wins.
    pub session_id: String,
    /// Its size in bytes when it was found.
    pub len: u64,
    /// When it was last written, if the file system says.
    pub modified: Option<SystemTime>,
}

/// The folder an agent keeps its sessions in, from the environment: `CLAUDE_CONFIG_DIR` or
/// `~/.claude`, then `projects`, for Claude Code; `CODEX_HOME` or `~/.codex`, then `sessions`,
/// for Codex.
pub fn default_root(agent: Agent) -> Option<PathBuf> {
    let (variable, folder, sessions) = match agent {
        Agent::ClaudeCode => ("CLAUDE_CONFIG_DIR", ".claude", "projects"),
        Agent::Codex => ("CODEX_HOME", ".codex", "sessions"),
    };
    let base = match std::env::var_os(variable) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home()?.join(folder),
    };
    Some(base.join(sessions))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Lists the session files of `agent` under `root`, sorted by path. A missing root gives an
/// empty list.
///
/// For Claude Code only the main transcripts are listed (`<root>/<project>/<session>.jsonl`);
/// subagent transcripts in `<session>/subagents/` are left out.
pub fn discover(agent: Agent, root: &Path) -> io::Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    match agent {
        Agent::ClaudeCode => {
            for project in read_dir(root)? {
                if project.is_dir() {
                    for path in read_dir(&project)? {
                        push_if_session(agent, path, &mut files);
                    }
                }
            }
        }
        Agent::Codex => walk(agent, root, CODEX_DEPTH, &mut files)?,
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Lists the session files of every agent in their default folders.
pub fn discover_all() -> io::Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    for agent in [Agent::ClaudeCode, Agent::Codex] {
        if let Some(root) = default_root(agent) {
            files.extend(discover(agent, &root)?);
        }
    }
    Ok(files)
}

/// The session file at `path`, if it is one `discover(agent, root)` would list: a `.jsonl` file
/// at the right depth under `root` that exists. `None` for anything else, such as a Claude Code
/// subagent transcript or a file that was removed.
pub(crate) fn session_file(agent: Agent, root: &Path, path: &Path) -> Option<SessionFile> {
    let relative = path.strip_prefix(root).ok()?;
    let depth = relative.components().count();
    let shaped = match agent {
        Agent::ClaudeCode => depth == 2,
        Agent::Codex => (1..=CODEX_DEPTH).contains(&depth),
    };
    if !shaped {
        return None;
    }
    let mut found = Vec::new();
    push_if_session(agent, path.to_path_buf(), &mut found);
    found.pop()
}

fn walk(agent: Agent, dir: &Path, depth: usize, files: &mut Vec<SessionFile>) -> io::Result<()> {
    for path in read_dir(dir)? {
        if path.is_dir() {
            if depth > 1 {
                walk(agent, &path, depth - 1, files)?;
            }
        } else {
            push_if_session(agent, path, files);
        }
    }
    Ok(())
}

fn read_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(|e| e.ok().map(|e| e.path())).collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn push_if_session(agent: Agent, path: PathBuf, files: &mut Vec<SessionFile>) {
    if path.extension() != Some(OsStr::new("jsonl")) {
        return;
    }
    let Ok(metadata) = fs::metadata(&path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let session_id = match agent {
        Agent::ClaudeCode => stem,
        Agent::Codex => trailing_uuid(&stem).unwrap_or(&stem).to_string(),
    };
    files.push(SessionFile {
        agent,
        path,
        session_id,
        len: metadata.len(),
        modified: metadata.modified().ok(),
    });
}

/// The UUID a Codex file name ends with (`rollout-<time>-<uuid>`).
fn trailing_uuid(stem: &str) -> Option<&str> {
    let start = stem.len().checked_sub(36)?;
    let candidate = stem.get(start..)?;
    let shaped = candidate.char_indices().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    shaped.then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_session_paths_by_depth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/projects");
        let main = root.join("-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666.jsonl");
        let sub = root.join(
            "-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666/subagents/agent-0001.jsonl",
        );
        assert!(session_file(Agent::ClaudeCode, &root, &main).is_some());
        assert!(session_file(Agent::ClaudeCode, &root, &sub).is_none());
        assert!(session_file(Agent::ClaudeCode, &root, &root.join("x/missing.jsonl")).is_none());
        assert!(session_file(Agent::ClaudeCode, Path::new("/elsewhere"), &main).is_none());
    }

    #[test]
    fn finds_the_uuid_at_the_end_of_codex_names() {
        assert_eq!(
            trailing_uuid("rollout-2026-01-02T03-04-05-0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee"),
            Some("0199aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee")
        );
        assert_eq!(trailing_uuid("short"), None);
        assert_eq!(
            trailing_uuid("rollout-without-a-uuid-at-the-end-of-its-name"),
            None
        );
    }
}

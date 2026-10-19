//! Finding session files on disk.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::Agent;

/// How deep to look under the Codex sessions folder (`YYYY/MM/DD/file`).
const CODEX_DEPTH: usize = 4;

/// How deep to look for Aider history files under an Aider root: the root itself can be a
/// project, or hold projects up to three folders down.
const AIDER_DEPTH: usize = 4;

/// Folders never searched for Aider history files.
const AIDER_SKIPPED: [&str; 4] = ["node_modules", "target", "Library", "vendor"];

/// The environment variable that names the folder to search for Aider history files.
pub const AIDER_ROOT_VARIABLE: &str = "KNODE_AIDER_ROOT";

/// A session an agent wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    /// The agent that wrote it.
    pub agent: Agent,
    /// Where it is. An OpenCode session, which lives in a database with the others, has a path
    /// of its own: `<folder>/opencode.db#<session id>`.
    pub path: PathBuf,
    /// The session id taken from the file name. For Codex, Gemini CLI, Copilot CLI and Amp, the
    /// id inside the file wins; an Aider history file holds one session per run, each with its
    /// own id.
    pub session_id: String,
    /// What has to change for the session to be read again: its size in bytes for sessions read
    /// a line at a time, and a version that changes with every write for sessions read whole
    /// (Gemini CLI `.json` files, Amp threads and OpenCode sessions).
    pub len: u64,
    /// When it was last written, if known.
    pub modified: Option<SystemTime>,
}

/// The folder an agent keeps its sessions in, from the environment:
///
/// - Claude Code: `CLAUDE_CONFIG_DIR` or `~/.claude`, then `projects`;
/// - Codex: `CODEX_HOME` or `~/.codex`, then `sessions`;
/// - Gemini CLI: `GEMINI_CLI_HOME` or the home folder, then `.gemini/tmp`;
/// - Copilot CLI: `COPILOT_HOME` or `~/.copilot`, then `session-state`;
/// - OpenCode: `XDG_DATA_HOME` or `~/.local/share`, then `opencode` (the folder holding
///   `opencode.db`);
/// - Amp: `XDG_DATA_HOME` or `~/.local/share`, then `amp/threads`;
/// - Aider, which keeps its history in each project rather than in one folder: the folder named
///   by `KNODE_AIDER_ROOT` ([`AIDER_ROOT_VARIABLE`]), searched up to four levels down; `None`
///   when it is not set.
///
/// The same folders are used on macOS and Linux.
pub fn default_root(agent: Agent) -> Option<PathBuf> {
    let root = match agent {
        Agent::ClaudeCode => env_dir("CLAUDE_CONFIG_DIR")
            .or_else(|| Some(home()?.join(".claude")))?
            .join("projects"),
        Agent::Codex => env_dir("CODEX_HOME")
            .or_else(|| Some(home()?.join(".codex")))?
            .join("sessions"),
        Agent::GeminiCli => env_dir("GEMINI_CLI_HOME")
            .or_else(home)?
            .join(".gemini")
            .join("tmp"),
        Agent::CopilotCli => env_dir("COPILOT_HOME")
            .or_else(|| Some(home()?.join(".copilot")))?
            .join("session-state"),
        Agent::OpenCode => data_home()?.join("opencode"),
        Agent::Amp => data_home()?.join("amp").join("threads"),
        Agent::Aider => env_dir(AIDER_ROOT_VARIABLE)?,
    };
    Some(root)
}

fn env_dir(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    env_dir("HOME").or_else(|| env_dir("USERPROFILE"))
}

/// `XDG_DATA_HOME` when it is an absolute path, else `~/.local/share`.
fn data_home() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME")
        .filter(|dir| dir.is_absolute())
        .or_else(|| Some(home()?.join(".local").join("share")))
}

/// Lists the sessions of `agent` under `root`, sorted by path. A missing root gives an empty
/// list.
///
/// For Claude Code only the main transcripts are listed (`<root>/<project>/<session>.jsonl`);
/// subagent transcripts in `<session>/subagents/` are left out, as are Gemini CLI subagent
/// sessions and OpenCode child sessions.
pub fn discover(agent: Agent, root: &Path) -> io::Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    if agent == Agent::OpenCode {
        files = crate::opencode::list(root)?;
    } else {
        walk(agent, root, root, max_depth(agent), &mut files)?;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Lists the sessions of every agent in their default folders.
pub fn discover_all() -> io::Result<Vec<SessionFile>> {
    let mut files = Vec::new();
    for agent in Agent::ALL {
        if let Some(root) = default_root(agent) {
            files.extend(discover(agent, &root)?);
        }
    }
    Ok(files)
}

/// The session file at `path`, if it is one `discover(agent, root)` would list and it exists.
/// `None` for anything else, such as a Claude Code subagent transcript or a file that was
/// removed.
pub(crate) fn session_file(agent: Agent, root: &Path, path: &Path) -> Option<SessionFile> {
    let relative = path.strip_prefix(root).ok()?;
    if !shaped(agent, relative) {
        return None;
    }
    let mut found = Vec::new();
    push_if_session(agent, path.to_path_buf(), &mut found);
    found.pop()
}

/// The sessions to read again after `path` changed under `root`: the session file itself, or
/// for OpenCode, every session when the database changed.
pub(crate) fn changed_sessions(
    agent: Agent,
    root: &Path,
    path: &Path,
) -> io::Result<Vec<SessionFile>> {
    if agent == Agent::OpenCode {
        let in_root = path.parent() == Some(root);
        return if in_root && crate::opencode::is_database_file(path) {
            crate::opencode::list(root)
        } else {
            Ok(Vec::new())
        };
    }
    Ok(session_file(agent, root, path).into_iter().collect())
}

/// How many folders down from the root session files are.
fn max_depth(agent: Agent) -> usize {
    match agent {
        Agent::ClaudeCode | Agent::CopilotCli => 2,
        Agent::Codex => CODEX_DEPTH,
        Agent::GeminiCli => 3,
        Agent::Aider => AIDER_DEPTH,
        _ => 1,
    }
}

/// Whether a path relative to the root has the shape of a session file of `agent`.
fn shaped(agent: Agent, relative: &Path) -> bool {
    let depth = relative.components().count();
    let name = relative
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let extension = relative.extension().and_then(OsStr::to_str);
    match agent {
        Agent::ClaudeCode => depth == 2 && extension == Some("jsonl"),
        Agent::Codex => (1..=CODEX_DEPTH).contains(&depth) && extension == Some("jsonl"),
        Agent::GeminiCli => {
            depth == 3
                && relative
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|parent| parent == "chats")
                && name.starts_with("session-")
                && matches!(extension, Some("json" | "jsonl"))
        }
        Agent::CopilotCli => depth == 2 && name == "events.jsonl",
        Agent::Amp => depth == 1 && extension == Some("json"),
        Agent::Aider => (1..=AIDER_DEPTH).contains(&depth) && name == crate::aider::HISTORY_FILE,
        _ => false,
    }
}

/// Whether the session is read whole each time it changes, rather than a line at a time.
pub(crate) fn read_whole(agent: Agent, path: &Path) -> bool {
    match agent {
        Agent::Amp | Agent::OpenCode => true,
        Agent::GeminiCli => path.extension() == Some(OsStr::new("json")),
        _ => false,
    }
}

/// A number that changes whenever the file is written: its modification time in nanoseconds,
/// mixed with its size so that a write within the clock's resolution still counts.
pub(crate) fn file_version(metadata: &fs::Metadata) -> u64 {
    let nanos = metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as u64);
    nanos ^ metadata.len().rotate_left(48)
}

fn walk(
    agent: Agent,
    root: &Path,
    dir: &Path,
    depth: usize,
    files: &mut Vec<SessionFile>,
) -> io::Result<()> {
    for path in read_dir(dir)? {
        if path.is_dir() {
            if depth > 1 && !skipped(agent, &path) {
                let result = walk(agent, root, &path, depth - 1, files);
                // An Aider root is a broad folder: one that cannot be read is left out.
                if agent != Agent::Aider {
                    result?;
                }
            }
        } else if path
            .strip_prefix(root)
            .is_ok_and(|relative| shaped(agent, relative))
        {
            push_if_session(agent, path, files);
        }
    }
    Ok(())
}

/// Folders an Aider search never enters: hidden ones and dependency or build folders.
fn skipped(agent: Agent, dir: &Path) -> bool {
    if agent != Agent::Aider {
        return false;
    }
    let name = dir.file_name().and_then(OsStr::to_str).unwrap_or_default();
    name.starts_with('.') || AIDER_SKIPPED.contains(&name)
}

fn read_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.filter_map(|e| e.ok().map(|e| e.path())).collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn push_if_session(agent: Agent, path: PathBuf, files: &mut Vec<SessionFile>) {
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
        Agent::Codex => trailing_uuid(&stem).unwrap_or(&stem).to_string(),
        Agent::CopilotCli => path
            .parent()
            .and_then(Path::file_name)
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or(stem),
        Agent::Aider => crate::aider::file_session_id(&path),
        _ => stem,
    };
    let len = if read_whole(agent, &path) {
        file_version(&metadata)
    } else {
        metadata.len()
    };
    files.push(SessionFile {
        agent,
        path,
        session_id,
        len,
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

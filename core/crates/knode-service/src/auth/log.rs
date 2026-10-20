//! The access log of this machine: `$KNODE_HOME/access.log`, one JSON line per call an agent
//! makes, with the time, the agent, the space, the method and the outcome. Never keys, node
//! content or search queries.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// The access log in the Knode home.
pub const ACCESS_LOG_FILE: &str = "access.log";
/// A log larger than this is moved to `access.log.1`, replacing the one there.
const LOG_LIMIT: u64 = 4 * 1024 * 1024;

/// What happened to a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    Allowed,
    Denied,
    Invisible,
}

#[derive(Serialize)]
struct Entry<'a> {
    at: i64,
    agent: Option<&'a str>,
    space: Option<&'a str>,
    method: &'a str,
    outcome: Outcome,
}

#[derive(Debug)]
pub(crate) struct AccessLog {
    file: PathBuf,
    writing: Mutex<()>,
}

impl AccessLog {
    pub fn new(home: &Path) -> Self {
        AccessLog {
            file: home.join(ACCESS_LOG_FILE),
            writing: Mutex::new(()),
        }
    }

    /// Records one call. A log that cannot be written never stops the call.
    pub fn record(&self, agent: Option<&str>, space: Option<&str>, method: &str, outcome: Outcome) {
        let entry = Entry {
            at: knode_format::Timestamp::now().0,
            agent,
            space,
            method,
            outcome,
        };
        let Ok(mut line) = serde_json::to_string(&entry) else {
            return;
        };
        line.push('\n');
        let _guard = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        if fs::metadata(&self.file).is_ok_and(|m| m.len() > LOG_LIMIT) {
            let _ = fs::rename(&self.file, self.file.with_extension("log.1"));
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.file);
        if let Ok(mut file) = file {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

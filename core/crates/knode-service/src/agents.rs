//! The agents of a machine and what they may do: the shapes of `auth`, `agents.list`,
//! `agents.add`, `agents.revoke` and `grants.set`. See `PROTOCOL.md`.

use std::path::PathBuf;

use knode_format::{GrantLevel, Provenance};
use serde::{Deserialize, Serialize};

/// Who a connection acts for: the result of `auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "lowercase")]
pub enum Identity {
    /// The person who owns the Knode home: the app and the CLI. Not limited by grants.
    Person,
    /// An agent, known by its key. Every call is checked against its grants.
    Agent {
        /// The agent's id, a ULID, the same in every space.
        id: String,
        /// The agent's name, the actor of its changes.
        name: String,
        /// The program, such as `claude-code`.
        kind: String,
    },
}

/// One key of an agent, by its fingerprint. The key itself never leaves the machine's
/// credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// `sha256:` and 64 lower case hexadecimal digits.
    pub fingerprint: String,
    /// When the key was made, ms UTC.
    pub created_at: i64,
    /// When the key was revoked, ms UTC, or `None` while it is valid.
    #[serde(default)]
    pub revoked_at: Option<i64>,
}

/// What an agent may do in one space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceGrant {
    /// The space, as the service opened it.
    pub space: PathBuf,
    /// The agent's level there, or `None` when the space cannot be opened now.
    pub level: Option<GrantLevel>,
}

/// An agent of this machine: the result of `agents.list`, `agents.add` and `agents.revoke`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// The agent's id, a ULID, the same in every space.
    pub id: String,
    /// The agent's name, such as `claude-code`.
    pub name: String,
    /// The program, such as `claude-code`.
    pub kind: String,
    /// When it was added on this machine, ms UTC.
    pub created_at: i64,
    /// Its keys, oldest first, revoked ones included.
    pub keys: Vec<KeyInfo>,
    /// The spaces it was given a level in, with the level each has now.
    pub spaces: Vec<SpaceGrant>,
}

impl AgentInfo {
    /// Whether the agent has a key that is not revoked.
    pub fn has_valid_key(&self) -> bool {
        self.keys.iter().any(|k| k.revoked_at.is_none())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct AuthParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentAddParams {
    pub name: String,
    pub kind: String,
    pub fingerprint: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentRevokeParams {
    pub agent: String,
    pub by: Provenance,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GrantSetParams {
    pub agent: String,
    pub space: PathBuf,
    pub level: GrantLevel,
    pub by: Provenance,
}

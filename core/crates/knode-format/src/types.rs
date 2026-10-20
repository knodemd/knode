//! The values stored in a space.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{Error, Result};

/// The id of a node: a [ULID](https://github.com/ulid/spec), 26 characters of Crockford
/// base32, ordered by creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeId(Ulid);

impl NodeId {
    /// Parses a node id. Lower case is accepted; ids are always written in upper case.
    pub fn parse(text: &str) -> Result<Self> {
        Ulid::from_string(text.trim())
            .map(NodeId)
            .map_err(|_| Error::Invalid(format!("{text:?} is not a node id (a ULID)")))
    }

    /// When the id was made.
    pub fn created_at(&self) -> Timestamp {
        Timestamp(self.0.timestamp_ms() as i64)
    }

    pub(crate) fn from_ulid(ulid: Ulid) -> Self {
        NodeId(ulid)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for NodeId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        NodeId::parse(s)
    }
}

impl TryFrom<String> for NodeId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        NodeId::parse(&value)
    }
}

impl From<NodeId> for String {
    fn from(id: NodeId) -> Self {
        id.to_string()
    }
}

/// A moment in time: milliseconds since the Unix epoch, in UTC. This is how every timestamp
/// is stored in `knode.db`. [`Display`](fmt::Display) writes it as RFC 3339, such as
/// `2026-10-20T09:30:00.000Z`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The current time.
    pub fn now() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Timestamp(ms)
    }

    /// Milliseconds since the Unix epoch.
    pub fn as_millis(&self) -> i64 {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.rem_euclid(1000);
        let secs = self.0.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }
}

/// Converts days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian calendar.
/// Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Who made a change: a person, an agent or Knode itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    /// A person, such as the owner of the space.
    Person,
    /// An AI agent, such as `claude-code` or `codex`.
    Agent,
    /// Knode itself, for changes no one asked for directly.
    System,
}

impl ActorKind {
    /// The value stored in `provenance.actor_kind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::Person => "person",
            ActorKind::Agent => "agent",
            ActorKind::System => "system",
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "person" => ActorKind::Person,
            "agent" => ActorKind::Agent,
            _ => ActorKind::System,
        }
    }
}

/// Who or what made a change, and from where. Every write takes one.
///
/// ```
/// use knode_format::Provenance;
///
/// let me = Provenance::person("Ángel");
/// let agent = Provenance::agent("claude-code")
///     .with_session("8f14e45f")
///     .with_source("~/Developer/Knode/knode");
/// assert_eq!(agent.actor, "claude-code");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Provenance {
    /// A person, an agent or Knode itself.
    pub kind: ActorKind,
    /// The name of the person or agent, such as `claude-code` or `codex`.
    pub actor: String,
    /// The conversation or session the change came from, such as an agent session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Where the content came from, such as a file path, a URL or a project folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Provenance {
    /// A change made by a person.
    pub fn person(name: impl Into<String>) -> Self {
        Self::new(ActorKind::Person, name)
    }

    /// A change made by an agent, such as `claude-code` or `codex`.
    pub fn agent(name: impl Into<String>) -> Self {
        Self::new(ActorKind::Agent, name)
    }

    /// A change made by Knode itself.
    pub fn system() -> Self {
        Self::new(ActorKind::System, "knode")
    }

    /// A change made by `actor` of the given kind.
    pub fn new(kind: ActorKind, actor: impl Into<String>) -> Self {
        Provenance {
            kind,
            actor: actor.into(),
            session: None,
            source: None,
        }
    }

    /// Sets the conversation or session the change came from.
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Sets where the content came from: a file path, a URL, a project folder.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// A node: one note, idea, conversation or any other piece of the space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// The node id.
    pub id: NodeId,
    /// What the node is, such as `note` or `conversation`. See [`kinds`](crate::kinds).
    pub kind: String,
    /// The title, shown in place of every `[[link]]` to the node.
    pub title: String,
    /// The text, in Markdown.
    pub body: String,
    /// Tags, lower case, sorted.
    pub tags: Vec<String>,
    /// When the node was created.
    pub created_at: Timestamp,
    /// When the node last changed.
    pub updated_at: Timestamp,
    /// Who created the node.
    pub created_by: Provenance,
    /// Who changed the node last.
    pub updated_by: Provenance,
}

/// A node without its body, for lists: search results, recent nodes, links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSummary {
    /// The node id.
    pub id: NodeId,
    /// What the node is.
    pub kind: String,
    /// The title.
    pub title: String,
    /// When the node was created.
    pub created_at: Timestamp,
    /// When the node last changed.
    pub updated_at: Timestamp,
    /// Who changed the node last.
    pub updated_by: Provenance,
}

/// A node to create with [`Space::create_node`](crate::Space::create_node).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NewNode {
    /// What the node is; empty means `note`.
    #[serde(default)]
    pub kind: String,
    /// The title; it cannot be empty.
    pub title: String,
    /// The text, in Markdown.
    #[serde(default)]
    pub body: String,
    /// Tags. They are trimmed, lower cased and stripped of a leading `#`.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl NewNode {
    /// A note with a title and a Markdown body.
    pub fn note(title: impl Into<String>, body: impl Into<String>) -> Self {
        NewNode {
            kind: crate::kinds::NOTE.to_string(),
            title: title.into(),
            body: body.into(),
            tags: Vec::new(),
        }
    }

    /// Sets the kind.
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    /// Sets the tags.
    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }
}

/// A node written elsewhere, such as in another space, to create with
/// [`Space::import_node`](crate::Space::import_node) keeping its id, dates and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedNode {
    /// The id the node keeps.
    pub id: NodeId,
    /// Its kind, title, body and tags.
    #[serde(flatten)]
    pub node: NewNode,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it last changed; not earlier than `created_at`.
    pub updated_at: Timestamp,
    /// Who created it.
    pub created_by: Provenance,
    /// Who changed it last.
    pub updated_by: Provenance,
}

/// The changes to make with [`Space::update_node`](crate::Space::update_node). Fields left as
/// `None` stay as they are.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeUpdate {
    /// A new kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// A new title. Every `[[Old title]]` link in other nodes is rewritten to the new title.
    #[serde(default)]
    pub title: Option<String>,
    /// A new Markdown body.
    #[serde(default)]
    pub body: Option<String>,
    /// New tags, replacing the old ones.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// The properties of a node: structured metadata by key, such as the agent and session of a
/// conversation. See [`property_keys`](crate::property_keys).
pub type Properties = BTreeMap<String, String>;

/// A typed link from one node to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// The node the link starts from.
    pub from: NodeId,
    /// The title of the node the link starts from.
    pub from_title: String,
    /// The node the link points at.
    pub to: NodeId,
    /// The title of the node the link points at.
    pub to_title: String,
    /// The type of link, such as `mentions` or `related`. See [`link_kinds`](crate::link_kinds).
    pub kind: String,
    /// When the link was made.
    pub created_at: Timestamp,
    /// Who made the link.
    pub created_by: Provenance,
}

/// What a change in the history did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeOp {
    /// The node was created.
    Create,
    /// The node was changed.
    Update,
    /// The node was deleted.
    Delete,
}

impl ChangeOp {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ChangeOp::Create => "create",
            ChangeOp::Update => "update",
            ChangeOp::Delete => "delete",
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "create" => ChangeOp::Create,
            "delete" => ChangeOp::Delete,
            _ => ChangeOp::Update,
        }
    }
}

/// One version of a node in its history: the node as it was right after the change (or, for a
/// delete, right before it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The order of the change in the space; it only grows.
    pub seq: i64,
    /// The node.
    pub node_id: NodeId,
    /// What the change did.
    pub op: ChangeOp,
    /// When the change was made.
    pub changed_at: Timestamp,
    /// Who made the change.
    pub changed_by: Provenance,
    /// The kind of the node.
    pub kind: String,
    /// The title of the node.
    pub title: String,
    /// The Markdown body of the node.
    pub body: String,
    /// The tags of the node.
    pub tags: Vec<String>,
    /// The change the version belongs to (see [`Space::change`](crate::Space::change)).
    /// `None` only in spaces written before schema version 3 that were never migrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<ChangeId>,
    /// When the version puts back an earlier one (an undo or a restore), the `seq` of the
    /// version whose content it puts back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<i64>,
}

/// The id of a change: its order in the space. It only grows.
pub type ChangeId = i64;

/// One change: one write that a person or an agent asked for, made in one transaction. What
/// [`Space::undo`](crate::Space::undo) works on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The order of the change in the space; it only grows.
    pub id: ChangeId,
    /// When the change was made.
    pub changed_at: Timestamp,
    /// Who made the change.
    pub changed_by: Provenance,
    /// What was done, such as `node.update` or `undo`. See [`actions`](crate::actions).
    pub action: String,
    /// What it was done to: a node id, a folder id, an agent id, a setting key, a fingerprint,
    /// or empty.
    pub subject: String,
    /// For an `undo`, the change it undoes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<ChangeId>,
    /// Details of the action, a JSON object.
    pub detail: serde_json::Value,
}

/// A link made or removed by hand, as kept in the history of links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHistoryEntry {
    /// The order of the row in the space.
    pub seq: i64,
    /// The change the row belongs to.
    pub change_id: ChangeId,
    /// [`ChangeOp::Create`] when the link was made, [`ChangeOp::Delete`] when it was removed.
    pub op: ChangeOp,
    /// The node the link starts from. It may no longer exist.
    pub from: NodeId,
    /// The type of link.
    pub kind: String,
    /// The node the link points at. It may no longer exist.
    pub to: NodeId,
}

/// A change with everything it wrote: the versions of nodes and the links made or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDetail {
    /// The change.
    pub change: Change,
    /// The versions of nodes the change wrote, in order.
    pub versions: Vec<HistoryEntry>,
    /// The links the change made or removed by hand, in order.
    pub links: Vec<LinkHistoryEntry>,
}

/// A folder on disk that feeds the space (schema version 3). Groundwork: the rules and the
/// routing of capture are applied by Knode Service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// The folder's id, a ULID.
    pub id: String,
    /// What the folder is: `project`, `notes` or `agent`. See [`folder_kinds`](crate::folder_kinds).
    pub kind: String,
    /// Where it is: relative to the home folder as `~/...`, or absolute.
    pub path: String,
    /// Whether the folder is read and captured.
    pub capture: bool,
    /// The rules of the folder, a JSON object.
    pub rules: serde_json::Value,
    /// When it was added.
    pub created_at: Timestamp,
    /// Who added it.
    pub created_by: Provenance,
    /// When it last changed.
    pub updated_at: Timestamp,
    /// Who changed it last.
    pub updated_by: Provenance,
}

/// A folder to add or change with [`Space::set_folder`](crate::Space::set_folder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSpec {
    /// The id of the folder to change, or `None` to add a new one (or change the folder with the
    /// same kind and path).
    #[serde(default)]
    pub id: Option<String>,
    /// What the folder is.
    pub kind: String,
    /// Where it is, already recorded as the specification says (`~/...` or absolute).
    pub path: String,
    /// Whether the folder is read and captured.
    pub capture: bool,
    /// The rules of the folder, a JSON object.
    #[serde(default = "empty_object")]
    pub rules: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

/// An agent that may have access to the space (schema version 3). Groundwork: Knode Service
/// checks keys and grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// The agent's id, a ULID, the same in every space.
    pub id: String,
    /// The agent's name, used as the actor of its changes.
    pub name: String,
    /// The program, such as `claude-code` or `codex`.
    pub kind: String,
    /// When it was added to this space.
    pub created_at: Timestamp,
    /// Who added it.
    pub created_by: Provenance,
}

/// The fingerprint of an agent's key. The key itself is never stored in a space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentKey {
    /// `sha256:` and 64 lower case hexadecimal digits.
    pub fingerprint: String,
    /// The agent the key belongs to.
    pub agent_id: String,
    /// When the key was made.
    pub created_at: Timestamp,
    /// When the key was revoked, or `None` while it is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<Timestamp>,
}

/// What an agent may do in a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantLevel {
    /// Nothing: the space is invisible to the agent.
    None,
    /// Read only.
    Read,
    /// Read and write.
    Write,
}

impl GrantLevel {
    /// The value stored in `grants.level`.
    pub fn as_str(&self) -> &'static str {
        match self {
            GrantLevel::None => "none",
            GrantLevel::Read => "read",
            GrantLevel::Write => "write",
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "read" => GrantLevel::Read,
            "write" => GrantLevel::Write,
            _ => GrantLevel::None,
        }
    }
}

/// A node found by [`Space::search`](crate::Space::search).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// The node.
    pub node: NodeSummary,
    /// A short piece of the text around the words found, with each match in `**bold**` and
    /// `…` where text was cut.
    pub snippet: String,
    /// How well the node matches; higher is better. Only comparable within one search.
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_display_as_rfc3339() {
        assert_eq!(Timestamp(0).to_string(), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            Timestamp(1_792_489_800_123).to_string(),
            "2026-10-20T09:50:00.123Z"
        );
        assert_eq!(
            Timestamp(951_782_400_000).to_string(),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(Timestamp(-1).to_string(), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn node_ids_round_trip() {
        let id = NodeId::parse("01JABCDEFGHJKMNPQRSTVWXYZ0").unwrap();
        assert_eq!(id.to_string(), "01JABCDEFGHJKMNPQRSTVWXYZ0");
        assert_eq!(NodeId::parse("01jabcdefghjkmnpqrstvwxyz0").unwrap(), id);
        assert!(NodeId::parse("not an id").is_err());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"01JABCDEFGHJKMNPQRSTVWXYZ0\"");
        assert_eq!(serde_json::from_str::<NodeId>(&json).unwrap(), id);
    }
}

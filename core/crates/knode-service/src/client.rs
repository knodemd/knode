//! [`Client`]: talk to Knode Service from any process.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use knode_format::{
    ChangeDetail, ChangeId, GrantLevel, HistoryEntry, Link, NewNode, Node, NodeId, NodeSummary,
    NodeUpdate, Provenance, SearchHit, SearchMode,
};
use knode_vault::{ExportReport, ImportReport};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::agents::{
    AgentAddParams, AgentInfo, AgentRevokeParams, AuthParams, GrantSetParams, Identity, SpaceGrant,
};
use crate::keys::{OWNER_TOKEN_FILE, read_owner_token};
use crate::protocol::*;
use crate::search::{IndexRunParams, IndexStatus, SearchResults};
use crate::spaces::{FolderInfo, FolderInput, SpaceEvent};
use crate::{Error, Result, paths};

mod import;
mod maps;

/// How long [`connect_or_spawn`] waits for a new service to listen.
pub const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

type Reply = std::result::Result<Value, RpcError>;

/// How a [`Client`] proves who it acts for when it connects.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// The person who owns the Knode home, with the owner token found next to the socket
    /// (`owner.token`, which only its owner can read). The app and the CLI connect like this.
    Owner,
    /// An agent, with its key (`knk_` and 64 hexadecimal digits).
    Agent(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Owner => f.write_str("Owner"),
            // Never print a key.
            Credential::Agent(_) => f.write_str("Agent(..)"),
        }
    }
}

/// A connection to Knode Service.
///
/// Every method sends one request and waits for its answer. A client can be shared between
/// threads; requests from one client run in the order they are sent. Space paths may be
/// relative: the client makes them absolute against the current folder before sending them.
///
/// ```no_run
/// use knode_format::{NewNode, Provenance};
///
/// let client = knode_service::Client::connect_default()?;
/// let space = std::path::Path::new("/Users/me/Knode/Personal.knode");
/// let me = Provenance::person("Ángel");
/// let node = client.create_node(space, NewNode::note("Plan", "Ship v0.1."), &me)?;
/// let changes = client.subscribe(Some(space))?;
/// for event in changes {
///     println!("{event:?}");
/// }
/// # Ok::<(), knode_service::Error>(())
/// ```
#[derive(Debug)]
pub struct Client {
    shared: Arc<Shared>,
    reader: Option<thread::JoinHandle<()>>,
    identity: Option<Identity>,
}

#[derive(Debug)]
struct Subscriber {
    key: u64,
    space: Option<PathBuf>,
    events: mpsc::Sender<Event>,
}

#[derive(Debug)]
struct Shared {
    stream: Mutex<UnixStream>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Client {
    /// Connects to the service listening on `socket` as the person who owns the Knode home
    /// (see [`Credential::Owner`]).
    pub fn connect(socket: impl AsRef<Path>) -> Result<Client> {
        Client::connect_with(socket, &Credential::Owner)
    }

    /// Connects to the service listening on `socket` as an agent, with its key.
    pub fn connect_as_agent(socket: impl AsRef<Path>, key: &str) -> Result<Client> {
        Client::connect_with(socket, &Credential::Agent(key.to_string()))
    }

    /// Connects to the service listening on `socket` and authenticates with `credential`.
    /// Fails with [`Error::Rpc`] when the service refuses the credential, such as an unknown
    /// or revoked key. A service too old to know `auth` is used as it is.
    pub fn connect_with(socket: impl AsRef<Path>, credential: &Credential) -> Result<Client> {
        let socket = socket.as_ref();
        let mut client = Client::connect_unauthenticated(socket)?;
        let params = match credential {
            Credential::Owner => {
                let token = read_owner_token(&socket.with_file_name(OWNER_TOKEN_FILE))
                    .ok()
                    .flatten();
                match token {
                    Some(owner) => AuthParams {
                        owner: Some(owner),
                        key: None,
                    },
                    // No token to show: the service will say what is missing on each call.
                    None => return Ok(client),
                }
            }
            Credential::Agent(key) => AuthParams {
                key: Some(key.trim().to_string()),
                owner: None,
            },
        };
        match client.request::<Identity>(methods::AUTH, params) {
            Ok(identity) => client.identity = Some(identity),
            Err(Error::Rpc(e)) if e.code == codes::METHOD_NOT_FOUND => {}
            Err(e) => return Err(e),
        }
        Ok(client)
    }

    /// Who the service says this connection acts for, once it has authenticated.
    pub fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Connects without authenticating: the service answers nothing but `auth`.
    pub fn connect_unauthenticated(socket: impl AsRef<Path>) -> Result<Client> {
        let stream = UnixStream::connect(socket.as_ref())?;
        let read = stream.try_clone()?;
        let shared = Arc::new(Shared {
            stream: Mutex::new(stream),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        let reader = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("knode-service-client".into())
                .spawn(move || read_messages(read, &shared))?
        };
        Ok(Client {
            shared,
            reader: Some(reader),
            identity: None,
        })
    }

    /// Connects to the service of the default Knode home, see [`knode_home`](crate::knode_home).
    pub fn connect_default() -> Result<Client> {
        Client::connect(paths::socket_path()?)
    }

    /// Sends any method with JSON params and returns the JSON result.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.shared.pending).insert(id, tx);
        if self.shared.closed.load(Ordering::SeqCst) {
            lock(&self.shared.pending).remove(&id);
            return Err(Error::Disconnected);
        }
        let mut line =
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        let sent = lock(&self.shared.stream).write_all(line.as_bytes());
        if let Err(e) = sent {
            lock(&self.shared.pending).remove(&id);
            return Err(e.into());
        }
        match rx.recv() {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(Error::Rpc(error)),
            Err(_) => Err(Error::Disconnected),
        }
    }

    fn request<T: DeserializeOwned>(&self, method: &str, params: impl Serialize) -> Result<T> {
        let value = self.call(method, serde_json::to_value(params)?)?;
        Ok(serde_json::from_value(value)?)
    }

    /// What the service is doing: its version, process id, open spaces and clients.
    pub fn status(&self) -> Result<Status> {
        self.request(methods::SERVICE_STATUS, json!({}))
    }

    /// Creates a space at `path`, which must end in `.knode` and must not exist.
    pub fn create_space(&self, path: impl AsRef<Path>, name: &str) -> Result<SpaceInfo> {
        let params = SpaceCreateParams {
            space: absolute(path.as_ref())?,
            name: name.to_string(),
        };
        self.request(methods::SPACE_CREATE, params)
    }

    /// Opens the space at `path` in the service, or returns it if it is already open.
    pub fn open_space(&self, path: impl AsRef<Path>) -> Result<SpaceInfo> {
        let params = SpaceParams {
            space: space_path(path.as_ref())?,
        };
        self.request(methods::SPACE_OPEN, params)
    }

    /// Renames a space: its name in `manifest.json`. Subscribers hear a `space` event. Only a
    /// person can rename a space.
    pub fn rename_space(
        &self,
        space: impl AsRef<Path>,
        name: &str,
        by: &Provenance,
    ) -> Result<SpaceInfo> {
        let params = SpaceRenameParams {
            space: space_path(space.as_ref())?,
            name: name.to_string(),
            by: by.clone(),
        };
        self.request(methods::SPACE_RENAME, params)
    }

    /// Every setting of a space, by key, such as `space.icon`. Absent keys have their default.
    pub fn settings(&self, space: impl AsRef<Path>) -> Result<BTreeMap<String, String>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::SETTINGS_GET, params)
    }

    /// Changes a setting of a space (`space.icon` or `capture.enabled`), or puts it back to its
    /// default with `None`. Returns every setting as it is now. Only a person can change
    /// settings.
    pub fn set_setting(
        &self,
        space: impl AsRef<Path>,
        key: &str,
        value: Option<&str>,
        by: &Provenance,
    ) -> Result<BTreeMap<String, String>> {
        let params = SettingsSetParams {
            space: space_path(space.as_ref())?,
            key: key.to_string(),
            value: value.map(str::to_string),
            by: by.clone(),
        };
        self.request(methods::SETTINGS_SET, params)
    }

    /// The folders that feed a space, oldest first.
    pub fn folders(&self, space: impl AsRef<Path>) -> Result<Vec<FolderInfo>> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::FOLDERS_LIST, params)
    }

    /// Adds a folder to a space, or changes one. Only a person can change folders.
    pub fn set_folder(
        &self,
        space: impl AsRef<Path>,
        folder: &FolderInput,
        by: &Provenance,
    ) -> Result<FolderInfo> {
        let params = FolderSetParams {
            space: space_path(space.as_ref())?,
            folder: folder.clone(),
            by: by.clone(),
        };
        self.request(methods::FOLDERS_SET, params)
    }

    /// Removes a folder from a space, by its id. Returns whether there was one. Only a person
    /// can change folders.
    pub fn remove_folder(
        &self,
        space: impl AsRef<Path>,
        id: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let params = FolderRemoveParams {
            space: space_path(space.as_ref())?,
            id: id.to_string(),
            by: by.clone(),
        };
        self.request(methods::FOLDERS_REMOVE, params)
    }

    /// Creates a node.
    pub fn create_node(
        &self,
        space: impl AsRef<Path>,
        node: NewNode,
        by: &Provenance,
    ) -> Result<Node> {
        let params = NodeCreateParams {
            space: space_path(space.as_ref())?,
            kind: node.kind,
            title: node.title,
            body: node.body,
            tags: node.tags,
            by: by.clone(),
        };
        self.request(methods::NODE_CREATE, params)
    }

    /// Reads a node, or `None` when no node has this id.
    pub fn get_node(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Option<Node>> {
        self.request(methods::NODE_GET, self.node_params(space.as_ref(), id)?)
    }

    /// The nodes with this title, compared without regard to case or extra spaces, oldest
    /// first. A `[[link]]` with this title points at the first one.
    pub fn find_by_title(&self, space: impl AsRef<Path>, title: &str) -> Result<Vec<NodeSummary>> {
        let params = TitleParams {
            space: space_path(space.as_ref())?,
            title: title.to_string(),
        };
        self.request(methods::NODE_FIND_BY_TITLE, params)
    }

    /// Changes a node and returns it as it is now.
    pub fn update_node(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        update: NodeUpdate,
        by: &Provenance,
    ) -> Result<Node> {
        let params = NodeUpdateParams {
            space: space_path(space.as_ref())?,
            id: *id,
            update,
            version: None,
            by: by.clone(),
        };
        self.request(methods::NODE_UPDATE, params)
    }

    /// Changes a node like [`update_node`](Self::update_node), with a new body merged from
    /// `version`, the version of its document the caller read with [`open_doc`](Self::open_doc):
    /// what others wrote since is kept, not replaced. Returns the node and, when the body was
    /// written through its document, [`DocEdited`] (with the body now as `text` when others
    /// wrote since `version`).
    pub fn update_node_from(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        update: NodeUpdate,
        version: Option<&str>,
        by: &Provenance,
    ) -> Result<(Node, Option<DocEdited>)> {
        let params = NodeUpdateParams {
            space: space_path(space.as_ref())?,
            id: *id,
            update,
            version: version.map(str::to_string),
            by: by.clone(),
        };
        let value = self.call(methods::NODE_UPDATE, serde_json::to_value(params)?)?;
        let edited = match (value.get("version"), value.get("seen")) {
            (Some(Value::String(version)), Some(Value::String(seen))) => Some(DocEdited {
                text: (version != seen)
                    .then(|| {
                        value
                            .get("body")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .flatten(),
                version: version.clone(),
                seen: seen.clone(),
            }),
            _ => None,
        };
        Ok((serde_json::from_value(value)?, edited))
    }

    /// Opens a node's body for editing together: its text and the version of its document,
    /// made from the body the first time.
    pub fn open_doc(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<DocText> {
        self.request(
            methods::DOC_OPEN,
            json!({ "space": space_path(space.as_ref())?, "id": id }),
        )
    }

    /// Edits a node's body with splices made in order on the text at `version` (the current
    /// text with `None`), merged with what others wrote since.
    pub fn edit_doc(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        version: Option<&str>,
        splices: &[DocSplice],
        by: &Provenance,
    ) -> Result<DocEdited> {
        self.request(
            methods::DOC_EDIT,
            json!({
                "space": space_path(space.as_ref())?,
                "id": id,
                "version": version,
                "splices": splices,
                "by": by,
            }),
        )
    }

    /// Replaces a node's body with `text`, diffed from the text at `version` (the current text
    /// with `None`), so what others wrote since is kept.
    pub fn write_doc(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        version: Option<&str>,
        text: &str,
        by: &Provenance,
    ) -> Result<DocEdited> {
        self.request(
            methods::DOC_EDIT,
            json!({
                "space": space_path(space.as_ref())?,
                "id": id,
                "version": version,
                "text": text,
                "by": by,
            }),
        )
    }

    /// Deletes a node.
    pub fn delete_node(&self, space: impl AsRef<Path>, id: &NodeId, by: &Provenance) -> Result<()> {
        let params = NodeDeleteParams {
            space: space_path(space.as_ref())?,
            id: *id,
            by: by.clone(),
        };
        self.call(methods::NODE_DELETE, serde_json::to_value(params)?)?;
        Ok(())
    }

    /// Links two nodes with a link of the given kind, such as `related`.
    pub fn link(
        &self,
        space: impl AsRef<Path>,
        from: &NodeId,
        to: &NodeId,
        kind: &str,
        by: &Provenance,
    ) -> Result<Link> {
        let params = LinkCreateParams {
            space: space_path(space.as_ref())?,
            from: *from,
            to: *to,
            kind: kind.to_string(),
            by: by.clone(),
        };
        self.request(methods::LINK_CREATE, params)
    }

    /// Removes a link, recorded as a change by `by`. Returns whether there was one.
    pub fn unlink(
        &self,
        space: impl AsRef<Path>,
        from: &NodeId,
        to: &NodeId,
        kind: &str,
        by: &Provenance,
    ) -> Result<bool> {
        let params = LinkDeleteParams {
            space: space_path(space.as_ref())?,
            from: *from,
            to: *to,
            kind: kind.to_string(),
            by: Some(by.clone()),
        };
        self.request(methods::LINK_DELETE, params)
    }

    /// The links that start from a node.
    pub fn links_from(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<Link>> {
        self.request(methods::LINKS_FROM, self.node_params(space.as_ref(), id)?)
    }

    /// The links that point at a node.
    pub fn backlinks(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<Link>> {
        self.request(methods::LINKS_BACK, self.node_params(space.as_ref(), id)?)
    }

    /// The `[[links]]` in a node's body that name no node yet, as written.
    pub fn unresolved_links(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<String>> {
        self.request(
            methods::LINKS_UNRESOLVED,
            self.node_params(space.as_ref(), id)?,
        )
    }

    /// Searches titles and text, best match first.
    pub fn search(
        &self,
        space: impl AsRef<Path>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let params = SearchParams {
            space: space_path(space.as_ref())?,
            query: query.to_string(),
            limit,
            mode: None,
        };
        self.request(methods::SEARCH, params)
    }

    /// Searches by words, by meaning or both ([`SearchMode::Hybrid`]), best first. The results
    /// say which mode ran: words, with a note saying why, when meaning cannot be used yet.
    pub fn search_with_mode(
        &self,
        space: impl AsRef<Path>,
        query: &str,
        mode: SearchMode,
        limit: usize,
    ) -> Result<SearchResults> {
        let params = SearchParams {
            space: space_path(space.as_ref())?,
            query: query.to_string(),
            limit,
            mode: Some(mode),
        };
        self.request(methods::SEARCH, params)
    }

    /// How far the open spaces are indexed for search by meaning, and the model. People only.
    pub fn index_status(&self) -> Result<IndexStatus> {
        self.request(methods::INDEX_STATUS, json!({}))
    }

    /// Indexes every open space for search by meaning now, even on battery, opening `space`
    /// first when given, and returns where the index is. People only.
    pub fn index_run(&self, space: Option<&Path>) -> Result<IndexStatus> {
        let params = IndexRunParams {
            space: space.map(space_path).transpose()?,
        };
        self.request(methods::INDEX_RUN, params)
    }

    /// The nodes changed most recently, newest first.
    pub fn recent(&self, space: impl AsRef<Path>, limit: usize) -> Result<Vec<NodeSummary>> {
        let params = RecentParams {
            space: space_path(space.as_ref())?,
            limit,
        };
        self.request(methods::RECENT, params)
    }

    /// Every version of a node, oldest first.
    pub fn history(&self, space: impl AsRef<Path>, id: &NodeId) -> Result<Vec<HistoryEntry>> {
        self.request(methods::HISTORY, self.node_params(space.as_ref(), id)?)
    }

    /// The changes of the space, newest first: at most `limit`, and only those older than
    /// `before` when it is set, to read a long history page by page.
    pub fn changes(
        &self,
        space: impl AsRef<Path>,
        limit: usize,
        before: Option<ChangeId>,
    ) -> Result<Vec<knode_format::Change>> {
        let params = ChangesParams {
            space: space_path(space.as_ref())?,
            subject: None,
            limit,
            before,
        };
        self.request(methods::CHANGES, params)
    }

    /// The changes made to one subject, such as a node id, newest first, at most `limit`.
    pub fn changes_of(
        &self,
        space: impl AsRef<Path>,
        subject: &str,
        limit: usize,
    ) -> Result<Vec<knode_format::Change>> {
        let params = ChangesParams {
            space: space_path(space.as_ref())?,
            subject: Some(subject.to_string()),
            limit,
            before: None,
        };
        self.request(methods::CHANGES, params)
    }

    /// A change with the versions of nodes and the links it wrote, or `None`.
    pub fn change(&self, space: impl AsRef<Path>, id: ChangeId) -> Result<Option<ChangeDetail>> {
        let params = ChangeParams {
            space: space_path(space.as_ref())?,
            id,
        };
        self.request(methods::CHANGE, params)
    }

    /// Undoes a change and returns the change that undoes it. Without `force`, fails with the
    /// `undo_conflict` error when later changes would be overwritten.
    pub fn undo(
        &self,
        space: impl AsRef<Path>,
        change: ChangeId,
        force: bool,
        by: &Provenance,
    ) -> Result<knode_format::Change> {
        let params = UndoParams {
            space: space_path(space.as_ref())?,
            change,
            force,
            by: by.clone(),
        };
        self.request(methods::UNDO, params)
    }

    /// Puts a node back to one of its versions (the `seq` of a row of its history), or
    /// recovers a deleted node, and returns it.
    pub fn restore(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        version: i64,
        by: &Provenance,
    ) -> Result<Node> {
        let params = RestoreParams {
            space: space_path(space.as_ref())?,
            id: *id,
            version,
            by: by.clone(),
        };
        self.request(methods::RESTORE, params)
    }

    /// Erases a node and every version of it for good. Only a person can purge.
    pub fn purge(
        &self,
        space: impl AsRef<Path>,
        id: &NodeId,
        by: &Provenance,
    ) -> Result<knode_format::Change> {
        let params = NodeDeleteParams {
            space: space_path(space.as_ref())?,
            id: *id,
            by: by.clone(),
        };
        self.request(methods::PURGE, params)
    }

    /// Writes the space to the folder `dir` as Markdown files.
    pub fn export_markdown(
        &self,
        space: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> Result<ExportReport> {
        let params = VaultExportParams {
            space: space_path(space.as_ref())?,
            dir: absolute(dir.as_ref())?,
        };
        self.request(methods::VAULT_EXPORT, params)
    }

    /// Reads the Markdown files under `dir`, an export or an Obsidian vault, into the space.
    pub fn import_markdown(
        &self,
        space: impl AsRef<Path>,
        dir: impl AsRef<Path>,
        by: &Provenance,
    ) -> Result<ImportReport> {
        let params = VaultImportParams {
            space: space_path(space.as_ref())?,
            dir: absolute(dir.as_ref())?,
            by: by.clone(),
        };
        self.request(methods::VAULT_IMPORT, params)
    }

    /// The live Markdown folder of the space.
    pub fn markdown_folder(&self, space: impl AsRef<Path>) -> Result<MarkdownFolder> {
        let params = SpaceParams {
            space: space_path(space.as_ref())?,
        };
        self.request(methods::MARKDOWN_FOLDER_GET, params)
    }

    /// Turns the live Markdown folder of the space on at `folder`, or off with `None`. Only a
    /// person can change it.
    pub fn set_markdown_folder(
        &self,
        space: impl AsRef<Path>,
        folder: Option<&Path>,
        by: &Provenance,
    ) -> Result<MarkdownFolder> {
        let params = MarkdownFolderSetParams {
            space: space_path(space.as_ref())?,
            folder: folder.map(absolute).transpose()?,
            by: by.clone(),
        };
        self.request(methods::MARKDOWN_FOLDER_SET, params)
    }

    fn node_params(&self, space: &Path, id: &NodeId) -> Result<NodeParams> {
        Ok(NodeParams {
            space: space_path(space)?,
            id: *id,
        })
    }

    /// The agents of this machine, with their keys' fingerprints and their levels in the
    /// spaces they were given. People only.
    pub fn agents(&self) -> Result<Vec<AgentInfo>> {
        self.request(methods::AGENTS_LIST, json!({}))
    }

    /// Adds an agent with the fingerprint of its new key, or gives an existing agent this new
    /// key and revokes its old ones. The key itself never goes to the service. People only.
    pub fn add_agent(
        &self,
        name: &str,
        kind: &str,
        fingerprint: &str,
        by: &Provenance,
    ) -> Result<AgentInfo> {
        let params = AgentAddParams {
            name: name.to_string(),
            kind: kind.to_string(),
            fingerprint: fingerprint.to_string(),
            by: by.clone(),
        };
        self.request(methods::AGENTS_ADD, params)
    }

    /// Revokes every key of an agent and takes away its access to every space. People only.
    pub fn revoke_agent(&self, name: &str, by: &Provenance) -> Result<AgentInfo> {
        let params = AgentRevokeParams {
            agent: name.to_string(),
            by: by.clone(),
        };
        self.request(methods::AGENTS_REVOKE, params)
    }

    /// Sets what an agent may do in a space. People only.
    pub fn set_grant(
        &self,
        agent: &str,
        space: impl AsRef<Path>,
        level: GrantLevel,
        by: &Provenance,
    ) -> Result<SpaceGrant> {
        let params = GrantSetParams {
            agent: agent.to_string(),
            space: space_path(space.as_ref())?,
            level,
            by: by.clone(),
        };
        self.request(methods::GRANTS_SET, params)
    }

    /// Starts receiving changes: to one space, or to every space with `None`. Changes made
    /// after this returns are all delivered, in the order they were made.
    ///
    /// Each call returns its own [`Subscription`]; dropping it stops its delivery.
    pub fn subscribe(&self, space: Option<&Path>) -> Result<Subscription> {
        self.subscribe_with(space, false)
    }

    /// Starts receiving changes like [`subscribe`](Self::subscribe), with the Loro updates of
    /// body writes in [`Change::update`] when `updates` is set, for an editor that keeps its own
    /// copy of a document.
    pub fn subscribe_with(&self, space: Option<&Path>, updates: bool) -> Result<Subscription> {
        let space = space.map(space_path).transpose()?;
        let (events, rx) = mpsc::channel();
        let key = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        // Register before asking, so a change that arrives right after the answer is kept.
        lock(&self.shared.subscribers).push(Subscriber {
            key,
            space: space.clone(),
            events,
        });
        let result = self.call(
            methods::SUBSCRIBE,
            serde_json::to_value(SubscribeParams { space, updates })?,
        );
        if let Err(e) = result {
            lock(&self.shared.subscribers).retain(|s| s.key != key);
            return Err(e);
        }
        Ok(Subscription { events: rx })
    }

    /// Stops every subscription of this client.
    pub fn unsubscribe(&self) -> Result<()> {
        self.call(methods::UNSUBSCRIBE, json!({}))?;
        lock(&self.shared.subscribers).clear();
        Ok(())
    }

    /// Whether the connection to the service has closed.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = lock(&self.shared.stream).shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Changes delivered to a [`Client`] after [`Client::subscribe`]. It ends when the connection
/// closes.
#[derive(Debug)]
pub struct Subscription {
    events: mpsc::Receiver<Event>,
}

impl Subscription {
    /// Waits for the next event; `None` once the connection has closed.
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }

    /// Waits at most `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// The next event if one is waiting.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Iterator for Subscription {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        self.recv()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads answers and notifications until the connection closes.
fn read_messages(stream: UnixStream, shared: &Shared) {
    let reader = BufReader::new(stream);
    for line in reader.split(b'\n') {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let event = match method {
                methods::CHANGED => serde_json::from_value::<Change>(params)
                    .ok()
                    .map(Event::Changed),
                methods::SPACE => serde_json::from_value::<SpaceEvent>(params)
                    .ok()
                    .map(Event::Space),
                methods::LAGGED => serde_json::from_value::<LaggedParams>(params)
                    .ok()
                    .map(|p| Event::Lagged { missed: p.missed }),
                _ => None,
            };
            if let Some(event) = event {
                deliver(shared, event);
            }
            continue;
        }
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let reply = match message.get("error") {
            Some(error) => Err(serde_json::from_value(error.clone()).unwrap_or_else(|_| {
                RpcError::new(
                    codes::INTERNAL_ERROR,
                    "the service sent an unreadable error",
                )
            })),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        if let Some(waiting) = lock(&shared.pending).remove(&id) {
            let _ = waiting.send(reply);
        }
    }
    shared.closed.store(true, Ordering::SeqCst);
    lock(&shared.pending).clear();
    lock(&shared.subscribers).clear();
}

fn deliver(shared: &Shared, event: Event) {
    lock(&shared.subscribers).retain(|subscriber| {
        let wanted = match (&event, &subscriber.space) {
            (Event::Changed(change), Some(space)) => change.space == *space,
            (Event::Space(event), Some(space)) => event.space == *space,
            _ => true,
        };
        !wanted || subscriber.events.send(event.clone()).is_ok()
    });
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

/// An existing space path as the service names it: absolute, and canonical when it exists.
fn space_path(path: &Path) -> Result<PathBuf> {
    let path = absolute(path)?;
    Ok(fs::canonicalize(&path).unwrap_or(path))
}

/// How long to keep trying to connect after the started service has exited: it may have lost
/// a race with another service starting at the same time.
const EXITED_GRACE: Duration = Duration::from_secs(2);

/// A log of the service's messages larger than this is started again.
const LOG_LIMIT: u64 = 1024 * 1024;

/// Connects to Knode Service, starting it in the background first when it is not running.
///
/// The service is started as `<exe> service run`, in its own process group, with the
/// environment of this process (so it uses the same `KNODE_HOME`). Its messages go to
/// [`LOG_FILE`](crate::LOG_FILE) in the Knode home. Waits up to [`SPAWN_TIMEOUT`] for its
/// socket.
pub fn connect_or_spawn(exe: impl AsRef<Path>) -> Result<Client> {
    connect_or_spawn_as(exe, &Credential::Owner)
}

/// Like [`connect_or_spawn`], authenticating with `credential`, such as an agent's key. A
/// credential the service refuses fails at once with [`Error::Rpc`].
pub fn connect_or_spawn_as(exe: impl AsRef<Path>, credential: &Credential) -> Result<Client> {
    let paths = paths::paths()?;
    let socket = paths.service_socket();
    match Client::connect_with(&socket, credential) {
        Ok(client) => return Ok(client),
        Err(e @ Error::Rpc(_)) => return Err(e),
        Err(_) => {}
    }
    let mut command = Command::new(exe.as_ref());
    command.args(["service", "run"]);
    let log = open_log(&paths.home.join(crate::LOG_FILE))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    spawn_and_connect(&socket, command, SPAWN_TIMEOUT, log, credential)
}

fn open_log(path: &Path) -> std::io::Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let long = fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT);
    fs::OpenOptions::new()
        .create(true)
        .append(!long)
        .write(true)
        .truncate(long)
        .open(path)
}

/// Like [`connect_or_spawn`], with the socket, the command that starts the service and the
/// time to wait given by the caller. The service's output is discarded.
pub fn connect_or_spawn_with(
    socket: impl AsRef<Path>,
    command: Command,
    timeout: Duration,
) -> Result<Client> {
    let socket = socket.as_ref();
    if let Ok(client) = Client::connect(socket) {
        return Ok(client);
    }
    spawn_and_connect(socket, command, timeout, Stdio::null(), &Credential::Owner)
}

fn spawn_and_connect(
    socket: &Path,
    mut command: Command,
    timeout: Duration,
    stderr: Stdio,
    credential: &Credential,
) -> Result<Client> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0);
    let mut child = command.spawn()?;
    let started = Instant::now();
    let mut exited: Option<(Instant, std::process::ExitStatus)> = None;
    let mut delay = Duration::from_millis(5);
    loop {
        let connected = match Client::connect_with(socket, credential) {
            Ok(client) => Some(client),
            Err(e @ Error::Rpc(_)) => return Err(e),
            Err(_) => None,
        };
        if let Some(client) = connected {
            // Reap the service when it ends, so it never lingers as a zombie of this process.
            if exited.is_none() {
                thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            return Ok(client);
        }
        if exited.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                exited = Some((Instant::now(), status));
            }
        }
        if let Some((at, status)) = exited {
            // A service that exits early may have lost a race with another one starting at
            // the same time; give that one a moment, then give up.
            if at.elapsed() >= EXITED_GRACE {
                return Err(Error::StartFailed {
                    status: status.to_string(),
                });
            }
        }
        if started.elapsed() >= timeout {
            return Err(Error::StartTimeout {
                seconds: timeout.as_secs(),
                socket: socket.to_path_buf(),
            });
        }
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(100));
    }
}

/// Stops the Knode Service listening on `socket`, however it was started: sends it SIGTERM and
/// waits up to `timeout` until it has closed its spaces and released its lock. A service run
/// by launchd or systemd stays stopped until the next login. Returns its process id, or `None`
/// when no service was listening.
pub fn stop_service(socket: impl AsRef<Path>, timeout: Duration) -> Result<Option<u32>> {
    let socket = socket.as_ref();
    let Ok(client) = Client::connect(socket) else {
        return Ok(None);
    };
    let pid = client.status()?.pid;
    drop(client);
    let pid_t = libc::pid_t::try_from(pid)
        .map_err(|_| Error::Protocol(format!("the service sent a bad process id {pid}")))?;
    // SAFETY: kill has no memory preconditions.
    if unsafe { libc::kill(pid_t, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let lock = socket.with_file_name(crate::LOCK_FILE);
    let started = Instant::now();
    while socket.exists() || crate::lock::is_held(&lock) {
        if started.elapsed() >= timeout {
            return Err(Error::StopTimeout {
                pid,
                seconds: timeout.as_secs(),
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(Some(pid))
}

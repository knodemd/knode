//! [`Client`]: talk to Knode Service from any process.

use std::collections::HashMap;
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
    HistoryEntry, Link, NewNode, Node, NodeId, NodeSummary, NodeUpdate, Provenance, SearchHit,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::protocol::*;
use crate::{Error, Result, paths};

/// How long [`connect_or_spawn`] waits for a new service to listen.
pub const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

type Reply = std::result::Result<Value, RpcError>;

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
    /// Connects to the service listening on `socket`.
    pub fn connect(socket: impl AsRef<Path>) -> Result<Client> {
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
            by: by.clone(),
        };
        self.request(methods::NODE_UPDATE, params)
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

    /// Removes a link. Returns whether there was one.
    pub fn unlink(
        &self,
        space: impl AsRef<Path>,
        from: &NodeId,
        to: &NodeId,
        kind: &str,
    ) -> Result<bool> {
        let params = LinkDeleteParams {
            space: space_path(space.as_ref())?,
            from: *from,
            to: *to,
            kind: kind.to_string(),
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
        };
        self.request(methods::SEARCH, params)
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

    fn node_params(&self, space: &Path, id: &NodeId) -> Result<NodeParams> {
        Ok(NodeParams {
            space: space_path(space)?,
            id: *id,
        })
    }

    /// Starts receiving changes: to one space, or to every space with `None`. Changes made
    /// after this returns are all delivered, in the order they were made.
    ///
    /// Each call returns its own [`Subscription`]; dropping it stops its delivery.
    pub fn subscribe(&self, space: Option<&Path>) -> Result<Subscription> {
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
            serde_json::to_value(SubscribeParams { space })?,
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

/// Connects to Knode Service, starting it in the background first when it is not running.
///
/// The service is started as `<exe> service run`, in its own process group, with the
/// environment of this process (so it uses the same `KNODE_HOME`). Waits up to
/// [`SPAWN_TIMEOUT`] for its socket.
pub fn connect_or_spawn(exe: impl AsRef<Path>) -> Result<Client> {
    let mut command = Command::new(exe.as_ref());
    command.args(["service", "run"]);
    connect_or_spawn_with(paths::socket_path()?, command, SPAWN_TIMEOUT)
}

/// Like [`connect_or_spawn`], with the socket, the command that starts the service and the
/// time to wait given by the caller.
pub fn connect_or_spawn_with(
    socket: impl AsRef<Path>,
    mut command: Command,
    timeout: Duration,
) -> Result<Client> {
    let socket = socket.as_ref();
    if let Ok(client) = Client::connect(socket) {
        return Ok(client);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn()?;
    let started = Instant::now();
    let mut delay = Duration::from_millis(5);
    loop {
        if let Ok(client) = Client::connect(socket) {
            // Reap the service when it ends, so it never lingers as a zombie of this process.
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(client);
        }
        if started.elapsed() >= timeout {
            let _ = child.try_wait();
            return Err(Error::StartTimeout {
                seconds: timeout.as_secs(),
                socket: socket.to_path_buf(),
            });
        }
        // A child that exits early may have lost a race with another service starting at the
        // same time; keep trying to connect until the timeout.
        let _ = child.try_wait();
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(100));
    }
}

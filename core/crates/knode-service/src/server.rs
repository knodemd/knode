//! The service: one process that opens spaces and serves them on a Unix socket.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use knode_format::{NewNode, NodeId, Space, link_kinds};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;

use crate::Result;
use crate::capture::{CaptureOptions, CaptureTask};
use crate::lock::InstanceLock;
use crate::paths::{LOCK_FILE, SOCKET_FILE, knode_home};
use crate::protocol::*;

/// How many changes a subscribed connection may fall behind before it gets `lagged`.
const CHANGE_BUFFER: usize = 1024;
/// How many outgoing messages wait for a slow connection.
const OUTBOX: usize = 256;
/// How often the service checks that its socket is still there. A service whose socket was
/// removed cannot be reached, so it stops.
const SOCKET_CHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// Where the service keeps its socket and lock, and whether it captures conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The Knode home. The socket is `service.sock` and the lock `service.lock` inside it.
    pub home: PathBuf,
    /// Capture of agent conversations into the default space, or `None` for no capture.
    pub capture: Option<CaptureOptions>,
}

impl Config {
    /// A service in this Knode home, without capture.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Config {
            home: home.into(),
            capture: None,
        }
    }

    /// A service in the default Knode home (see [`knode_home`](crate::knode_home)) that
    /// captures conversations as `config.toml` says. This is `knode service run`.
    pub fn from_env() -> Result<Self> {
        Ok(Config::new(knode_home()?).with_capture(CaptureOptions::default()))
    }

    /// The same service, capturing conversations with these options.
    pub fn with_capture(mut self, options: CaptureOptions) -> Self {
        self.capture = Some(options);
        self
    }

    /// The socket the service listens on.
    pub fn socket_path(&self) -> PathBuf {
        self.home.join(SOCKET_FILE)
    }

    /// The lock file that keeps a second service from starting.
    pub fn lock_path(&self) -> PathBuf {
        self.home.join(LOCK_FILE)
    }
}

/// Runs Knode Service in this thread until it receives SIGTERM or SIGINT. This is
/// `knode service run`.
///
/// Fails at once with [`Error::AlreadyRunning`](crate::Error::AlreadyRunning) when another
/// service runs in the same Knode home. On the way out it removes the socket and releases the
/// lock.
pub fn run(config: Config) -> Result<()> {
    let bound = Bound::bind(&config)?;
    let runtime = runtime()?;
    eprintln!(
        "Knode Service {} listening on {}",
        crate::version(),
        bound.socket.display()
    );
    runtime.block_on(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let shutdown = async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        };
        serve(bound, shutdown).await
    })?;
    eprintln!("Knode Service stopped");
    Ok(())
}

/// Starts Knode Service on a background thread and returns once it is listening. Used by
/// tests and by apps that embed the service. The service stops when the handle is stopped or
/// dropped.
pub fn start(config: Config) -> Result<ServiceHandle> {
    let bound = Bound::bind(&config)?;
    let socket = bound.socket.clone();
    let runtime = runtime()?;
    let (stop, stopped) = oneshot::channel::<()>();
    let thread = thread::Builder::new()
        .name("knode-service".into())
        .spawn(move || {
            runtime.block_on(serve(bound, async move {
                let _ = stopped.await;
            }))
        })?;
    Ok(ServiceHandle {
        socket,
        stop: Some(stop),
        thread: Some(thread),
    })
}

/// A service started with [`start`].
#[derive(Debug)]
pub struct ServiceHandle {
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}

impl ServiceHandle {
    /// The socket the service listens on.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Stops the service and waits until it has closed every space and released the lock.
    pub fn stop(mut self) -> Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| io::Error::other("the service thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("knode-service")
        .enable_all()
        .build()
}

/// The lock and the listening socket, taken before the runtime starts so that errors come back
/// to the caller at once.
struct Bound {
    lock: InstanceLock,
    listener: StdUnixListener,
    socket: PathBuf,
    home: PathBuf,
    capture: Option<CaptureOptions>,
}

impl Bound {
    fn bind(config: &Config) -> Result<Self> {
        if !config.home.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&config.home)?;
        }
        let lock = InstanceLock::acquire(&config.lock_path())?;
        // Holding the lock means no other service is running: a socket left here is stale.
        let socket = config.socket_path();
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = StdUnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Bound {
            lock,
            listener,
            socket,
            home: config.home.clone(),
            capture: config.capture.clone(),
        })
    }
}

/// Accepts connections until `shutdown` completes, then closes them, removes the socket and
/// releases the lock.
async fn serve(bound: Bound, shutdown: impl Future<Output = ()>) -> io::Result<()> {
    let Bound {
        lock,
        listener,
        socket,
        home,
        capture,
    } = bound;
    let listener = UnixListener::from_std(listener)?;
    let state = Arc::new(State::new());
    let capture = match capture {
        Some(options) => Some(CaptureTask::start(state.clone(), home, options)?),
        None => None,
    };
    let mut connections = JoinSet::new();
    let mut check = tokio::time::interval(SOCKET_CHECK);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = check.tick() => {
                if !socket.exists() {
                    eprintln!("Knode Service: {} was removed; stopping", socket.display());
                    break;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(stream, state.clone()));
                }
                Err(e) => {
                    eprintln!("Knode Service: accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    let _ = fs::remove_file(&socket);
    connections.shutdown().await;
    // Capture holds a space open: stop it before the spaces close.
    if let Some(capture) = capture {
        tokio::task::spawn_blocking(move || capture.stop()).await?;
    }
    // Writes already running finish in their blocking threads; wait for them before closing
    // the spaces and releasing the lock.
    tokio::task::spawn_blocking(move || state.close()).await?;
    drop(lock);
    Ok(())
}

/// Each open space, by canonical path.
type OpenSpaces = HashMap<PathBuf, Arc<Mutex<Space>>>;

/// What every connection shares: the open spaces and the change feed.
pub(crate) struct State {
    /// Each open space, by canonical path. A space is only used with its mutex held, so writes
    /// to one space happen one at a time, in one place.
    spaces: Mutex<OpenSpaces>,
    changes: broadcast::Sender<Change>,
    clients: AtomicU64,
    /// Set when the service is stopping: no space opens after that.
    closing: AtomicBool,
    /// The space conversations are captured into, while capture runs.
    capturing: Mutex<Option<PathBuf>>,
}

impl State {
    fn new() -> Self {
        State {
            spaces: Mutex::new(HashMap::new()),
            changes: broadcast::channel(CHANGE_BUFFER).0,
            clients: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            capturing: Mutex::new(None),
        }
    }

    /// The open space at `path`, opening it the first time.
    pub(crate) fn space(&self, path: &Path) -> Result<Arc<Mutex<Space>>, RpcError> {
        require_absolute(path)?;
        let key = fs::canonicalize(path)
            .map_err(|_| RpcError::from(knode_format::Error::NotASpace(path.to_path_buf())))?;
        let mut spaces = self.open_spaces()?;
        if let Some(space) = spaces.get(&key) {
            return Ok(space.clone());
        }
        let space = Arc::new(Mutex::new(Space::open(&key)?));
        spaces.insert(key, space.clone());
        Ok(space)
    }

    /// The map of open spaces, locked, unless the service is stopping.
    fn open_spaces(&self) -> Result<MutexGuard<'_, OpenSpaces>, RpcError> {
        let spaces = lock_ignoring_poison(&self.spaces);
        if self.closing.load(Ordering::SeqCst) {
            return Err(RpcError::new(
                codes::INTERNAL_ERROR,
                "Knode Service is stopping",
            ));
        }
        Ok(spaces)
    }

    /// Stops opening spaces and closes the open ones, waiting for writes in progress.
    fn close(&self) {
        let spaces = {
            let mut spaces = lock_ignoring_poison(&self.spaces);
            self.closing.store(true, Ordering::SeqCst);
            std::mem::take(&mut *spaces)
        };
        for space in spaces.into_values() {
            drop(lock_ignoring_poison(&space));
        }
    }

    fn create_space(&self, path: &Path, name: &str) -> Result<SpaceInfo, RpcError> {
        require_absolute(path)?;
        let mut spaces = self.open_spaces()?;
        drop(Space::create(path, name)?);
        let key = fs::canonicalize(path).map_err(knode_format::Error::from)?;
        let space = Space::open(&key)?;
        let info = SpaceInfo::of(&space)?;
        spaces.insert(key, Arc::new(Mutex::new(space)));
        Ok(info)
    }

    fn notify(&self, space: &Space, node_id: NodeId, op: ChangeKind) {
        self.notify_path(space.path(), node_id, op);
    }

    /// Announces a change to the space opened at `space`.
    pub(crate) fn notify_path(&self, space: &Path, node_id: NodeId, op: ChangeKind) {
        // No receivers is not an error: nobody is subscribed.
        let _ = self.changes.send(Change {
            space: space.to_path_buf(),
            node_id,
            op,
        });
    }

    /// Records the space capture writes to, or that capture stopped.
    pub(crate) fn set_capturing(&self, space: Option<PathBuf>) {
        *lock_ignoring_poison(&self.capturing) = space;
    }

    fn status(&self) -> Status {
        let mut spaces: Vec<PathBuf> = lock_ignoring_poison(&self.spaces).keys().cloned().collect();
        spaces.sort();
        Status {
            version: crate::version().to_string(),
            pid: std::process::id(),
            spaces,
            clients: self.clients.load(Ordering::Relaxed),
            capture: lock_ignoring_poison(&self.capturing).clone(),
        }
    }
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn require_absolute(path: &Path) -> Result<(), RpcError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "space must be an absolute path: {}",
            path.display()
        )))
    }
}

fn params<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() {
        Value::Object(Map::new())
    } else {
        params
    };
    serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("invalid params for {method}: {e}")))
}

fn to_value(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::new(codes::INTERNAL_ERROR, e.to_string()))
}

/// Runs one method on the spaces. Blocking: called on a blocking thread.
fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    match method {
        methods::SPACE_CREATE => {
            let p: SpaceCreateParams = params(method, raw)?;
            to_value(state.create_space(&p.space, &p.name)?)
        }
        methods::SPACE_OPEN => {
            let p: SpaceParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(SpaceInfo::of(&space)?)
        }
        methods::NODE_CREATE => {
            let p: NodeCreateParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let new = NewNode {
                kind: p.kind,
                title: p.title,
                body: p.body,
                tags: p.tags,
            };
            let node = space.create_node(new, &p.by)?;
            state.notify(&space, node.id, ChangeKind::Create);
            to_value(node)
        }
        methods::NODE_GET => {
            let p: NodeParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(space.get_node(&p.id)?)
        }
        methods::NODE_FIND_BY_TITLE => {
            let p: TitleParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(space.find_by_title(&p.title)?)
        }
        methods::NODE_UPDATE => {
            let p: NodeUpdateParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            // A new title rewrites `[[links]]` in the nodes that mention this one.
            let mut rewritten = Vec::new();
            if p.update.title.is_some() {
                for link in space.backlinks(&p.id)? {
                    if link.kind == link_kinds::MENTIONS
                        && link.from != p.id
                        && !rewritten.contains(&link.from)
                    {
                        rewritten.push(link.from);
                    }
                }
            }
            let before = space.get_node(&p.id)?;
            let node = space.update_node(&p.id, p.update, &p.by)?;
            if before.as_ref() != Some(&node) {
                state.notify(&space, node.id, ChangeKind::Update);
                for id in rewritten {
                    state.notify(&space, id, ChangeKind::Update);
                }
            }
            to_value(node)
        }
        methods::NODE_DELETE => {
            let p: NodeDeleteParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            space.delete_node(&p.id, &p.by)?;
            state.notify(&space, p.id, ChangeKind::Delete);
            Ok(Value::Null)
        }
        methods::LINK_CREATE => {
            let p: LinkCreateParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let link = space.link(&p.from, &p.to, &p.kind, &p.by)?;
            state.notify(&space, p.from, ChangeKind::Link);
            to_value(link)
        }
        methods::LINK_DELETE => {
            let p: LinkDeleteParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let mut space = lock_ignoring_poison(&space);
            let removed = space.unlink(&p.from, &p.to, &p.kind)?;
            if removed {
                state.notify(&space, p.from, ChangeKind::Unlink);
            }
            to_value(removed)
        }
        methods::LINKS_FROM
        | methods::LINKS_BACK
        | methods::LINKS_UNRESOLVED
        | methods::HISTORY => {
            let p: NodeParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            match method {
                methods::LINKS_FROM => to_value(space.links_from(&p.id)?),
                methods::LINKS_BACK => to_value(space.backlinks(&p.id)?),
                methods::LINKS_UNRESOLVED => to_value(space.unresolved_links(&p.id)?),
                _ => to_value(space.history(&p.id)?),
            }
        }
        methods::SEARCH => {
            let p: SearchParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(space.search(&p.query, p.limit.min(MAX_LIMIT))?)
        }
        methods::RECENT => {
            let p: RecentParams = params(method, raw)?;
            let space = state.space(&p.space)?;
            let space = lock_ignoring_poison(&space);
            to_value(space.recent(p.limit.min(MAX_LIMIT))?)
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

/// Which changes a connection receives.
#[derive(Debug, Default)]
enum Filter {
    #[default]
    Nothing,
    All,
    Spaces(HashSet<PathBuf>),
}

impl Filter {
    fn wants(&self, change: &Change) -> bool {
        match self {
            Filter::Nothing => false,
            Filter::All => true,
            Filter::Spaces(spaces) => spaces.contains(&change.space),
        }
    }
}

/// One client connection. Requests on a connection run one after another, in order; many
/// connections run at once.
async fn connection(stream: UnixStream, state: Arc<State>) {
    state.clients.fetch_add(1, Ordering::Relaxed);
    let (read, mut write) = stream.into_split();
    let (outbox, mut outgoing) = mpsc::channel::<String>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(mut line) = outgoing.recv().await {
            line.push('\n');
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let filter = Arc::new(Mutex::new(Filter::Nothing));
    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line(&mut reader, &mut line).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                let error = RpcError::new(codes::INVALID_REQUEST, e.to_string());
                let _ = outbox.send(response(Value::Null, Err(error))).await;
                break;
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request = match parse_request(&line) {
            Ok(request) => request,
            Err(error) => {
                if outbox
                    .send(response(Value::Null, Err(error)))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };
        let result = match request.method.as_str() {
            methods::SUBSCRIBE => {
                subscribe(&state, &filter, &mut forwarder, &outbox, request.params)
            }
            methods::UNSUBSCRIBE => {
                *lock_ignoring_poison(&filter) = Filter::Nothing;
                Ok(Value::Bool(true))
            }
            methods::SERVICE_STATUS => to_value(state.status()),
            _ => {
                let state = state.clone();
                let method = request.method.clone();
                tokio::task::spawn_blocking(move || call(&state, &method, request.params))
                    .await
                    .unwrap_or_else(|e| {
                        Err(RpcError::new(
                            codes::INTERNAL_ERROR,
                            format!("{} failed inside the service: {e}", request.method),
                        ))
                    })
            }
        };
        if let Some(id) = request.id {
            if outbox.send(response(id, result)).await.is_err() {
                break;
            }
        }
    }
    if let Some(forwarder) = forwarder {
        forwarder.abort();
    }
    drop(outbox);
    let _ = writer.await;
    state.clients.fetch_sub(1, Ordering::Relaxed);
}

fn subscribe(
    state: &State,
    filter: &Arc<Mutex<Filter>>,
    forwarder: &mut Option<tokio::task::JoinHandle<()>>,
    outbox: &mpsc::Sender<String>,
    raw: Value,
) -> Result<Value, RpcError> {
    let p: SubscribeParams = params(methods::SUBSCRIBE, raw)?;
    {
        let mut filter = lock_ignoring_poison(filter);
        match p.space {
            None => *filter = Filter::All,
            Some(path) => {
                require_absolute(&path)?;
                let key = fs::canonicalize(&path)
                    .map_err(|_| RpcError::from(knode_format::Error::NotASpace(path)))?;
                match &mut *filter {
                    Filter::All => {}
                    Filter::Spaces(spaces) => {
                        spaces.insert(key);
                    }
                    Filter::Nothing => *filter = Filter::Spaces(HashSet::from([key])),
                }
            }
        }
    }
    if forwarder.is_none() {
        // Subscribe to the feed before answering, so no change made after the answer is missed.
        let mut changes = state.changes.subscribe();
        let filter = filter.clone();
        let outbox = outbox.clone();
        *forwarder = Some(tokio::spawn(async move {
            loop {
                let message = match changes.recv().await {
                    Ok(change) => {
                        if !lock_ignoring_poison(&filter).wants(&change) {
                            continue;
                        }
                        notification(methods::CHANGED, &change)
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        notification(methods::LAGGED, &LaggedParams { missed })
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if outbox.send(message).await.is_err() {
                    break;
                }
            }
        }));
    }
    Ok(Value::Bool(true))
}

struct Request {
    /// `None` for a notification, which gets no answer.
    id: Option<Value>,
    method: String,
    params: Value,
}

fn parse_request(line: &[u8]) -> Result<Request, RpcError> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| RpcError::new(codes::PARSE_ERROR, format!("not valid JSON: {e}")))?;
    let Value::Object(mut object) = value else {
        let message = if value.is_array() {
            "batches are not supported: send one request per line"
        } else {
            "a request must be a JSON object"
        };
        return Err(RpcError::new(codes::INVALID_REQUEST, message));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::new(
            codes::INVALID_REQUEST,
            "\"jsonrpc\" must be \"2.0\"",
        ));
    }
    let id = match object.remove("id") {
        None => None,
        Some(id @ (Value::Null | Value::Number(_) | Value::String(_))) => Some(id),
        Some(_) => {
            return Err(RpcError::new(
                codes::INVALID_REQUEST,
                "\"id\" must be a string, a number or null",
            ));
        }
    };
    let method = match object.remove("method") {
        Some(Value::String(method)) => method,
        _ => {
            return Err(RpcError::new(
                codes::INVALID_REQUEST,
                "\"method\" must be a string",
            ));
        }
    };
    let params = match object.remove("params") {
        None | Some(Value::Null) => Value::Null,
        Some(params @ Value::Object(_)) => params,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "params must be an object with named fields",
            ));
        }
    };
    Ok(Request { id, method, params })
}

fn response(id: Value, result: Result<Value, RpcError>) -> String {
    let message = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    };
    message.to_string()
}

fn notification(method: &str, params: &impl Serialize) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

/// Reads one line, without its newline, into `line`. Returns `false` at the end of the stream.
/// Fails when a line is longer than [`MAX_LINE`].
async fn read_line(reader: &mut BufReader<OwnedReadHalf>, line: &mut Vec<u8>) -> io::Result<bool> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let (taken, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&available[..i]);
                (i + 1, true)
            }
            None => {
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        reader.consume(taken);
        if line.len() > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a message is longer than {MAX_LINE} bytes"),
            ));
        }
        if done {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_checked() {
        let ok =
            parse_request(br#"{"jsonrpc":"2.0","id":1,"method":"recent","params":{}}"#).unwrap();
        assert_eq!(ok.id, Some(json!(1)));
        let note = parse_request(br#"{"jsonrpc":"2.0","method":"recent"}"#).unwrap();
        assert!(note.id.is_none());
        let null_id = parse_request(br#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).unwrap();
        assert_eq!(null_id.id, Some(Value::Null));

        let code = |line: &[u8]| parse_request(line).err().unwrap().code;
        assert_eq!(code(b"{nope"), codes::PARSE_ERROR);
        assert_eq!(code(b"[]"), codes::INVALID_REQUEST);
        assert_eq!(code(br#"{"id":1,"method":"x"}"#), codes::INVALID_REQUEST);
        assert_eq!(
            code(br#"{"jsonrpc":"2.0","id":{},"method":"x"}"#),
            codes::INVALID_REQUEST
        );
        assert_eq!(code(br#"{"jsonrpc":"2.0","id":1}"#), codes::INVALID_REQUEST);
        assert_eq!(
            code(br#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1]}"#),
            codes::INVALID_PARAMS
        );
    }
}

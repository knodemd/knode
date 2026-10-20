//! Capture inside the service: every agent conversation becomes a node of the space it belongs
//! to, kept current while the agent writes it.
//!
//! Where a conversation goes follows section 15.5 of the .knode specification: the `project`
//! folders of every space the service knows (those in `config.toml`, `default_space` and
//! `spaces`, and those it has open) are matched against the folder the conversation was held
//! in, the most specific one wins, and capture turned off for a project, or for a space, means
//! nothing of it is stored. Conversations of no project go to the space of the `agent` folder
//! they were read from, or else to the default space.
//!
//! The machine-wide settings come from `config.toml` in the Knode home: `default_space`,
//! `spaces`, `capture` (on unless `false`) and `capture_exclude`, which leaves projects out
//! whatever the spaces say. The file and the folders of the spaces are read again from time to
//! time, and at once when a client changes folders or settings, so changes take effect without
//! restarting the service.
//!
//! Capture writes through the same per-space mutex as the clients, so its writes never
//! interleave with theirs, and every node it creates or changes is announced with `changed`.
//! Locks are always taken in the order capture, then space.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use knode_format::{ChangeOp, Space, folder_kinds};
use knode_ingest::{
    Agent, Capture, CaptureError, CaptureReport, Conversation, Destinations, ProjectFilter,
    WatchOptions, Watcher, discover,
};

use crate::markdown::{record_path, resolve_path};
use crate::protocol::ChangeKind;
use crate::server::State;
use crate::spaces::{capture_enabled, excluded, rule_patterns};

/// The folder in the Knode home where capture keeps its state.
pub const CAPTURE_DIR: &str = "capture";
/// The capture state file in [`CAPTURE_DIR`].
pub const CAPTURE_STATE_FILE: &str = "state.json";

/// How many session files are read under one lock while catching up, so that clients wait at
/// most for a few files.
const CATCH_UP_CHUNK: usize = 8;

/// How the service captures conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// The session folders to follow, by agent. `None` uses each agent's own folder:
    /// `$CLAUDE_CONFIG_DIR/projects` (by default `~/.claude/projects`) and `$CODEX_HOME/sessions`
    /// (by default `~/.codex/sessions`).
    pub roots: Option<Vec<(Agent, PathBuf)>>,
    /// How changes in the session folders are batched.
    pub watch: WatchOptions,
    /// How often `config.toml` is read again.
    pub settings_poll: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        CaptureOptions {
            roots: None,
            watch: WatchOptions::default(),
            settings_poll: Duration::from_secs(5),
        }
    }
}

/// What capture should do, from `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
    /// `capture` in `config.toml`: off stops capture everywhere.
    enabled: bool,
    /// The default space, where conversations of no folder go.
    default: Option<PathBuf>,
    /// Projects whose conversations are never captured, whatever the spaces say.
    exclude: Vec<PathBuf>,
    /// Every space `config.toml` names, the default one first.
    spaces: Vec<PathBuf>,
}

fn read_settings(home: &Path) -> Result<Settings, String> {
    let config = knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
        .map_err(|e| e.to_string())?;
    Ok(Settings {
        enabled: config.capture_enabled(),
        default: config.default_space.clone().filter(|p| p.is_absolute()),
        exclude: config.capture_exclude.clone(),
        spaces: config
            .known_spaces()
            .into_iter()
            .filter(|p| p.is_absolute())
            .collect(),
    })
}

/// One space capture may write to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// The space as the service opened it.
    path: PathBuf,
    /// Its `capture.enabled` setting.
    capture: bool,
}

/// A `project` folder of a space.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Project {
    /// The id of its space.
    space: String,
    /// The folder as the space records it.
    path: String,
    capture: bool,
    exclude: Vec<String>,
}

/// An `agent` folder of a space, read by capture besides the agents' own folders.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentFolder {
    /// The id of its space.
    space: String,
    agent: Agent,
    /// The folder on this machine.
    path: PathBuf,
}

/// Where conversations go: the folders and settings of every space the service knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Routes {
    /// The id of the default space, when it can be opened.
    default: Option<String>,
    /// Every space that can be opened, by id.
    spaces: BTreeMap<String, Target>,
    projects: Vec<Project>,
    agents: Vec<AgentFolder>,
}

impl Routes {
    /// Reads the folders and settings of every space in `config.toml` and every space the
    /// service has open. Spaces that cannot be opened are left out.
    fn build(state: &State, settings: &Settings) -> Routes {
        let mut routes = Routes::default();
        let default = settings
            .default
            .as_deref()
            .and_then(|p| std::fs::canonicalize(p).ok());
        let mut paths = settings.spaces.clone();
        paths.extend(state.open_paths());
        for path in paths {
            if !path.exists() {
                continue;
            }
            let Ok(space) = state.space(&path) else {
                continue;
            };
            let space = lock(&space);
            let id = space.manifest().id.clone();
            if routes.spaces.contains_key(&id) {
                continue;
            }
            if let Err(e) = routes.add(&space, &id) {
                eprintln!(
                    "Knode Service: cannot read the folders of {}: {e}",
                    space.path().display()
                );
                continue;
            }
            if default.as_deref() == Some(space.path()) {
                routes.default = Some(id);
            }
        }
        routes
            .projects
            .sort_by(|a, b| (&a.path, &a.space).cmp(&(&b.path, &b.space)));
        routes
            .agents
            .sort_by(|a, b| (&a.path, &a.space).cmp(&(&b.path, &b.space)));
        routes
    }

    /// Reads the folders and settings of one space again, after a client changed them, so
    /// capture follows the change before the client hears back.
    pub(crate) fn replace(&mut self, space: &Space) -> knode_format::Result<()> {
        let id = space.manifest().id.clone();
        self.projects.retain(|p| p.space != id);
        self.agents.retain(|a| a.space != id);
        self.spaces.remove(&id);
        self.add(space, &id)?;
        self.projects
            .sort_by(|a, b| (&a.path, &a.space).cmp(&(&b.path, &b.space)));
        self.agents
            .sort_by(|a, b| (&a.path, &a.space).cmp(&(&b.path, &b.space)));
        Ok(())
    }

    fn add(&mut self, space: &Space, id: &str) -> knode_format::Result<()> {
        let capture = capture_enabled(space)?;
        let folders = space.folders()?;
        self.spaces.insert(
            id.to_string(),
            Target {
                path: space.path().to_path_buf(),
                capture,
            },
        );
        for folder in folders {
            match folder.kind.as_str() {
                folder_kinds::PROJECT => self.projects.push(Project {
                    space: id.to_string(),
                    exclude: rule_patterns(&folder.rules, "exclude")
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    path: folder.path,
                    capture: folder.capture,
                }),
                folder_kinds::AGENT if folder.capture => {
                    let agent = folder
                        .rules
                        .get("agent")
                        .and_then(serde_json::Value::as_str)
                        .and_then(Agent::from_id);
                    if let (Some(agent), Some(path)) = (agent, resolve_path(&folder.path)) {
                        self.agents.push(AgentFolder {
                            space: id.to_string(),
                            agent,
                            path,
                        });
                    }
                }
                // Notes folders are not captured, and kinds this version does not know are
                // kept and left alone.
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether any conversation can go anywhere.
    fn active(&self) -> bool {
        self.default.is_some() || !self.projects.is_empty() || !self.agents.is_empty()
    }

    /// The session folders of the agent folders, read besides the agents' own.
    fn roots(&self) -> Vec<(Agent, PathBuf)> {
        let mut roots: Vec<(Agent, PathBuf)> = Vec::new();
        for folder in &self.agents {
            if !roots.iter().any(|(_, root)| *root == folder.path) {
                roots.push((folder.agent, folder.path.clone()));
            }
        }
        roots
    }

    /// The default space as the service opened it.
    fn default_path(&self) -> Option<PathBuf> {
        let id = self.default.as_ref()?;
        Some(self.spaces.get(id)?.path.clone())
    }

    fn path_of(&self, id: &str) -> Option<&Path> {
        self.spaces.get(id).map(|t| t.path.as_path())
    }

    /// The ids of the spaces a conversation held in `project` and read from the session file
    /// `file` goes to (section 15.5). Empty when it is stored nowhere.
    fn route(&self, project: Option<&Path>, file: &Path) -> Vec<String> {
        let enabled = |id: &str| self.spaces.get(id).is_some_and(|t| t.capture);
        if let Some(project) = project.filter(|p| p.is_absolute()) {
            let recorded = record_path(project);
            let matching: Vec<&Project> = self
                .projects
                .iter()
                .filter(|p| inside(&p.path, &recorded))
                .collect();
            if let Some(longest) = matching.iter().map(|p| p.path.len()).max() {
                let mut spaces: Vec<String> = Vec::new();
                for folder in matching.iter().filter(|p| p.path.len() == longest) {
                    let relative = recorded[folder.path.len()..].trim_start_matches('/');
                    let rules: Vec<&str> = folder.exclude.iter().map(String::as_str).collect();
                    if folder.capture
                        && !excluded(relative, &rules)
                        && enabled(&folder.space)
                        && !spaces.contains(&folder.space)
                    {
                        spaces.push(folder.space.clone());
                    }
                }
                // Capture off for the project means nothing of it is kept anywhere.
                return spaces;
            }
        }
        let from_agent_folder = self
            .agents
            .iter()
            .filter(|folder| file.starts_with(&folder.path))
            .max_by_key(|folder| folder.path.as_os_str().len());
        let space = match from_agent_folder {
            Some(folder) => Some(&folder.space),
            None => self.default.as_ref(),
        };
        match space {
            Some(id) if enabled(id) => vec![id.clone()],
            _ => Vec::new(),
        }
    }
}

/// Whether the recorded path `path` is the folder `folder` or inside it.
fn inside(folder: &str, path: &str) -> bool {
    if folder == "/" {
        return path.starts_with('/');
    }
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The spaces capture writes to, for [`Capture::sync_all_to`]: the routes the service keeps,
/// and the spaces as it has them open.
struct Router {
    state: Arc<State>,
}

impl Router {
    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.state.routes()
    }
}

impl Destinations for Router {
    fn routing(&self) -> String {
        format!("{:?}", *self.routes())
    }

    fn route(&self, conversation: &Conversation, file: &Path) -> Vec<String> {
        self.routes().route(conversation.project.as_deref(), file)
    }

    fn with_space(
        &self,
        key: &str,
        write: &mut dyn FnMut(&mut Space) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        let path = self.routes().path_of(key).map(Path::to_path_buf);
        let Some(path) = path else {
            return Err(CaptureError::Space(knode_format::Error::Invalid(format!(
                "capture has no space with the id {key}"
            ))));
        };
        let space = self
            .state
            .space(&path)
            .map_err(|e| CaptureError::Space(knode_format::Error::Invalid(e.message)))?;
        let mut space = lock(&space);
        write(&mut space)
    }
}

/// What capture runs with: it starts again when this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    exclude: Vec<PathBuf>,
    /// The agent folders of the spaces.
    roots: Vec<(Agent, PathBuf)>,
}

/// Capture while it runs.
struct Running {
    capture: Arc<Mutex<Capture>>,
    watcher: Watcher,
}

/// The capture thread of a running service.
pub(crate) struct CaptureTask {
    stopping: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureTask {
    /// Starts following `config.toml` in `home` and the folders of the spaces, and capturing as
    /// they say.
    pub(crate) fn start(
        state: Arc<State>,
        home: PathBuf,
        options: CaptureOptions,
    ) -> std::io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let (wake, woken) = mpsc::channel();
        state.set_capture_wake(Some(wake.clone()));
        let thread = {
            let stopping = stopping.clone();
            thread::Builder::new()
                .name("knode-capture".into())
                .spawn(move || supervise(&state, &home, &options, &stopping, &woken))?
        };
        Ok(CaptureTask {
            stopping,
            wake,
            thread: Some(thread),
        })
    }

    /// Stops capturing and waits for a sync in progress to finish.
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads the settings and the folders of the spaces, starts and stops capture as they change,
/// until told to stop.
fn supervise(
    state: &Arc<State>,
    home: &Path,
    options: &CaptureOptions,
    stopping: &AtomicBool,
    woken: &mpsc::Receiver<()>,
) {
    let router = Arc::new(Router {
        state: state.clone(),
    });
    // What capture runs with, the routes it last read every file with, and capture while it
    // runs.
    let mut plan: Option<Plan> = None;
    let mut synced: Option<Routes> = None;
    let mut running: Option<Running> = None;
    let mut last_error: Option<String> = None;
    loop {
        match read_settings(home) {
            Ok(settings) => {
                let generation = state.routes_generation();
                let built = if settings.enabled {
                    Routes::build(state, &settings)
                } else {
                    Routes::default()
                };
                if !state.set_routes(built, generation) {
                    // A client changed folders while they were read: read them again.
                    continue;
                }
                let routes = router.routes().clone();
                let active = settings.enabled && routes.active();
                let wanted = active.then(|| Plan {
                    exclude: settings.exclude.clone(),
                    roots: routes.roots(),
                });
                let default = routes.default_path();
                let rerouted = synced.as_ref() != Some(&routes);
                // A capture that could not start is tried again at each poll.
                let retry = wanted.is_some() && running.is_none();
                if wanted != plan || retry {
                    if let Some(old) = running.take() {
                        old.watcher.stop();
                        state.set_capturing(None);
                        eprintln!("Knode Service: capture stopped");
                    }
                    plan = wanted.clone();
                    if let Some(wanted) = &wanted {
                        state.set_capturing(default.clone());
                        match start(state, home, options, &router, wanted, stopping) {
                            Ok(started) => {
                                running = started;
                                synced = Some(routes.clone());
                                last_error = None;
                            }
                            Err(error) => {
                                if last_error.as_ref() != Some(&error) {
                                    eprintln!("Knode Service: capture cannot start: {error}");
                                }
                                last_error = Some(error);
                            }
                        }
                    }
                } else if rerouted {
                    if let Some(running) = &running {
                        // Folders or settings changed: what was left out may go somewhere now.
                        let result = lock(&running.capture).sync_all_to(router.as_ref());
                        report(state, &router, result);
                        synced = Some(routes.clone());
                    }
                }
                state.set_capturing(if running.is_some() { default } else { None });
            }
            Err(error) => {
                // Keep capturing with the settings already read.
                if last_error.as_ref() != Some(&error) {
                    eprintln!("Knode Service: {error}");
                }
                last_error = Some(error);
            }
        }
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        match woken.recv_timeout(options.settings_poll) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Several changes at once are read together.
        while woken.try_recv().is_ok() {}
        if stopping.load(Ordering::SeqCst) {
            break;
        }
    }
    state.set_capture_wake(None);
    if let Some(running) = running {
        running.watcher.stop();
    }
    state.set_capturing(None);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Catches up with every session file and starts the watcher. Returns `None` when the service
/// started stopping meanwhile.
fn start(
    state: &Arc<State>,
    home: &Path,
    options: &CaptureOptions,
    router: &Arc<Router>,
    plan: &Plan,
    stopping: &AtomicBool,
) -> Result<Option<Running>, String> {
    let state_file = home.join(CAPTURE_DIR).join(CAPTURE_STATE_FILE);
    let mut capture = Capture::open(state_file)
        .map_err(|e| e.to_string())?
        .with_filter(ProjectFilter::excluding(plan.exclude.iter().cloned()));
    let mut roots = match &options.roots {
        Some(roots) => roots.clone(),
        None => capture.roots().to_vec(),
    };
    for (agent, root) in &plan.roots {
        if !roots.iter().any(|(_, known)| known == root) {
            roots.push((*agent, root.clone()));
        }
    }
    capture = capture.with_roots(roots.iter().cloned());
    let capture = Arc::new(Mutex::new(capture));
    match router.routes().default_path() {
        Some(default) => eprintln!(
            "Knode Service: capturing conversations, by default into {}",
            default.display()
        ),
        None => eprintln!("Knode Service: capturing the conversations of the spaces' folders"),
    }

    // Watch first, so nothing written while catching up is missed: the watcher's syncs wait
    // for the capture lock.
    let watcher = {
        let state = state.clone();
        let reporter = router.clone();
        Watcher::capture_to(
            capture.clone(),
            router.clone(),
            options.watch,
            move |result| {
                report(&state, &reporter, result);
            },
        )
        .map_err(|e| e.to_string())?
    };

    // Catch up a few files at a time, newest first, so clients never wait long and the latest
    // conversations show up first.
    let mut files = Vec::new();
    for (agent, root) in &roots {
        files.extend(discover(*agent, root).map_err(|e| e.to_string())?);
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.modified));
    let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
    for chunk in paths.chunks(CATCH_UP_CHUNK) {
        if stopping.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let result = lock(&capture).sync_paths_to(router.as_ref(), chunk);
        report(state, router, result);
    }
    // Then once more over everything, which also forgets files that are gone.
    if stopping.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let result = lock(&capture).sync_all_to(router.as_ref());
    report(state, router, result);
    Ok(Some(Running { capture, watcher }))
}

/// Announces what a sync wrote, and logs what went wrong.
fn report(state: &State, router: &Router, result: Result<CaptureReport, CaptureError>) {
    match result {
        Ok(report) => {
            for change in &report.changes {
                let op = match change.op {
                    ChangeOp::Create => ChangeKind::Create,
                    ChangeOp::Update => ChangeKind::Update,
                    ChangeOp::Delete => ChangeKind::Delete,
                };
                let path = router
                    .routes()
                    .path_of(&change.space)
                    .map(Path::to_path_buf);
                if let Some(path) = path {
                    state.notify_path(&path, change.node_id, op);
                }
            }
            for (path, error) in &report.failed {
                eprintln!(
                    "Knode Service: capture could not read {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => eprintln!("Knode Service: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_follow_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let off = Settings {
            enabled: true,
            default: None,
            exclude: Vec::new(),
            spaces: Vec::new(),
        };
        assert_eq!(read_settings(home).unwrap(), off);

        let config = home.join("config.toml");
        std::fs::write(
            &config,
            "default_space = \"/s/Work.knode\"\nspaces = [\"/s/Home.knode\", \"rel.knode\"]\n\
             capture_exclude = [\"/p\"]\n",
        )
        .unwrap();
        let on = read_settings(home).unwrap();
        assert_eq!(on.default, Some(PathBuf::from("/s/Work.knode")));
        assert_eq!(
            on.spaces,
            vec![
                PathBuf::from("/s/Work.knode"),
                PathBuf::from("/s/Home.knode")
            ]
        );
        assert_eq!(on.exclude, vec![PathBuf::from("/p")]);

        std::fs::write(
            &config,
            "default_space = \"/s/Work.knode\"\ncapture = false\n",
        )
        .unwrap();
        assert!(!read_settings(home).unwrap().enabled);

        std::fs::write(&config, "default_space = \"relative.knode\"\n").unwrap();
        assert_eq!(read_settings(home).unwrap().default, None);

        std::fs::write(&config, "default_space = 3\n").unwrap();
        assert!(read_settings(home).is_err());
    }

    fn target(path: &str, capture: bool) -> Target {
        Target {
            path: PathBuf::from(path),
            capture,
        }
    }

    fn project(space: &str, path: &str, capture: bool, exclude: &[&str]) -> Project {
        Project {
            space: space.into(),
            path: path.into(),
            capture,
            exclude: exclude.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn routes_follow_the_specification() {
        let routes = Routes {
            default: Some("home".into()),
            spaces: BTreeMap::from([
                ("home".into(), target("/k/Home.knode", true)),
                ("work".into(), target("/k/Work.knode", true)),
                ("team".into(), target("/k/Team.knode", true)),
                ("quiet".into(), target("/k/Quiet.knode", false)),
            ]),
            projects: vec![
                project("work", "/code", true, &[]),
                project("work", "/code/app", true, &["vendor/**"]),
                project("team", "/code/app", true, &[]),
                project("home", "/code/secret", false, &[]),
                project("quiet", "/code/quiet", true, &[]),
            ],
            agents: vec![AgentFolder {
                space: "team".into(),
                agent: Agent::ClaudeCode,
                path: PathBuf::from("/agents/team"),
            }],
        };
        let file = Path::new("/sessions/s.jsonl");
        let route = |project: &str| routes.route(Some(Path::new(project)), file);
        // The longest matching project wins; the same folder can feed two spaces.
        assert_eq!(route("/code/app/src"), vec!["work", "team"]);
        assert_eq!(route("/code/lib"), vec!["work"]);
        assert_eq!(route("/code"), vec!["work"]);
        // Not a folder inside /code.
        assert_eq!(route("/codex"), vec!["home"]);
        // Excluded by the rules of one of them: only the other.
        assert_eq!(route("/code/app/vendor/x"), vec!["team"]);
        // Capture off for the project, or for its space: stored nowhere, not in the default.
        assert!(route("/code/secret/deep").is_empty());
        assert!(route("/code/quiet").is_empty());
        // No project: the agent folder it was read from, else the default space.
        assert_eq!(
            routes.route(None, Path::new("/agents/team/p/s.jsonl")),
            vec!["team"]
        );
        assert_eq!(routes.route(None, file), vec!["home"]);
        assert_eq!(route("/elsewhere"), vec!["home"]);

        let no_default = Routes {
            default: None,
            ..routes.clone()
        };
        assert!(no_default.route(None, file).is_empty());
        assert!(no_default.active());
        assert!(!Routes::default().active());
    }

    #[test]
    fn routes_match_paths_in_the_home_folder() {
        let home = std::env::var("HOME").unwrap();
        let routes = Routes {
            default: None,
            spaces: BTreeMap::from([("work".into(), target("/k/Work.knode", true))]),
            projects: vec![project("work", "~/Developer/knode", true, &[])],
            agents: Vec::new(),
        };
        let held = PathBuf::from(&home).join("Developer/knode/core");
        assert_eq!(
            routes.route(Some(&held), Path::new("/s.jsonl")),
            vec!["work"]
        );
        let other = PathBuf::from(&home).join("Developer/knode-site");
        assert!(routes.route(Some(&other), Path::new("/s.jsonl")).is_empty());
    }
}

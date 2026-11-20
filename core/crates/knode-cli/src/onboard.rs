//! JSON boundary used by the native onboarding tutorial; planning/import logic is in the core.
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use knode_config::{Config, Paths};
use knode_format::{GrantLevel, NodeId, Space, kinds, link_kinds, property_keys};
use knode_ingest::{Agent, CaptureError, Conversation, Destinations};
use knode_vault::onboarding::{self, Mode, Plan};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Print the step-by-step guide an agent follows to bring your notes into Knode.
    Guide {
        /// The folder the notes are in, written into the guide's commands.
        #[arg(long)]
        source: Option<PathBuf>,
    },
    /// Print the short prompt to paste into an agent: it asks the agent to follow the guide.
    Prompt {
        #[arg(long)]
        source: Option<PathBuf>,
    },
    /// Analyze a source folder without changing it; print a reviewable JSON proposal.
    Analyze {
        #[arg(long)]
        source: Option<PathBuf>,
        /// vault, project or empty.
        #[arg(long, value_parser=["vault","project","empty"])]
        mode: String,
    },
    /// Import a reviewed JSON proposal into a new independent .knode package, or with `--into`
    /// into an existing space that is not set up yet.
    Apply {
        #[arg(long)]
        plan: PathBuf,
        /// The new .knode package to create.
        #[arg(long, required_unless_present = "into")]
        destination: Option<PathBuf>,
        /// The name of the new space.
        #[arg(long, required_unless_present = "into")]
        name: Option<String>,
        /// Apply into this existing space (a name or a .knode path) instead of a new one, such
        /// as the space the agent is already connected to. Refused when the space is already set
        /// up: it has an onboarding receipt, projects or a few notes of its own. What it holds
        /// stays; capture is turned off, as in a new onboarding space.
        #[arg(long, conflicts_with_all = ["destination", "name"])]
        into: Option<String>,
        /// Also bring the agent conversations (Claude Code, Codex and the other agents Knode
        /// reads) held inside the folders of the projects kept, each part of its project, with
        /// obvious secrets replaced. Off unless given; `onboard sessions --plan` lists them first.
        #[arg(long)]
        sessions: bool,
    },
    /// List the agent conversations onboarding would bring in, by project, without writing
    /// anything; with `--target` and `--apply`, bring them into an existing onboarding space
    /// (such as after an interrupted `apply --sessions`).
    Sessions {
        /// A reviewed plan, before it is applied.
        #[arg(long, conflicts_with = "target", required_unless_present = "target")]
        plan: Option<PathBuf>,
        /// An onboarding space, after it is applied.
        #[arg(long)]
        target: Option<PathBuf>,
        /// Bring the conversations into `--target`.
        #[arg(long, requires = "target")]
        apply: bool,
    },
    /// Resolve a native note link through the core (ids, titles, imported paths and headings).
    ResolveLink {
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long)]
        reference: String,
    },
    /// Inspect the original import report and proposal of an existing copy.
    Inspect {
        #[arg(long)]
        target: PathBuf,
    },
    /// Connect Codex while keeping a private, guarded configuration recovery snapshot.
    ConnectCodex {
        #[arg(long)]
        target: PathBuf,
    },
    /// Archive without deleting; revoke only this copy's grants and flush open service files.
    Archive {
        #[arg(long)]
        target: PathBuf,
    },
}
pub fn run(paths: &Paths, action: Action) -> Result<()> {
    match action {
        Action::Guide { source } => {
            let source = source.map(|s| crate::absolute(&s)).transpose()?;
            print!("{}", onboarding::agent_guide(source.as_deref()));
        }
        Action::Prompt { source } => {
            let source = source.map(|s| crate::absolute(&s)).transpose()?;
            println!("{}", onboarding::agent_prompt(source.as_deref()));
        }
        Action::Analyze { source, mode } => {
            let mode = match mode.as_str() {
                "vault" => Mode::Vault,
                "project" => Mode::Project,
                _ => Mode::Empty,
            };
            println!(
                "{}",
                serde_json::to_string(&onboarding::analyze(source.as_deref(), mode)?)?
            );
        }
        Action::Apply {
            plan,
            destination,
            name,
            into,
            sessions,
        } => {
            let plan: Plan = serde_json::from_slice(&std::fs::read(plan)?)?;
            let receipt = match (into, destination, name) {
                (Some(into), _, _) => {
                    apply_into(&plan, &crate::spaces::find(paths, &into)?, &crate::person())?
                }
                (None, Some(destination), Some(name)) => onboarding::apply(
                    &plan,
                    &crate::absolute(&destination)?,
                    &name,
                    &crate::person(),
                )?,
                _ => bail!("Give --destination and --name, or --into"),
            };
            let mut out = serde_json::to_value(&receipt)?;
            if sessions {
                // The space and its receipt exist now: a failure here is reported with the
                // receipt, and `onboard sessions --target <space> --apply` tries again.
                match bring_sessions(&receipt, None) {
                    Ok(brought) => out["conversations"] = serde_json::to_value(&brought)?,
                    Err(error) => {
                        let done = if receipt.applied_into {
                            "The plan was applied"
                        } else {
                            "The space was created"
                        };
                        let message = format!(
                            "{done}, but its agent conversations could not be brought in: {error:#}. Run `knode onboard sessions --target \"{}\" --apply` to try again.",
                            receipt.destination.display()
                        );
                        eprintln!("{message}");
                        out["conversations_error"] = serde_json::Value::String(message);
                    }
                }
            }
            println!("{}", serde_json::to_string(&out)?);
        }
        Action::Sessions {
            plan,
            target,
            apply,
        } => {
            let out = match (plan, target) {
                (Some(plan), _) => {
                    let plan: Plan = serde_json::from_slice(&std::fs::read(plan)?)?;
                    serde_json::to_value(preview_sessions(&plan, None, None)?)?
                }
                (None, Some(target)) => {
                    let receipt = read_receipt(&target)?;
                    if apply {
                        serde_json::to_value(bring_sessions(&receipt, None)?)?
                    } else {
                        serde_json::to_value(preview_sessions(
                            &receipt.plan,
                            Some(&receipt.contexts),
                            None,
                        )?)?
                    }
                }
                (None, None) => bail!("Give --plan or --target"),
            };
            println!("{}", serde_json::to_string(&out)?);
        }
        Action::ResolveLink {
            target,
            from,
            reference,
        } => {
            let space = knode_format::Space::open(target)?;
            let id = match space.resolve_link(&knode_format::NodeId::parse(&from)?, &reference)? {
                Some(knode_format::LinkTarget::Node(node)) => Some(node.id.to_string()),
                _ => None,
            };
            println!("{}", serde_json::json!({"node_id": id}));
        }
        Action::Inspect { target } => {
            println!("{}", serde_json::to_string(&read_receipt(&target)?)?);
        }
        Action::ConnectCodex { target } => {
            let target = target.canonicalize()?;
            let manifest = knode_format::Manifest::read(&target)?;
            let receipt: onboarding::Receipt =
                serde_json::from_slice(&std::fs::read(target.join("onboarding.json"))?)?;
            if receipt.space_id != manifest.id {
                bail!("Import receipt does not match this space");
            }
            let backup_path = recovery_path(paths, &manifest.id)?;
            let config = crate::connect::codex_config()?;
            let current = read_optional(&config)?;
            let is_new = !backup_path.exists();
            let mut backup = if is_new {
                ConnectionRecovery {
                    config: config.clone(),
                    before: current.clone(),
                    after: None,
                }
            } else {
                let previous: ConnectionRecovery =
                    serde_json::from_slice(&std::fs::read(&backup_path)?)?;
                if previous.config != config
                    || (current != previous.before && current != previous.after)
                {
                    bail!(
                        "Codex configuration changed since setup. Review it before reconnecting; the recovery point was preserved."
                    );
                }
                previous
            };
            if current == backup.before {
                backup.after = None;
            }
            write_recovery(&backup_path, &backup, is_new)?;
            crate::connect::run(paths, crate::connect::Agent::Codex, false, Some(&target))?;
            backup.after = read_optional(&config)?;
            write_recovery(&backup_path, &backup, false)?;
        }
        Action::Archive { target } => {
            let target = target.canonicalize()?;
            if target.extension().and_then(|s| s.to_str()) != Some("knode") {
                bail!("Only .knode import copies can be archived");
            }
            // Validate ownership evidence before any configuration/grant mutation.
            let receipt: onboarding::Receipt = serde_json::from_slice(
                &std::fs::read(target.join("onboarding.json"))
                    .context("Only onboarding-created copies can be archived here")?,
            )?;
            if knode_format::Manifest::read(&target)?.id != receipt.space_id {
                bail!("Import receipt does not match this space");
            }
            if let Ok(client) = knode_service::Client::connect(paths.service_socket()) {
                for agent in client.agents()? {
                    if agent.spaces.iter().any(|g| g.space == target) {
                        client.set_grant(
                            &agent.name,
                            &target,
                            GrantLevel::None,
                            &crate::person(),
                        )?;
                    }
                }
            }
            let mut config = Config::load(paths)?;
            let previous_count = config.spaces.len();
            let was_default = config.default_space.as_ref() == Some(&target);
            config.spaces.retain(|p| p != &target);
            if was_default {
                config.default_space = None;
            }
            if was_default || config.spaces.len() != previous_count {
                config.save(paths)?;
            }
            let was_running = knode_service::stop_service(
                paths.service_socket(),
                std::time::Duration::from_secs(10),
            )?
            .is_some();
            let archived = onboarding::archive(&target);
            // The service was stopped only so the package could move: start it again, whatever
            // happened to the archive, so the app and agents keep working.
            if was_running {
                crate::service::connect().context("Knode Service could not start again")?;
            }
            let archived = archived?;
            let recovery = restore_connection(paths, &receipt.space_id).unwrap_or("Copy archived, but Codex configuration recovery needs review. Reconnect your preferred space in Settings.");
            let mut out =
                serde_json::json!({"archived": archived, "codex_configuration": recovery});
            // A space onboarding applied into existed before it: say that what it held then is
            // in the archive too.
            if receipt.applied_into {
                out["note"] = serde_json::Value::String("Onboarding applied into this space, which existed before; what it held before onboarding, such as captured conversations, is archived with it and comes back when the archive is reopened.".into());
            }
            println!("{}", serde_json::to_string(&out)?);
        }
    }
    Ok(())
}

/// Applies the plan into the existing space at `space`: through Knode Service, which has the
/// space open for the app and agents, or directly when the service cannot run.
fn apply_into(
    plan: &Plan,
    space: &Path,
    by: &knode_format::Provenance,
) -> Result<onboarding::Receipt> {
    match crate::service::client() {
        Some(client) => client
            .onboarding_apply_into(space, plan, by)
            .map_err(|e| match e {
                knode_service::Error::Rpc(e) => anyhow::anyhow!(e.message),
                other => anyhow::anyhow!(other),
            }),
        None => Ok(onboarding::apply_into(plan, &mut Space::open(space)?, by)?),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ConnectionRecovery {
    config: PathBuf,
    before: Option<String>,
    after: Option<String>,
}
fn read_optional(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn recovery_path(paths: &Paths, id: &str) -> Result<PathBuf> {
    knode_format::NodeId::parse(id).context("Invalid recovery identity")?;
    let folder = paths.home.join("onboarding-recovery");
    std::fs::create_dir_all(&folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(folder.join(format!("{id}.json")))
}
fn write_recovery(path: &std::path::Path, snapshot: &ConnectionRecovery, new: bool) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if new {
        options.create_new(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec(snapshot)?)?;
    file.sync_all()?;
    Ok(())
}
fn restore_connection(paths: &Paths, id: &str) -> Result<&'static str> {
    let path = recovery_path(paths, id)?;
    if !path.exists() {
        return Ok("No Codex connection was changed by this tutorial.");
    }
    let recovery: ConnectionRecovery = serde_json::from_slice(&std::fs::read(&path)?)?;
    if read_optional(&recovery.config)? != recovery.after || recovery.after.is_none() {
        return Ok(
            "Codex configuration changed after setup. It was left untouched; reconnect your preferred space in Settings.",
        );
    }
    // Reuse the guarded atomic writer; a previously absent config becomes an empty config.
    crate::instructions::save(
        &recovery.config,
        recovery.after.as_deref(),
        recovery.before.as_deref().unwrap_or(""),
    )?;
    Ok("Previous Codex configuration restored. Start a fresh Codex session.")
}

/// Each project folder of a reviewed plan, deepest first so the nearest one wins, with the
/// project its conversations join. A folder whose group was excluded or whose documents were
/// all assigned elsewhere maps to `None`, so the conversations held in it are left out rather
/// than falling to an enclosing project. With `contexts` (an applied plan), a project must also
/// have its context node.
fn project_folders(
    plan: &Plan,
    contexts: Option<&BTreeMap<String, String>>,
) -> Vec<(PathBuf, Option<String>)> {
    let Some(source) = &plan.source else {
        return Vec::new();
    };
    let mut folders: Vec<(PathBuf, Option<String>)> = plan
        .groups
        .iter()
        .filter(|g| g.id != "shared")
        .map(|g| {
            let folder = if g.id == "project" {
                source.clone()
            } else {
                source.join(&g.id)
            };
            // The same trim as `onboarding::apply`, which keys `contexts` by it.
            let name = g.name.trim().to_owned();
            let kept = !name.is_empty()
                && plan
                    .documents
                    .iter()
                    .any(|d| d.include && d.project == g.id)
                && contexts.is_none_or(|c| c.contains_key(&name));
            (folder, kept.then_some(name))
        })
        .collect();
    folders.sort_by_key(|(folder, _)| std::cmp::Reverse(folder.components().count()));
    folders
}

/// The project a conversation joins: that of the nearest project folder it was held in.
fn project_of(
    folders: &[(PathBuf, Option<String>)],
    conversation: &Conversation,
) -> Option<String> {
    let cwd = conversation.project.as_deref()?;
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    folders
        .iter()
        .find(|(folder, _)| cwd.starts_with(folder))
        .and_then(|(_, project)| project.clone())
}

/// A capture of its own, with its state in a throwaway folder, so Knode Service's capture is
/// untouched. `roots` replaces where agents keep their sessions, for tests.
fn private_capture(
    roots: Option<Vec<(Agent, PathBuf)>>,
) -> Result<(tempfile::TempDir, knode_ingest::Capture)> {
    let state = tempfile::tempdir()?;
    let mut capture = knode_ingest::Capture::open(state.path().join("capture.json"))?;
    if let Some(roots) = roots {
        capture = capture.with_roots(roots);
    }
    Ok((state, capture))
}

/// One conversation onboarding would bring in.
#[derive(Debug, Serialize)]
pub(crate) struct SessionPreview {
    pub agent: String,
    pub session: String,
    pub title: String,
    pub messages: usize,
    /// Obvious secrets in it, replaced with `[redacted]` when it is brought in.
    pub secrets: usize,
}

/// The conversations onboarding would bring in, by project, before anything is written.
#[derive(Debug, Default, Serialize)]
pub(crate) struct SessionsPreview {
    pub projects: BTreeMap<String, Vec<SessionPreview>>,
    pub total: usize,
    pub secrets: usize,
    /// Session files that could not be read.
    pub failed: Vec<String>,
}

/// Lists, without writing anything, the agent conversations held inside the plan's project
/// folders and the project each would join.
pub(crate) fn preview_sessions(
    plan: &Plan,
    contexts: Option<&BTreeMap<String, String>>,
    roots: Option<Vec<(Agent, PathBuf)>>,
) -> Result<SessionsPreview> {
    struct Look<'a> {
        folders: &'a [(PathBuf, Option<String>)],
        found: RefCell<BTreeMap<(String, String), (String, SessionPreview)>>,
    }
    impl Destinations for Look<'_> {
        fn routing(&self) -> String {
            "preview".into()
        }
        fn route(&self, conversation: &Conversation, _: &Path) -> Vec<String> {
            if conversation.is_empty() {
                return Vec::new();
            }
            if let Some(project) = project_of(self.folders, conversation) {
                let preview = SessionPreview {
                    agent: conversation.agent.id().to_string(),
                    session: conversation.session_id.clone(),
                    title: knode_ingest::redact_secrets(&conversation.title()).0,
                    messages: conversation.messages.len(),
                    secrets: knode_ingest::count_secrets(conversation),
                };
                self.found.borrow_mut().insert(
                    (preview.agent.clone(), preview.session.clone()),
                    (project, preview),
                );
            }
            // Nothing is written while previewing.
            Vec::new()
        }
        fn with_space(
            &self,
            _: &str,
            _: &mut dyn FnMut(&mut Space) -> Result<(), CaptureError>,
        ) -> Result<(), CaptureError> {
            Ok(())
        }
    }

    let mut preview = SessionsPreview::default();
    let folders = project_folders(plan, contexts);
    if folders.iter().all(|(_, project)| project.is_none()) {
        return Ok(preview);
    }
    let look = Look {
        folders: &folders,
        found: RefCell::new(BTreeMap::new()),
    };
    let (_state, mut capture) = private_capture(roots)?;
    let report = capture.sync_all_to(&look)?;
    preview.failed = report
        .failed
        .iter()
        .map(|(path, reason)| format!("{}: {reason}", path.display()))
        .collect();
    for (project, session) in look.found.into_inner().into_values() {
        preview.total += 1;
        preview.secrets += session.secrets;
        preview.projects.entry(project).or_default().push(session);
    }
    Ok(preview)
}

/// What bringing conversations into an onboarding space did.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Brought {
    /// Conversations written, new or brought up to date.
    pub brought: usize,
    /// Conversations joined to their project, by project.
    pub projects: BTreeMap<String, usize>,
    /// Secrets replaced with `[redacted]`.
    pub redacted: usize,
    /// Session files that could not be read.
    pub failed: Vec<String>,
}

/// Brings into an onboarding space the agent conversations held inside the projects the person
/// kept, each `part-of` the project whose folder is nearest, with obvious secrets replaced.
/// Others are left out. Running it again over the same space finds the conversations already
/// there instead of adding them twice, so a failed run can be retried.
pub(crate) fn bring_sessions(
    receipt: &onboarding::Receipt,
    roots: Option<Vec<(Agent, PathBuf)>>,
) -> Result<Brought> {
    struct Route<'a> {
        folders: &'a [(PathBuf, Option<String>)],
        key: String,
        space: RefCell<Space>,
        projects: RefCell<BTreeMap<(String, String), String>>,
    }
    impl Destinations for Route<'_> {
        fn routing(&self) -> String {
            self.key.clone()
        }
        fn route(&self, conversation: &Conversation, _: &Path) -> Vec<String> {
            match project_of(self.folders, conversation) {
                Some(project) => {
                    self.projects.borrow_mut().insert(
                        (
                            conversation.agent.id().to_string(),
                            conversation.session_id.clone(),
                        ),
                        project,
                    );
                    vec![self.key.clone()]
                }
                None => Vec::new(),
            }
        }
        fn with_space(
            &self,
            _key: &str,
            write: &mut dyn FnMut(&mut Space) -> Result<(), CaptureError>,
        ) -> Result<(), CaptureError> {
            write(&mut self.space.borrow_mut())
        }
    }

    let folders = project_folders(&receipt.plan, Some(&receipt.contexts));
    let mut brought = Brought::default();
    if folders.iter().all(|(_, project)| project.is_none()) {
        return Ok(brought);
    }
    let space = Space::open(&receipt.destination)?;
    let route = Route {
        folders: &folders,
        key: space.manifest().id.clone(),
        space: RefCell::new(space),
        projects: RefCell::new(BTreeMap::new()),
    };
    let (_state, capture) = private_capture(roots)?;
    let report = capture.with_redaction().sync_all_to(&route)?;
    brought.brought = report.changes.len();
    brought.redacted = report.redacted;
    brought.failed = report
        .failed
        .iter()
        .map(|(path, reason)| format!("{}: {reason}", path.display()))
        .collect();
    let projects = route.projects.into_inner();
    let mut space = route.space.into_inner();
    let by = crate::person();
    // Every routed conversation, including ones an earlier, interrupted run wrote, joins its
    // project once.
    for ((agent, session), project) in &projects {
        let Some(context) = receipt
            .contexts
            .get(project)
            .and_then(|id| NodeId::parse(id).ok())
        else {
            continue;
        };
        for node in space.find_by_property(property_keys::CONVERSATION_SESSION, session)? {
            if node.kind != kinds::CONVERSATION
                || space
                    .property(&node.id, property_keys::CONVERSATION_AGENT)?
                    .as_deref()
                    != Some(agent.as_str())
            {
                continue;
            }
            let linked = space
                .links_from(&node.id)?
                .iter()
                .any(|l| l.to == context && l.kind == link_kinds::PART_OF);
            if !linked {
                space.link(&node.id, &context, link_kinds::PART_OF, &by)?;
            }
            *brought.projects.entry(project.clone()).or_default() += 1;
        }
    }
    Ok(brought)
}

/// Reads the receipt of an onboarding space, checking that it belongs to it.
fn read_receipt(target: &Path) -> Result<onboarding::Receipt> {
    let target = target.canonicalize()?;
    let mut receipt: onboarding::Receipt =
        serde_json::from_slice(&std::fs::read(target.join("onboarding.json"))?)?;
    if knode_format::Manifest::read(&target)?.id != receipt.space_id {
        bail!("Import receipt does not match this space");
    }
    receipt.destination = target;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use knode_format::Provenance;

    /// A made-up Claude Code session held in `cwd`, with one user and one assistant turn.
    fn session(roots: &Path, id: &str, cwd: &Path, text: &str) {
        let folder = roots.join(format!("folder-{id}"));
        std::fs::create_dir_all(&folder).unwrap();
        let cwd = cwd.to_str().unwrap();
        let lines = [
            serde_json::json!({"type": "user", "sessionId": id, "cwd": cwd, "uuid": format!("{id}-u"), "timestamp": "2026-01-02T10:00:00Z", "message": {"role": "user", "content": text}}),
            serde_json::json!({"type": "assistant", "sessionId": id, "cwd": cwd, "uuid": format!("{id}-a"), "parentUuid": format!("{id}-u"), "timestamp": "2026-01-02T10:00:05Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "Done."}]}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(folder.join(format!("{id}.jsonl")), body).unwrap();
    }

    struct Fixture {
        dir: tempfile::TempDir,
        source: PathBuf,
        roots: PathBuf,
    }

    impl Fixture {
        /// A vault with the projects `alpha` and `beta`, and sessions held in alpha, beta and
        /// elsewhere. Every value is made up.
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join("vault");
            for project in ["alpha", "beta"] {
                std::fs::create_dir_all(source.join(project).join("src")).unwrap();
                std::fs::write(source.join(project).join("README.md"), "A project.").unwrap();
            }
            let source = source.canonicalize().unwrap();
            let roots = dir.path().canonicalize().unwrap().join("claude/projects");
            session(
                &roots,
                "0f8e2c1a-1111-4222-8333-000000000001",
                &source.join("alpha/src"),
                "Wire it up with API_KEY=abcd1234efgh5678 please.",
            );
            session(
                &roots,
                "0f8e2c1a-1111-4222-8333-000000000002",
                &source.join("beta"),
                "Plan the beta release.",
            );
            session(
                &roots,
                "0f8e2c1a-1111-4222-8333-000000000003",
                Path::new("/nonexistent/example/other"),
                "Unrelated work.",
            );
            Fixture { dir, source, roots }
        }
        fn plan(&self) -> Plan {
            onboarding::analyze(Some(&self.source), Mode::Vault).unwrap()
        }
        fn roots(&self) -> Option<Vec<(Agent, PathBuf)>> {
            Some(vec![(Agent::ClaudeCode, self.roots.clone())])
        }
        fn apply(&self, plan: &Plan) -> onboarding::Receipt {
            onboarding::apply(
                plan,
                &self.dir.path().canonicalize().unwrap().join("Work.knode"),
                "Work",
                &Provenance::person("Tester"),
            )
            .unwrap()
        }
    }

    fn conversations_of(receipt: &onboarding::Receipt, project: &str) -> Vec<String> {
        let space = Space::open(&receipt.destination).unwrap();
        let context = NodeId::parse(&receipt.contexts[project]).unwrap();
        space
            .backlinks(&context)
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == link_kinds::PART_OF)
            .filter_map(|l| space.get_node(&l.from).unwrap())
            .filter(|n| n.kind == kinds::CONVERSATION)
            .map(|n| n.body)
            .collect()
    }

    fn all_conversations(receipt: &onboarding::Receipt) -> usize {
        let space = Space::open(&receipt.destination).unwrap();
        space
            .recent(1000)
            .unwrap()
            .into_iter()
            .filter(|n| n.kind == kinds::CONVERSATION)
            .count()
    }

    #[test]
    fn conversations_held_in_a_project_folder_join_that_project() {
        let fixture = Fixture::new();
        let receipt = fixture.apply(&fixture.plan());
        let brought = bring_sessions(&receipt, fixture.roots()).unwrap();
        assert_eq!(brought.brought, 2);
        assert_eq!(conversations_of(&receipt, "alpha").len(), 1);
        assert_eq!(conversations_of(&receipt, "beta").len(), 1);
        assert_eq!(all_conversations(&receipt), 2);
    }

    #[test]
    fn the_preview_lists_sessions_by_project_and_writes_nothing() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let preview = preview_sessions(&plan, None, fixture.roots()).unwrap();
        assert_eq!(preview.total, 2);
        assert_eq!(preview.projects["alpha"].len(), 1);
        assert_eq!(preview.projects["beta"].len(), 1);
        assert_eq!(preview.projects["alpha"][0].secrets, 1);
        assert_eq!(preview.secrets, 1);
        // Previewing before applying leaves no space behind.
        assert!(!fixture.dir.path().join("Work.knode").exists());
        // After applying, the preview of the space is the same and still writes nothing.
        let receipt = fixture.apply(&plan);
        let again =
            preview_sessions(&receipt.plan, Some(&receipt.contexts), fixture.roots()).unwrap();
        assert_eq!(again.total, 2);
        assert_eq!(all_conversations(&receipt), 0);
    }

    #[test]
    fn excluded_and_reassigned_projects_bring_no_conversations() {
        let fixture = Fixture::new();
        // Exclude beta's documents.
        let mut plan = fixture.plan();
        for doc in &mut plan.documents {
            if doc.project == "beta" {
                doc.include = false;
            }
        }
        let preview = preview_sessions(&plan, None, fixture.roots()).unwrap();
        assert_eq!(preview.total, 1);
        assert!(!preview.projects.contains_key("beta"));
        let receipt = fixture.apply(&plan);
        bring_sessions(&receipt, fixture.roots()).unwrap();
        assert_eq!(all_conversations(&receipt), 1);

        // Reassign beta's documents to alpha: beta's folder no longer holds a project.
        let mut plan = fixture.plan();
        for doc in &mut plan.documents {
            if doc.project == "beta" {
                doc.project = "alpha".into();
            }
        }
        let preview = preview_sessions(&plan, None, fixture.roots()).unwrap();
        assert_eq!(preview.total, 1);
        assert_eq!(preview.projects["alpha"].len(), 1);
    }

    #[test]
    fn group_names_are_trimmed_like_apply_does() {
        let fixture = Fixture::new();
        let mut plan = fixture.plan();
        for group in &mut plan.groups {
            if group.id == "alpha" {
                group.name = "  Alpha  ".into();
            }
        }
        let receipt = fixture.apply(&plan);
        assert!(receipt.contexts.contains_key("Alpha"));
        let brought = bring_sessions(&receipt, fixture.roots()).unwrap();
        assert_eq!(brought.projects["Alpha"], 1);
        assert_eq!(conversations_of(&receipt, "Alpha").len(), 1);
    }

    #[test]
    fn secrets_are_redacted_from_imported_transcripts() {
        let fixture = Fixture::new();
        let receipt = fixture.apply(&fixture.plan());
        let brought = bring_sessions(&receipt, fixture.roots()).unwrap();
        assert_eq!(brought.redacted, 1);
        let alpha = conversations_of(&receipt, "alpha");
        assert!(alpha[0].contains("API_KEY=[redacted]"), "{}", alpha[0]);
        assert!(!alpha[0].contains("abcd1234efgh5678"));
    }

    #[test]
    fn bringing_again_into_the_same_space_does_not_duplicate() {
        let fixture = Fixture::new();
        let receipt = fixture.apply(&fixture.plan());
        // An interrupted run: one conversation written but never joined to its project.
        {
            let mut space = Space::open(&receipt.destination).unwrap();
            let roots = fixture.dir.path().join("partial");
            session(
                &roots,
                "0f8e2c1a-1111-4222-8333-000000000002",
                &fixture.source.join("beta"),
                "Plan the beta release.",
            );
            let (_state, mut capture) =
                private_capture(Some(vec![(Agent::ClaudeCode, roots)])).unwrap();
            capture.sync_all(&mut space).unwrap();
        }
        assert_eq!(all_conversations(&receipt), 1);
        assert!(conversations_of(&receipt, "beta").is_empty());

        // The retry, through the saved receipt, as `onboard sessions --target --apply` does.
        let saved = read_receipt(&receipt.destination).unwrap();
        bring_sessions(&saved, fixture.roots()).unwrap();
        bring_sessions(&saved, fixture.roots()).unwrap();
        assert_eq!(all_conversations(&receipt), 2);
        assert_eq!(conversations_of(&receipt, "alpha").len(), 1);
        assert_eq!(conversations_of(&receipt, "beta").len(), 1);
    }

    #[test]
    fn capture_state_is_private_and_removed() {
        let (state, capture) = private_capture(None).unwrap();
        let folder = state.path().to_path_buf();
        assert!(capture.state_path().starts_with(&folder));
        drop(capture);
        drop(state);
        assert!(!folder.exists());
    }
}

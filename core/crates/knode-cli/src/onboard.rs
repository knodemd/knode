//! JSON boundary used by the native onboarding tutorial; planning/import logic is in the core.
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use knode_config::{Config, Paths};
use knode_format::GrantLevel;
use knode_vault::onboarding::{self, Mode, Plan};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Analyze a source folder without changing it; print a reviewable JSON proposal.
    Analyze {
        #[arg(long)]
        source: Option<PathBuf>,
        /// vault, project or empty.
        #[arg(long, value_parser=["vault","project","empty"])]
        mode: String,
    },
    /// Import a reviewed JSON proposal into a new independent .knode package.
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        name: String,
        /// Also bring the agent conversations (Claude Code, Codex and the other agents Knode
        /// reads) held inside the source's project folders, each part of its project.
        #[arg(long)]
        sessions: bool,
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
            sessions,
        } => {
            let plan: Plan = serde_json::from_slice(&std::fs::read(plan)?)?;
            let receipt = onboarding::apply(
                &plan,
                &crate::absolute(&destination)?,
                &name,
                &crate::person(),
            )?;
            let mut out = serde_json::to_value(&receipt)?;
            if sessions {
                let conversations = bring_sessions(&plan, &receipt, None)?;
                out["conversations"] = serde_json::json!(conversations);
            }
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
            let target = target.canonicalize()?;
            let mut receipt: onboarding::Receipt =
                serde_json::from_slice(&std::fs::read(target.join("onboarding.json"))?)?;
            if knode_format::Manifest::read(&target)?.id != receipt.space_id {
                bail!("Import receipt does not match this space");
            }
            receipt.destination = target;
            println!("{}", serde_json::to_string(&receipt)?);
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
            println!(
                "{}",
                serde_json::json!({"archived": archived, "codex_configuration": recovery})
            );
        }
    }
    Ok(())
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

/// Brings into a new onboarding space the agent conversations held inside the source's project
/// folders, each `part-of` the project whose folder is nearest, and returns how many. Others are
/// left out. A capture state of its own, thrown away, keeps Knode Service's capture untouched.
/// `roots` replaces where agents keep their sessions, for tests.
pub(crate) fn bring_sessions(
    plan: &Plan,
    receipt: &onboarding::Receipt,
    roots: Option<Vec<(knode_ingest::Agent, PathBuf)>>,
) -> Result<usize> {
    use knode_format::{NodeId, Space, link_kinds};
    use knode_ingest::{Capture, CaptureError, Destinations};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;

    let Some(source) = &plan.source else {
        return Ok(0);
    };
    // Each project's folder, deepest first so the nearest one wins.
    let mut folders: Vec<(PathBuf, String)> = plan
        .groups
        .iter()
        .filter(|g| g.id != "shared")
        .map(|g| {
            let folder = if g.id == "project" {
                source.clone()
            } else {
                source.join(&g.id)
            };
            (folder, g.name.clone())
        })
        .collect();
    folders.sort_by_key(|(folder, _)| std::cmp::Reverse(folder.components().count()));

    struct Route<'a> {
        folders: &'a [(PathBuf, String)],
        key: String,
        space: RefCell<Space>,
        projects: RefCell<HashMap<PathBuf, String>>,
    }
    impl Destinations for Route<'_> {
        fn routing(&self) -> String {
            self.key.clone()
        }
        fn route(&self, conversation: &knode_ingest::Conversation, file: &Path) -> Vec<String> {
            let Some(cwd) = conversation.project.as_deref() else {
                return Vec::new();
            };
            let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
            match self
                .folders
                .iter()
                .find(|(folder, _)| cwd.starts_with(folder))
            {
                Some((_, project)) => {
                    self.projects
                        .borrow_mut()
                        .insert(file.to_path_buf(), project.clone());
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

    let space = Space::open(&receipt.destination)?;
    let route = Route {
        folders: &folders,
        key: space.manifest().id.clone(),
        space: RefCell::new(space),
        projects: RefCell::new(HashMap::new()),
    };
    let state = std::env::temp_dir().join(format!(
        "knode-onboard-capture-{}-{}",
        std::process::id(),
        receipt.space_id
    ));
    std::fs::create_dir_all(&state)?;
    let mut capture = Capture::open(state.join("capture.json"))?;
    if let Some(roots) = roots {
        capture = capture.with_roots(roots);
    }
    let report = capture.sync_all_to(&route);
    let _ = std::fs::remove_dir_all(&state);
    let report = report?;
    let projects = route.projects.into_inner();
    let mut space = route.space.into_inner();
    let by = crate::person();
    let mut brought = 0;
    for captured in report.created() {
        brought += 1;
        let Some(project) = projects.get(&captured.path) else {
            continue;
        };
        if let Some(id) = receipt
            .contexts
            .get(project)
            .and_then(|id| NodeId::parse(id).ok())
        {
            space.link(&captured.node_id, &id, link_kinds::PART_OF, &by)?;
        }
    }
    Ok(brought)
}

#[cfg(test)]
mod tests {
    use super::*;
    use knode_format::{Space, link_kinds};

    #[test]
    fn conversations_held_in_a_project_folder_join_that_project() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vault");
        for project in ["alpha", "beta"] {
            std::fs::create_dir_all(source.join(project)).unwrap();
            std::fs::write(source.join(project).join("README.md"), "A project.").unwrap();
        }
        let source = source.canonicalize().unwrap();
        let plan = onboarding::analyze(Some(&source), Mode::Vault).unwrap();
        let receipt = onboarding::apply(
            &plan,
            &dir.path().canonicalize().unwrap().join("Work.knode"),
            "Work",
            &knode_format::Provenance::person("Tester"),
        )
        .unwrap();

        // Two Claude Code sessions: one held in alpha, one somewhere else.
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../knode-ingest/tests/fixtures/claude/projects/-Users-example-project/0f8e2c1a-1111-4222-8333-444455556666.jsonl"
        ))
        .unwrap();
        let roots = dir.path().join("claude/projects");
        for (folder, cwd, id) in [
            (
                "in-alpha",
                source.join("alpha/src"),
                "0f8e2c1a-1111-4222-8333-444455556666",
            ),
            (
                "elsewhere",
                PathBuf::from("/Users/example/other"),
                "1f8e2c1a-1111-4222-8333-444455556666",
            ),
        ] {
            let text = fixture
                .replace("/Users/example/project", cwd.to_str().unwrap())
                .replace("0f8e2c1a-1111-4222-8333-444455556666", id);
            std::fs::create_dir_all(roots.join(folder)).unwrap();
            std::fs::write(roots.join(folder).join(format!("{id}.jsonl")), text).unwrap();
        }
        let brought = bring_sessions(
            &plan,
            &receipt,
            Some(vec![(knode_ingest::Agent::ClaudeCode, roots.clone())]),
        )
        .unwrap();
        assert_eq!(brought, 1);

        let space = Space::open(&receipt.destination).unwrap();
        let alpha = knode_format::NodeId::parse(&receipt.contexts["alpha"]).unwrap();
        let conversations: Vec<_> = space
            .backlinks(&alpha)
            .unwrap()
            .into_iter()
            .filter(|l| l.kind == link_kinds::PART_OF)
            .filter_map(|l| space.get_node(&l.from).unwrap())
            .filter(|n| n.kind == knode_format::kinds::CONVERSATION)
            .collect();
        assert_eq!(conversations.len(), 1);
    }
}

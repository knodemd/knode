//! Preview and install project-local instructions, without configuring an agent or its grants.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Agent {
    Codex,
    ClaudeCode,
}
impl Agent {
    fn filename(self) -> &'static str {
        match self {
            Self::Codex => "AGENTS.md",
            Self::ClaudeCode => "CLAUDE.md",
        }
    }
}
#[derive(Debug, Clone, Args)]
pub struct Options {
    pub agent: Agent,
    /// Existing project folder whose instructions to preview or change.
    #[arg(long)]
    pub project: PathBuf,
    /// Apply the previewed change; without this flag nothing is written.
    #[arg(long)]
    pub apply: bool,
    /// Remove only Knode's managed block; preview unless --apply is also given.
    #[arg(long)]
    pub remove: bool,
}

pub fn run(options: Options) -> Result<()> {
    let project = options
        .project
        .canonicalize()
        .context("project folder does not exist")?;
    if !project.is_dir() {
        bail!("project must be a directory");
    }
    let path = project.join(options.agent.filename());
    let original = read(&path)?;
    let updated = knode_config::workflow::edit(original.as_deref().unwrap_or(""), options.remove)?;
    println!("Project instructions: {}", path.display());
    println!("This does not connect an agent, change its space or permissions, or enable capture.");
    if matches!(options.agent, Agent::Codex) && project.join("AGENTS.override.md").exists() {
        bail!(
            "AGENTS.override.md takes precedence here; resolve it before installing AGENTS.md guidance"
        );
    }
    let Some(updated) = updated else {
        println!("Already up to date.");
        return Ok(());
    };
    if !options.apply {
        println!(
            "Preview only. Run again with --apply to {} Knode's managed block.",
            if options.remove {
                "remove"
            } else {
                "install/update"
            }
        );
        println!("--- proposed file ---\n{updated}");
        return Ok(());
    }
    let backup = save(&path, original.as_deref(), &updated)?;
    if let Some(backup) = backup {
        println!("Backup: {}", backup.display());
    }
    println!(
        "Instructions updated. Start a fresh agent session in this project to load them. Parent/global overrides and client settings may affect instruction loading; verify the active instructions with the agent."
    );
    Ok(())
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            bail!("refusing non-regular instruction file {}", path.display())
        }
        Ok(_) => Ok(Some(
            fs::read_to_string(path).context("instructions must be UTF-8")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn create_unique(path: &Path, suffix: &str) -> Result<(PathBuf, fs::File)> {
    for i in 0..1000 {
        let candidate = path.with_file_name(format!(
            "{}.{}.{i}",
            path.file_name().unwrap().to_string_lossy(),
            suffix
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("no free backup/staging filename next to {}", path.display())
}

fn save(path: &Path, original: Option<&str>, updated: &str) -> Result<Option<PathBuf>> {
    if read(path)?.as_deref() != original {
        bail!("instructions changed since preview; retry");
    }
    let permissions = fs::metadata(path).ok().map(|m| m.permissions());
    let backup = if let Some(text) = original {
        let (name, mut file) = create_unique(path, "knode-backup")?;
        if let Some(p) = &permissions {
            file.set_permissions(p.clone())?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Some(name)
    } else {
        None
    };
    let (staging, mut file) = create_unique(path, "knode-staging")?;
    let result = (|| -> Result<()> {
        if let Some(p) = permissions {
            file.set_permissions(p)?;
        }
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        if read(path)?.as_deref() != original {
            bail!("instructions changed while preparing update; retry");
        }
        fs::rename(&staging, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_does_not_write_apply_is_idempotent_and_remove_keeps_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# Build\nRun cargo test.\n").unwrap();
        let run_with = |apply, remove| {
            run(Options {
                agent: Agent::Codex,
                project: dir.path().into(),
                apply,
                remove,
            })
        };
        run_with(false, false).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Build\nRun cargo test.\n"
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        run_with(true, false).unwrap();
        let installed = fs::read_to_string(&path).unwrap();
        run_with(true, false).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        run_with(true, true).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# Build\nRun cargo test.\n")
        );
        assert_eq!(
            fs::read_to_string(path.with_file_name("AGENTS.md.knode-backup.0")).unwrap(),
            "# Build\nRun cargo test.\n"
        );
        assert_eq!(
            fs::read_to_string(path.with_file_name("AGENTS.md.knode-backup.1")).unwrap(),
            installed
        );
    }
    #[test]
    fn refuses_overrides_and_non_utf8_without_changes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.override.md"), "priority").unwrap();
        assert!(
            run(Options {
                agent: Agent::Codex,
                project: dir.path().into(),
                apply: true,
                remove: false
            })
            .is_err()
        );
        assert!(!dir.path().join("AGENTS.md").exists());
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, [0xff]).unwrap();
        assert!(read(&path).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_and_preserves_file_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        let target = dir.path().join("target");
        fs::write(&target, "rules").unwrap();
        symlink(&target, &path).unwrap();
        assert!(read(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, "rules").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let backup = save(&path, Some("rules"), "updated").unwrap().unwrap();
        for p in [path, backup] {
            assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

//! `knode index`: search by meaning. Show how far the spaces are indexed, index them now, or
//! turn it off.
//!
//! Knode Service does the work: it downloads the search model once and indexes the open spaces
//! in the background. These commands ask it, and change `embeddings` in `config.toml`.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::Subcommand;
use knode_config::{Config, Paths};
use knode_service::{Client, IndexStatus, ModelState, SpaceIndex};

/// How often `knode index run` asks how far the index is.
const POLL: Duration = Duration::from_millis(250);
/// How often it prints that.
const REPORT: Duration = Duration::from_secs(2);

#[derive(Debug, Subcommand)]
pub enum IndexAction {
    /// Show whether search by meaning is on, the model and how far each space is indexed.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Turn search by meaning on and index the space now, waiting until it is done. The first
    /// time, the search model (about 490 MB) is downloaded once.
    Run,
    /// Turn search by meaning off: nothing is downloaded or indexed, and search is by words.
    Off,
}

pub fn run(paths: &Paths, space: &Path, action: IndexAction) -> Result<()> {
    match action {
        IndexAction::Status { json } => status(space, json),
        IndexAction::Run => index_now(paths, space),
        IndexAction::Off => off(paths),
    }
}

fn status(space: &Path, json: bool) -> Result<()> {
    let client = crate::service::connect()?;
    client.open_space(space)?;
    let status = client.index_status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    for line in describe(&status) {
        println!("{line}");
    }
    Ok(())
}

/// The status, in lines for people.
pub fn describe(status: &IndexStatus) -> Vec<String> {
    let mut lines = Vec::new();
    if !status.available {
        lines.push(
            "Search by meaning: not available in this build of Knode; search is by words."
                .to_string(),
        );
        return lines;
    }
    if !status.enabled {
        lines.push(
            "Search by meaning: off; search is by words (`knode index run` turns it on)."
                .to_string(),
        );
        return lines;
    }
    if !status.running {
        lines.push("Search by meaning: on, but this Knode Service does not index.".to_string());
        return lines;
    }
    lines.push("Search by meaning: on, on this machine".to_string());
    lines.push(format!(
        "Model: {}, {}",
        status.model.name,
        model_state(status)
    ));
    if let Some(reason) = &status.paused {
        lines.push(format!(
            "Paused: {reason} (`knode index run` indexes anyway)"
        ));
    }
    for space in &status.spaces {
        lines.push(space_line(space));
    }
    lines
}

fn model_state(status: &IndexStatus) -> String {
    let model = &status.model;
    match model.state {
        ModelState::Unavailable => "not available".into(),
        ModelState::Missing => format!("not downloaded yet ({})", megabytes(model.size)),
        ModelState::Downloading => format!(
            "downloading {}% ({} of {})",
            model.downloaded * 100 / model.size.max(1),
            megabytes(model.downloaded),
            megabytes(model.size)
        ),
        ModelState::Downloaded => "downloaded".into(),
        ModelState::Loading => "loading".into(),
        ModelState::Ready => "ready".into(),
        ModelState::Failed => format!(
            "failed: {}",
            model.error.as_deref().unwrap_or("unknown error")
        ),
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

fn space_line(space: &SpaceIndex) -> String {
    let name = space
        .space
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| space.space.display().to_string());
    let notes = plural(space.nodes, "note");
    let state = if let Some(error) = &space.error {
        format!("error: {error}")
    } else if space.waiting > 0 {
        format!("{} waiting", space.waiting)
    } else if space.indexing {
        "indexing".to_string()
    } else {
        "indexed".to_string()
    };
    format!("{name}: {notes}, {state}")
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn index_now(paths: &Paths, space: &Path) -> Result<()> {
    let mut config = Config::load(paths)?;
    if !config.embeddings_enabled() {
        config.set_embeddings(true);
        config.save(paths)?;
        println!("Search by meaning is on.");
    }
    let client = crate::service::connect()?;
    let canonical = std::fs::canonicalize(space).unwrap_or_else(|_| space.to_path_buf());
    let status = client.index_run(Some(space))?;
    if !status.available {
        bail!("this build of Knode has no search by meaning; search is by words");
    }
    if !status.running {
        bail!("Knode Service does not index spaces; restart it with `knode service stop`");
    }
    if !status.enabled {
        bail!(
            "search by meaning is off (KNODE_EMBEDDINGS=off in the environment of Knode Service)"
        );
    }
    wait(&client, &canonical)
}

/// Waits until the space is indexed, printing progress now and then.
fn wait(client: &Client, space: &Path) -> Result<()> {
    let mut last: Option<(Instant, String)> = None;
    loop {
        let status = client.index_status()?;
        if status.model.state == ModelState::Failed {
            bail!(
                "{}. `knode index run` tries again",
                status
                    .model
                    .error
                    .as_deref()
                    .unwrap_or("the search model failed")
            );
        }
        let entry = status.spaces.iter().find(|s| s.space == space);
        if let Some(entry) = entry {
            if let Some(error) = &entry.error {
                bail!("cannot index {}: {error}", space.display());
            }
            if entry.waiting == 0 && !entry.indexing {
                println!("{}", space_line(entry));
                return Ok(());
            }
        }
        let line = match (status.model.state, entry) {
            (ModelState::Missing | ModelState::Downloading, _) => {
                format!("Search model: {}", model_state(&status))
            }
            (_, Some(entry)) => space_line(entry),
            _ => "Starting".to_string(),
        };
        let due = last
            .as_ref()
            .is_none_or(|(at, said)| *said != line && at.elapsed() >= REPORT);
        if due {
            eprintln!("{line}");
            last = Some((Instant::now(), line));
        }
        thread::sleep(POLL);
    }
}

fn off(paths: &Paths) -> Result<()> {
    let mut config = Config::load(paths)?;
    config.set_embeddings(false);
    config.save(paths)?;
    // A running service reads the setting again when asked.
    if let Ok(client) = Client::connect(paths.service_socket()) {
        client.index_status()?;
    }
    println!(
        "Search by meaning is off: search is by words. What is indexed stays; `knode index run` \
         turns it on again."
    );
    Ok(())
}

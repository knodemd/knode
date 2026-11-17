//! `knode sources`: the importers and connectors of a space. Connect a connector with the
//! history to bring in, pause it, exclude chats, channels or folders, or disconnect it.
//!
//! Through Knode Service when it runs; otherwise the settings of the space are changed directly,
//! and the connectors run the next time the service starts.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::Subcommand;
use knode_config::Paths;
use knode_format::{Provenance, Space, Timestamp};
use knode_service::sources::{
    self, Container, ContainerInfo, History, SignInStatus, SignInValues, SourceKind, SourceSecrets,
    SourceState, SourceStatus,
};

use crate::service;

/// What `knode sources` does.
#[derive(Debug, Subcommand)]
pub enum Action {
    /// List the sources of the space, connectors then importers, with their status.
    List {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Connect a connector to the space and turn it on. It brings in what happens from today
    /// on, unless `--history everything` asks for everything it has; the choice is kept.
    Connect {
        /// The connector, as `knode sources list` names it.
        source: String,
        /// How much history to bring in: `from-today` or `everything`.
        #[arg(long, value_name = "HISTORY", value_parser = ["from-today", "everything"])]
        history: Option<String>,
    },
    /// Pause a connector: it stops bringing things in until it is resumed.
    Pause {
        /// The connector.
        source: String,
    },
    /// Resume a paused connector.
    Resume {
        /// The connector.
        source: String,
    },
    /// Exclude a chat, channel or folder: the connector never reads it again. What it already
    /// brought in stays.
    Exclude {
        /// The connector.
        source: String,
        /// The id of the chat, channel or folder, as `knode sources containers` shows it.
        id: String,
        /// Its name, to show in the list.
        #[arg(long)]
        name: Option<String>,
    },
    /// Include again a chat, channel or folder excluded before.
    Include {
        /// The connector.
        source: String,
        /// The id of the chat, channel or folder.
        id: String,
    },
    /// List the chats, channels or folders of a connector, the excluded ones marked.
    Containers {
        /// The connector.
        source: String,
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run a connector now.
    Run {
        /// The connector.
        source: String,
    },
    /// Disconnect a connector from the space. What it brought in stays; connecting it again
    /// chooses the history again. A connector signed in to an account is signed out and its
    /// secrets leave the keychain.
    Disconnect {
        /// The connector.
        source: String,
    },
    /// Sign a connector in to the account it reads, step by step: Gmail opens Google's
    /// sign-in page in the browser, Mail (IMAP) asks for the server, the user name and an app
    /// password. The secrets are kept in the system keychain. `connect` does it when needed.
    SignIn {
        /// The connector: `gmail` or `imap`.
        source: String,
    },
    /// Sign a connector out of its account and forget its secrets.
    SignOut {
        /// The connector.
        source: String,
    },
}

/// Runs `knode sources` on the space at `space`.
pub fn run(paths: &Paths, space: &Path, action: Action, by: &Provenance) -> Result<()> {
    let client = service::client();
    match action {
        Action::List { json } => {
            let list = match &client {
                Some(client) => client.sources_list(space)?,
                None => sources::list_offline(&paths.home, &Space::open(space)?)?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                print_list(&list);
            }
        }
        Action::Connect { source, history } => {
            if needs_sign_in(&source) {
                let flow = SignInFlow::new(client.as_ref(), paths, &source)?;
                let status = flow.step(None, &SignInValues::default())?;
                if !status.signed_in {
                    sign_in(&flow, &source, status)?;
                }
            }
            let history = history.as_deref().and_then(History::from_id);
            let status = match &client {
                Some(client) => client.sources_connect(space, &source, history, by)?,
                None => {
                    let mut space = Space::open(space)?;
                    let known: Vec<String> = sources::builtin_connectors()
                        .iter()
                        .map(|c| c.info().id)
                        .collect();
                    sources::connect(&mut space, &known, &source, history, by)?;
                    find(sources::list_offline(&paths.home, &space)?, &source)?
                }
            };
            let history = status
                .setting
                .as_ref()
                .map_or(History::FromToday, |s| s.history);
            println!(
                "Connected {} to the space, bringing in {}.",
                status.info.name,
                match history {
                    History::FromToday => "what happens from today on",
                    History::Everything => "everything it has",
                }
            );
            if !status.info.permissions.is_empty() {
                let needs: Vec<&str> = status
                    .info
                    .permissions
                    .iter()
                    .map(|p| p.describe())
                    .collect();
                println!("It needs: {}.", needs.join(", "));
            }
        }
        Action::Pause { source } => {
            set_on(client.as_ref(), space, &source, false, by)?;
            println!("Paused {source}.");
        }
        Action::Resume { source } => {
            set_on(client.as_ref(), space, &source, true, by)?;
            println!("Resumed {source}.");
        }
        Action::Exclude { source, id, name } => {
            match &client {
                Some(client) => {
                    client.sources_exclude(space, &source, &id, name.as_deref(), by)?;
                }
                None => {
                    let container = Container::new(id.clone(), name.clone().unwrap_or_default());
                    sources::exclude(&mut Space::open(space)?, &source, container, by)?;
                }
            }
            println!(
                "Excluded {} from {source}: it is never read again.",
                name.unwrap_or(id)
            );
        }
        Action::Include { source, id } => {
            match &client {
                Some(client) => {
                    client.sources_include(space, &source, &id, by)?;
                }
                None => {
                    sources::include(&mut Space::open(space)?, &source, &id, by)?;
                }
            }
            println!("Included {id} in {source} again.");
        }
        Action::Containers { source, json } => {
            let client = client.ok_or_else(|| {
                anyhow!("listing what a connector has needs Knode Service (`knode service status`)")
            })?;
            let list = client.sources_containers(space, &source)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                print_containers(&list);
            }
        }
        Action::Run { source } => {
            let client = client.ok_or_else(|| {
                anyhow!("connectors run in Knode Service (`knode service status`)")
            })?;
            client.sources_run(space, &source)?;
            println!("Running {source} now; `knode sources list` shows how it went.");
        }
        Action::Disconnect { source } => {
            let was = match &client {
                Some(client) => client.sources_disconnect(space, &source, by)?,
                None => {
                    let mut space = Space::open(space)?;
                    let was = sources::disconnect(&mut space, &source, by)?;
                    if was {
                        sources::forget_state(&paths.home, &space.manifest().id, &source)?;
                        if needs_sign_in(&source) {
                            SignInFlow::new(None, paths, &source)?.sign_out()?;
                        }
                    }
                    was
                }
            };
            if was {
                println!("Disconnected {source}. What it brought in stays in the space.");
            } else {
                println!("{source} was not connected to this space.");
            }
        }
        Action::SignIn { source } => {
            let flow = SignInFlow::new(client.as_ref(), paths, &source)?;
            let status = flow.step(None, &SignInValues::default())?;
            if status.signed_in {
                println!(
                    "{source} is signed in to {} already; `knode sources sign-out {source}` \
                     signs it out.",
                    status.account.as_deref().unwrap_or("its account")
                );
            } else {
                sign_in(&flow, &source, status)?;
            }
        }
        Action::SignOut { source } => {
            if SignInFlow::new(client.as_ref(), paths, &source)?.sign_out()? {
                println!("Signed {source} out and forgot its secrets.");
            } else {
                println!("{source} was not signed in.");
            }
        }
    }
    Ok(())
}

/// Whether the built-in connector `id` signs in to an account.
fn needs_sign_in(id: &str) -> bool {
    sources::builtin_connectors().iter().any(|c| {
        let info = c.info();
        info.id == id && info.permissions.contains(&sources::Permission::SignIn)
    })
}

/// Where the steps of signing in are taken: Knode Service when it runs (it keeps the secrets),
/// else the connector itself in this process.
enum SignInFlow<'a> {
    Service(&'a knode_service::Client, String),
    Here {
        connector: Arc<dyn sources::Connector>,
        secrets: SourceSecrets,
    },
}

impl<'a> SignInFlow<'a> {
    fn new(client: Option<&'a knode_service::Client>, paths: &Paths, source: &str) -> Result<Self> {
        if let Some(client) = client {
            return Ok(SignInFlow::Service(client, source.to_string()));
        }
        let connector = sources::builtin_connectors()
            .into_iter()
            .find(|c| c.info().id == source)
            .ok_or_else(|| anyhow!("no connector is named {source:?}"))?;
        Ok(SignInFlow::Here {
            connector,
            secrets: SourceSecrets::new(&paths.home, source),
        })
    }

    fn step(&self, step: Option<&str>, values: &SignInValues) -> Result<SignInStatus> {
        match self {
            SignInFlow::Service(client, source) => {
                Ok(client.sources_sign_in(source, step, values)?)
            }
            SignInFlow::Here { connector, secrets } => Ok(sources::sign_in_step(
                connector.as_ref(),
                secrets,
                step,
                values,
            )?),
        }
    }

    fn sign_out(&self) -> Result<bool> {
        match self {
            SignInFlow::Service(client, source) => Ok(client.sources_sign_out(source)?),
            SignInFlow::Here { connector, secrets } => match connector.sign_in() {
                Some(flow) => Ok(flow.sign_out(secrets)?),
                None => Ok(false),
            },
        }
    }
}

/// Goes through the steps of signing in from `status`, asking in the terminal.
fn sign_in(flow: &SignInFlow<'_>, source: &str, mut status: SignInStatus) -> Result<()> {
    let mut tries = 0;
    while let Some(step) = status.step.clone() {
        println!("{}", step.title);
        if let Some(message) = &step.message {
            println!("{message}");
        }
        if let Some(url) = &step.browser_url {
            println!("Open this page in your browser to go on:\n\n  {url}\n");
            open_in_browser(url);
        }
        let mut values = SignInValues::default();
        for field in &step.fields {
            let label = match field.placeholder.as_str() {
                "" => format!("{}: ", field.label),
                example => format!("{} (such as {example}): ", field.label),
            };
            values = values.with(field.id.clone(), ask(&label, field.secret)?);
        }
        if step.fields.is_empty() {
            ask(
                &format!(
                    "Press Return once you have signed in ({}): ",
                    step.continue_title.as_deref().unwrap_or("Continue")
                ),
                false,
            )?;
        }
        match flow.step(Some(&step.id), &values) {
            Ok(next) => status = next,
            Err(error) => {
                tries += 1;
                if tries >= 5 {
                    return Err(error);
                }
                eprintln!("{error}");
                // The same step is asked again (or a new one, if the flow starts over).
            }
        }
    }
    match status.account {
        Some(account) => println!("Signed {source} in to {account}."),
        None => println!("Signed {source} in."),
    }
    Ok(())
}

/// Asks for one line; `secret` ones are not shown as they are typed.
fn ask(label: &str, secret: bool) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    if terminal {
        eprint!("{label}");
        std::io::stderr().flush()?;
    }
    let hide = secret && terminal;
    if hide {
        let _ = std::process::Command::new("stty").arg("-echo").status();
    }
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if hide {
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
    }
    if read? == 0 {
        return Err(anyhow!("signing in was stopped"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Opens `url` in the browser of this machine, when there is one to open.
fn open_in_browser(url: &str) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn set_on(
    client: Option<&knode_service::Client>,
    space: &Path,
    source: &str,
    on: bool,
    by: &Provenance,
) -> Result<()> {
    match client {
        Some(client) => {
            client.sources_set_on(space, source, on, by)?;
        }
        None => {
            sources::set_on(&mut Space::open(space)?, source, on, by)?;
        }
    }
    Ok(())
}

fn find(list: Vec<SourceStatus>, id: &str) -> Result<SourceStatus> {
    list.into_iter()
        .find(|s| s.info.id == id)
        .ok_or_else(|| anyhow!("no source is named {id:?}"))
}

fn print_list(list: &[SourceStatus]) {
    let connectors: Vec<&SourceStatus> = list
        .iter()
        .filter(|s| s.info.kind == SourceKind::Connector)
        .collect();
    let importers: Vec<&SourceStatus> = list
        .iter()
        .filter(|s| s.info.kind == SourceKind::Importer)
        .collect();
    println!("Connectors");
    if connectors.is_empty() {
        println!("  none in this version of Knode yet");
    }
    let now = Timestamp::now();
    for s in connectors {
        println!(
            "  {:<14} {:<16} {}",
            s.info.id,
            state(s.state),
            s.info.reads
        );
        if let Some(setting) = &s.setting {
            let mut line = format!(
                "  {:<14} history {}, {} items",
                "",
                setting.history.id(),
                s.items
            );
            if let Some(run) = s.last_run {
                line.push_str(&format!(", last run {}", ago(now, run)));
            }
            println!("{line}");
            if !setting.exclude.is_empty() {
                let names: Vec<&str> = setting.exclude.iter().map(|c| c.name.as_str()).collect();
                println!("  {:<14} excluded: {}", "", names.join(", "));
            }
        }
        if let Some(error) = &s.last_error {
            println!("  {:<14} {}", "", error.message);
        }
    }
    println!();
    println!("Importers (knode import --from <id>)");
    for s in importers {
        println!("  {:<14} {}", s.info.id, s.info.reads);
    }
}

fn print_containers(list: &[ContainerInfo]) {
    if list.is_empty() {
        println!("This connector lists nothing to exclude.");
    }
    for c in list {
        let mark = if c.excluded { "excluded" } else { "" };
        println!("  {:<9} {:<32} {}", mark, c.container.id, c.container.name);
    }
}

fn state(state: SourceState) -> &'static str {
    match state {
        SourceState::Importer => "importer",
        SourceState::Off => "off",
        SourceState::Paused => "paused",
        SourceState::Unavailable => "not on this machine",
        SourceState::Stopped => "stopped",
        SourceState::Running => "running",
        SourceState::Idle => "on",
        SourceState::Failing => "failing",
        SourceState::NeedsUpdate => "needs an update",
        SourceState::NeedsAttention => "needs you",
    }
}

/// How long ago `then` was, for people.
fn ago(now: Timestamp, then: Timestamp) -> String {
    let secs = (now.0 - then.0).max(0) / 1000;
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_ago() {
        let now = Timestamp(10_000_000);
        assert_eq!(ago(now, Timestamp(9_990_000)), "just now");
        assert_eq!(ago(now, Timestamp(10_000_000 - 120_000)), "2 min ago");
        assert_eq!(ago(now, Timestamp(10_000_000 - 7_200_000)), "2 h ago");
    }
}

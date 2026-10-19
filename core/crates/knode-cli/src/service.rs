//! `knode service`: run, start, stop and inspect Knode Service, and start it at login.
//!
//! Commands that read or write a space go through the service, starting it in the background
//! when it is not running. Only when it cannot run do they open the space themselves.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use knode_config::Paths;
use knode_mcp::{Backend, DirectBackend, ServiceBackend};
use knode_service::{Client, LoginKind, LoginService};

/// How long to wait for the service to start or stop.
const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Run Knode Service in the foreground until Ctrl-C.
    Run,
    /// Start Knode Service in the background if it is not running.
    Start,
    /// Stop Knode Service.
    Stop,
    /// Show whether Knode Service is running, and what it is doing.
    Status,
    /// Start Knode Service at login, and now.
    Install,
    /// Stop starting Knode Service at login, and stop it.
    Uninstall,
}

pub fn run(paths: &Paths, action: Action) -> Result<()> {
    match action {
        Action::Run => foreground(),
        Action::Start => start(paths),
        Action::Stop => stop(paths),
        Action::Status => status(paths),
        Action::Install => install(paths),
        Action::Uninstall => uninstall(paths),
    }
}

/// The program that runs the service: this one, through the stable link on PATH when there is
/// one.
fn exe() -> Result<PathBuf> {
    crate::connect::knode_path()
}

/// Connects to Knode Service, starting it in the background when it is not running.
pub fn connect() -> knode_service::Result<Client> {
    let exe = std::env::current_exe()?;
    knode_service::connect_or_spawn(exe)
}

/// The backend commands and the MCP server use for the space at `path`: Knode Service, or the
/// space opened directly, with a warning, when the service cannot run.
pub fn backend(path: &Path) -> Result<Box<dyn Backend>> {
    let exe = std::env::current_exe().context("cannot find the knode program")?;
    let connect = move || knode_service::connect_or_spawn(&exe);
    Ok(match ServiceBackend::new(path, connect) {
        Ok(backend) => Box::new(backend),
        Err(error) => {
            warn_direct(&error);
            Box::new(DirectBackend::new(path))
        }
    })
}

/// Tells the person that the space is opened directly because the service cannot run.
pub fn warn_direct(error: &knode_service::Error) {
    eprintln!(
        "warning: Knode Service cannot run ({error}); opening the space directly. \
         See `knode service status`."
    );
}

fn foreground() -> Result<()> {
    let config = knode_service::Config::from_env()?;
    match knode_service::run(config) {
        Ok(()) => Ok(()),
        // Not an error: the service the person wants is running. launchd and systemd do not
        // start it again for this.
        Err(knode_service::Error::AlreadyRunning { pid }) => {
            let pid = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
            eprintln!("Knode Service is already running{pid}.");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn start(paths: &Paths) -> Result<()> {
    let running = Client::connect(paths.service_socket()).is_ok();
    let client = connect().context("Knode Service could not start")?;
    let pid = client.status()?.pid;
    if running {
        println!("Knode Service is already running (pid {pid}).");
    } else {
        println!("Started Knode Service (pid {pid}).");
    }
    Ok(())
}

fn stop(paths: &Paths) -> Result<()> {
    match knode_service::stop_service(paths.service_socket(), WAIT)? {
        Some(pid) => {
            println!("Stopped Knode Service (pid {pid}).");
            if login()?.is_installed() {
                println!(
                    "It starts again at the next login; `knode service uninstall` stops that."
                );
            }
        }
        None => println!("Knode Service is not running."),
    }
    Ok(())
}

fn login() -> Result<LoginService> {
    Ok(LoginService::for_this_system()?)
}

fn status(paths: &Paths) -> Result<()> {
    match Client::connect(paths.service_socket()) {
        Ok(client) => {
            let status = client.status()?;
            println!(
                "Knode Service {} is running (pid {}).",
                status.version, status.pid
            );
            println!("Socket: {}", paths.service_socket().display());
            if status.spaces.is_empty() {
                println!("Spaces: none open");
            } else {
                println!("Spaces:");
                for space in &status.spaces {
                    println!("  {}", space.display());
                }
            }
            // This command is one of the clients.
            println!("Clients: {}", status.clients.saturating_sub(1));
            match &status.capture {
                Some(space) => println!("Capture: on, into {}", space.display()),
                None => println!("Capture: off"),
            }
        }
        Err(_) => println!("Knode Service is not running."),
    }
    let login = login()?;
    let what = match login.kind() {
        LoginKind::LaunchAgent => "launch agent",
        LoginKind::SystemdUnit => "systemd user unit",
    };
    if login.is_installed() {
        println!("Starts at login: yes, {what} {}", login.path().display());
    } else {
        println!("Starts at login: no (`knode service install` to start it at login)");
    }
    Ok(())
}

fn install(paths: &Paths) -> Result<()> {
    // A service started by hand would keep the login one from running: stop it first.
    knode_service::stop_service(paths.service_socket(), WAIT)?;
    let login = login()?;
    let log = paths.home.join(knode_service::LOG_FILE);
    std::fs::create_dir_all(&paths.home)
        .with_context(|| format!("cannot create {}", paths.home.display()))?;
    let file = login.install(exe()?, Some(&log))?;
    println!(
        "Knode Service now starts at login, from {}.",
        file.display()
    );
    let started = Instant::now();
    loop {
        if let Ok(client) = Client::connect(paths.service_socket()) {
            println!("It is running (pid {}).", client.status()?.pid);
            return Ok(());
        }
        if started.elapsed() >= WAIT {
            bail!(
                "Knode Service did not start within {} seconds; see {}",
                WAIT.as_secs(),
                log.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn uninstall(paths: &Paths) -> Result<()> {
    let login = login()?;
    let path = login.path();
    if login.uninstall()? {
        println!(
            "Knode Service no longer starts at login; removed {}.",
            path.display()
        );
    } else {
        println!("Knode Service was not set to start at login.");
    }
    // Unloading stops the service it started; wait for that, and stop one started by hand.
    let _ = knode_service::stop_service(paths.service_socket(), WAIT);
    let started = Instant::now();
    while paths.service_socket().exists() {
        if started.elapsed() >= WAIT {
            bail!("Knode Service did not stop; try `knode service stop`");
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

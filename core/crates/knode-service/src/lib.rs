//! Knode Service: the one process that opens spaces, for the app and the agents.
//!
//! Only Knode Service opens a `.knode` space at runtime. The app, the CLI, the MCP server and
//! capture talk to it over a Unix socket in the Knode home (`$KNODE_HOME/service.sock`), with
//! JSON-RPC 2.0, one JSON message per line. Many clients connect at once; each space is written
//! one change at a time, and every subscribed client hears about each change. The protocol is
//! described in `PROTOCOL.md`.
//!
//! - [`run`] runs the service until SIGTERM: this is `knode service run`. With
//!   [`Config::from_env`] it also captures agent conversations into the default space (see
//!   [`CaptureOptions`]).
//! - [`start`] runs it on a background thread, for tests and embedding.
//! - [`Client`] talks to it; [`connect_or_spawn`] starts it first when it is not running.
//! - [`install_launch_agent`] and [`install_systemd_unit`] start it at login.
//!
//! ```no_run
//! use knode_format::{NewNode, Provenance};
//!
//! let client = knode_service::connect_or_spawn("/opt/homebrew/bin/knode")?;
//! let space = client.open_space("/Users/me/Knode/Personal.knode")?;
//! let me = Provenance::person("Ángel");
//! client.create_node(&space.path, NewNode::note("Plan", "Ship v0.1."), &me)?;
//! # Ok::<(), knode_service::Error>(())
//! ```

#![warn(missing_docs)]
#![cfg(unix)]

mod capture;
mod client;
mod error;
mod install;
mod lock;
mod paths;
pub mod protocol;
mod server;

pub use capture::{CAPTURE_DIR, CAPTURE_STATE_FILE, CaptureOptions};
pub use client::{Client, SPAWN_TIMEOUT, Subscription, connect_or_spawn, connect_or_spawn_with};
pub use error::{Error, Result};
pub use install::{
    LAUNCH_AGENT_LABEL, SYSTEMD_UNIT, install_launch_agent, install_launch_agent_in,
    install_systemd_unit, install_systemd_unit_in, launch_agent_path, launch_agent_plist,
    systemd_unit, systemd_unit_path, uninstall_launch_agent, uninstall_launch_agent_in,
    uninstall_systemd_unit, uninstall_systemd_unit_in,
};
pub use knode_ingest::{Agent, WatchOptions};
pub use paths::{LOCK_FILE, LOG_FILE, SOCKET_FILE, knode_home, socket_path};
pub use protocol::{Change, ChangeKind, Event, RpcError, SpaceInfo, Status};
pub use server::{Config, ServiceHandle, run, start};

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

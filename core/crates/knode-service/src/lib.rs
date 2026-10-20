//! Knode Service: the one process that opens spaces, for the app and the agents.
//!
//! Only Knode Service opens a `.knode` space at runtime. The app, the CLI, the MCP server and
//! capture talk to it over a Unix socket in the Knode home (`$KNODE_HOME/service.sock`), with
//! JSON-RPC 2.0, one JSON message per line. Many clients connect at once; each space is written
//! one change at a time, and every subscribed client hears about each change. The protocol is
//! described in `PROTOCOL.md`.
//!
//! - [`run`] runs the service until SIGTERM: this is `knode service run`. With
//!   [`Config::from_env`] it also captures agent conversations into the spaces whose folders
//!   they belong to, or the default space (see [`CaptureOptions`]).
//! - [`spaces`] holds the rules for the name, settings and folders of a space.
//! - [`search`] searches by words, by meaning or both, and indexes the open spaces in the
//!   background for search by meaning (the `semantic` feature, on by default).
//! - [`import`] tells what is at a path or URL (a data export, Markdown, a session, a PDF, a
//!   web page, Kindle or Readwise highlights, or the notes of Apple Notes, Bear, Notion or
//!   Evernote) and imports it: `import.run`.
//! - [`start`] runs it on a background thread, for tests and embedding.
//! - [`Client`] talks to it; [`connect_or_spawn`] starts it first when it is not running.
//! - [`LoginService`] starts it at login with launchd or systemd; [`stop_service`] stops it.
//! - Every connection authenticates: [`Client::connect`] as the person who owns the Knode
//!   home, [`Client::connect_as_agent`] as an agent with its key. Agents are limited by the
//!   grants of each space; [`keys`] makes keys and keeps them in the machine's credential store.
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

pub mod agents;
mod auth;
mod capture;
mod client;
mod coedit;
mod error;
pub mod import;
#[cfg(semantic)]
mod index;
mod install;
pub mod keys;
mod lock;
pub mod maps;
pub mod markdown;
mod paths;
pub mod protocol;
pub mod readwise;
pub mod search;
mod server;
pub mod spaces;

pub use agents::{AgentInfo, Identity, KeyInfo, SpaceGrant};
pub use auth::{ACCESS_LOG_FILE, AGENTS_FILE};
pub use capture::{CAPTURE_DIR, CAPTURE_STATE_FILE, CaptureOptions};
pub use client::{
    Client, Credential, SPAWN_TIMEOUT, Subscription, connect_or_spawn, connect_or_spawn_as,
    connect_or_spawn_with, stop_service,
};
pub use error::{Error, Result};
pub use import::{ImportOutcome, ImportResult, ImportSource, ImportedNode};
pub use install::{
    LAUNCH_AGENT_LABEL, LoginKind, LoginService, SYSTEMD_UNIT, launch_agent_path,
    launch_agent_plist, systemd_unit, systemd_unit_path,
};
pub use knode_ingest::{Agent, WatchOptions};
pub use maps::{
    AttachmentContent, ConversationImport, MAX_INLINE_ATTACHMENT, PositionInput, SkippedItem,
};
pub use paths::{
    LOCK_FILE, LOG_FILE, SOCKET_FILE, SOCKET_PATH_MAX, check_socket_path, knode_home, socket_path,
};
pub use protocol::{
    Change, ChangeKind, DeletedNote, DocEdited, DocSplice, DocText, Event, MarkdownFolder,
    RpcError, SpaceInfo, Status,
};
pub use search::{IndexOptions, IndexStatus, ModelState, ModelStatus, SearchResults, SpaceIndex};
pub use server::{Config, ServiceHandle, run, start};
pub use spaces::{FolderInfo, FolderInput, SpaceEvent, SpaceEventKind};

/// The version of this crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

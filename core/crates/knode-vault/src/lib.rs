//! Markdown in and out of `.knode` spaces.
//!
//! Every node of a space is Markdown ([decision 0006]). This crate moves nodes between a space
//! and a folder of `.md` files:
//!
//! - [`export`] writes a space to a folder, one file per node, with YAML frontmatter;
//! - [`import`] reads a folder of `.md` files into a space: a folder written by [`export`], or
//!   an Obsidian vault;
//! - [`LiveFolder`] keeps a folder in step with a space as it changes, one way, from the space
//!   to the folder.
//!
//! ```
//! use knode_format::{NewNode, Provenance, Space};
//!
//! # let tmp = tempfile::tempdir().unwrap();
//! let me = Provenance::person("Ángel");
//! let mut space = Space::create(tmp.path().join("Notes.knode"), "Notes")?;
//! space.create_node(NewNode::note("Roadmap", "Ship v0.2."), &me)?;
//! space.create_node(NewNode::note("Plan", "Follow the [[Roadmap]]."), &me)?;
//!
//! let folder = tmp.path().join("Notes");
//! knode_vault::export(&space, &folder)?;
//! assert!(folder.join("Roadmap.md").exists());
//!
//! let mut copy = Space::create(tmp.path().join("Copy.knode"), "Copy")?;
//! let report = knode_vault::import(&mut copy, &folder, &me)?;
//! assert_eq!(report.created.len(), 2);
//! # Ok::<(), knode_vault::Error>(())
//! ```
//!
//! # Files
//!
//! A node is written to `<Title>.md`, at the root of the folder. A title is made safe for every
//! file system (`/ \ : * ? " < > |` become `-`); two nodes with the same title are written to
//! `Plan.md` and `Plan 2.md`, in the order of their ids. A node imported from a file keeps that
//! file's path, folders included, as long as its title still fits the file name. The folder keeps
//! the path of each node in `.knode/files.json`, so names stay the same from one export to the
//! next. Knode only ever overwrites or removes a file whose frontmatter has the id of the node it
//! writes: files it did not write are never touched.
//!
//! A node written by Knode looks like this:
//!
//! ```markdown
//! ---
//! id: 01JB7Q9W8K3M2N4P5R6S7T8V9X
//! kind: decision
//! tags: [format, v0]
//! created: 2026-10-20T09:30:00.000Z
//! created_by:
//!   kind: person
//!   actor: Ada
//! updated: 2026-10-20T10:02:13.511Z
//! updated_by:
//!   kind: agent
//!   actor: claude-code
//!   session: 8f14e45f
//! links:
//!   derived-from: [01JB7Q2A0B1C2D3E4F5G6H7J8K]
//! properties:
//!   conversation.agent: claude-code
//! ---
//! The body, as stored, with [[links]] by current title.
//! ```
//!
//! `title` is written only when the file name does not read back as the title. `created_by` and
//! `updated_by` are the provenance of the node, with `session` and `source` when it has them;
//! `updated_by` is left out when it is the same as `created_by`. `links` holds the typed links
//! made by hand (every kind but `mentions`, which come from the body).
//!
//! Importing the folder into another space keeps the id, dates and provenance of every node, so
//! exporting the copy gives the same files.
//!
//! # Notes from other apps
//!
//! A file whose frontmatter Knode did not write, such as a note of an Obsidian vault, keeps its
//! frontmatter as it is: on export Knode only adds `id` as its first line (and `title`, `kind`,
//! `links` or `properties` right after it, when they are not the defaults). Its tags are those of
//! its frontmatter and its inline `#tags`; its folder is kept in the property
//! [`property_keys::PATH`].
//!
//! [decision 0006]: https://github.com/knodemd/handbook/blob/main/decisions/0006-markdown.md

#![warn(missing_docs)]

mod frontmatter;
mod import;
mod live;
mod mirror;
mod names;
mod tags;
mod time;

use std::path::PathBuf;

pub use import::{ImportReport, Skipped, import};
pub use live::{LiveFolder, cloud_folder_warning};
pub use mirror::{ExportReport, export};

/// The folder, inside an exported folder, where Knode keeps its own files.
pub const STATE_DIR: &str = ".knode";
/// The file, inside [`STATE_DIR`], that maps node ids to file paths.
pub const FILES_MAP: &str = "files.json";

/// The node properties this crate keeps. They are never written to the `properties` of a file:
/// each one stands for something else of the file.
pub mod property_keys {
    /// The path of the file the node was last imported from, relative to the folder, with `/`
    /// between folders: `Projects/Plan.md`. Export writes the node to the same path while its
    /// title fits the file name.
    pub const PATH: &str = "markdown.path";
    /// `vault` for a node imported from a file whose frontmatter Knode did not write. Such a
    /// node is exported with that frontmatter, as it was.
    pub const STYLE: &str = "markdown.style";
    /// The frontmatter entries of the file Knode does not read, exactly as written.
    pub const FRONTMATTER: &str = "markdown.frontmatter";
    /// The `id` of the file, when it is not the id of the node: the node was imported from
    /// another space by an earlier version of Knode, which could not keep ids. Export writes it
    /// as the id, so the same note keeps one id in every copy. Read, no longer written: imports
    /// now keep the id of the file.
    pub const ID: &str = "markdown.id";
    /// The `created` of the file, for a node with [`ID`]. Read, no longer written.
    pub const CREATED: &str = "markdown.created";
    /// The `updated` of the file, for a node with [`ID`]. Read, no longer written.
    pub const UPDATED: &str = "markdown.updated";
    /// When the node was written from that file: while the node's `updated_at` is still this,
    /// export writes [`UPDATED`] as `updated`. Read, no longer written.
    pub const WRITTEN: &str = "markdown.written";

    /// The prefix of every key above.
    pub const PREFIX: &str = "markdown.";
}

/// The value of [`property_keys::STYLE`] for notes from other apps.
pub(crate) const STYLE_VAULT: &str = "vault";

/// Everything that can go wrong moving Markdown in or out of a space.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The space returned an error.
    #[error(transparent)]
    Space(#[from] knode_format::Error),
    /// A file or folder could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The folder's `.knode/files.json` could not be read.
    #[error("{}: {message}", path.display())]
    Map {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
}

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}

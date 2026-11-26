//! The spaces an agent reaches in a session: every space it may read, and the one it saves to.
//!
//! Decision 0017 of the handbook: an agent reads wide and writes narrow. It searches every space
//! it was given `read` or `write` in ([`Backend::reachable`]), and saves to one: the space given
//! with `--space` or `KNODE_SPACE`; else the space of the project folder nearest to the folder
//! the agent works in, among those it may write to, by the rule capture routes conversations
//! with (section 15.5 of the .knode specification); else the default space. A space where its
//! level is `none` is never listed, so it stays invisible.

use std::path::{Path, PathBuf};

use knode_format::{Folder, Node, NodeId, NodeSummary};

use crate::backend::{Backend, BackendResult, Reachable};

/// One space of a [`Reach`].
#[derive(Clone, Copy)]
pub struct Reached<'a> {
    /// Its name, when known: results name the space they come from.
    pub name: Option<&'a str>,
    /// Where it is, when known.
    pub path: Option<&'a Path>,
    /// Whether the agent may read it. The space the agent was started with is kept to save to
    /// even when it may not read it, so it says so when asked.
    pub readable: bool,
    /// Whether the agent may write to it.
    pub writable: bool,
    /// Its folders, to find the space of the working folder.
    folders: &'a [Folder],
    /// The backend that reads and writes it.
    pub backend: &'a dyn Backend,
}

impl std::fmt::Debug for Reached<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reached")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("readable", &self.readable)
            .field("writable", &self.writable)
            .finish_non_exhaustive()
    }
}

/// Why the agent saves to the space it saves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    /// It was given with `--space` or `KNODE_SPACE`.
    Given,
    /// A project folder of the space holds the folder the agent works in.
    Folder,
    /// No project folder of a space it may write to holds that folder: the default space.
    Default,
}

/// The spaces an agent reaches: those it reads, its own first, and the one it saves to.
#[derive(Debug)]
pub struct Reach<'a> {
    spaces: Vec<Reached<'a>>,
    home: usize,
    why: Home,
}

impl<'a> Reach<'a> {
    /// One space, read and written: a space opened directly, or a service that cannot list the
    /// agent's spaces.
    pub fn single(backend: &'a dyn Backend) -> Self {
        Reach {
            spaces: vec![Reached {
                name: None,
                path: None,
                readable: true,
                writable: true,
                folders: &[],
                backend,
            }],
            home: 0,
            why: Home::Default,
        }
    }

    /// The spaces of an agent started on `own` (the default space, or the one in `--space`
    /// when `given`), which may read `reachable` (from [`Backend::reachable`]), working in
    /// `folder`.
    pub fn new(
        own: &'a dyn Backend,
        reachable: &'a [Reachable],
        given: bool,
        folder: Option<&Path>,
    ) -> Self {
        let own_key = own.space_path().map(|p| same_key(&p));
        let own_at = reachable
            .iter()
            .position(|r| own_key.as_ref() == Some(&same_key(&r.path)));
        // No other space, or a space given with --space that the agent may not see: that space
        // alone, so every tool says it cannot be opened rather than answering from others.
        if reachable.is_empty() || (given && own_at.is_none()) {
            let mut reach = Reach::single(own);
            if given {
                reach.why = Home::Given;
            }
            return reach;
        }
        let mut spaces = Vec::with_capacity(reachable.len() + 1);
        spaces.push(match own_at {
            Some(i) => Reached {
                name: Some(&reachable[i].name),
                path: Some(&reachable[i].path),
                readable: true,
                writable: reachable[i].writable,
                folders: &reachable[i].folders,
                backend: own,
            },
            // The agent may not read the space it was started on: it is kept only to save to,
            // so a write says what is wrong, as before.
            None => Reached {
                name: None,
                path: None,
                readable: false,
                writable: true,
                folders: &[],
                backend: own,
            },
        });
        for (i, r) in reachable.iter().enumerate() {
            if Some(i) != own_at {
                spaces.push(Reached {
                    name: Some(&r.name),
                    path: Some(&r.path),
                    readable: true,
                    writable: r.writable,
                    folders: &r.folders,
                    backend: r.backend.as_ref(),
                });
            }
        }
        let mut reach = Reach {
            spaces,
            home: 0,
            why: if given { Home::Given } else { Home::Default },
        };
        if !given && let Some(at) = folder.and_then(|folder| reach.working_space(folder)) {
            reach.home = at;
            reach.why = Home::Folder;
        }
        reach
    }

    /// The index of the space whose project folder is nearest to `folder`, among those the
    /// agent may write to, the space it was started on first.
    fn working_space(&self, folder: &Path) -> Option<usize> {
        let candidates: Vec<(usize, PathBuf)> = self
            .spaces
            .iter()
            .enumerate()
            .filter(|(_, s)| s.readable && s.writable)
            .filter_map(|(i, s)| Some((i, s.path?.to_path_buf())))
            .collect();
        let path = nearest_space(
            candidates
                .iter()
                .map(|(i, path)| (path.clone(), self.spaces[*i].folders.to_vec())),
            folder,
        )?;
        candidates.iter().find(|(_, p)| *p == path).map(|(i, _)| *i)
    }

    /// The space the agent saves to.
    pub fn home(&self) -> &Reached<'a> {
        &self.spaces[self.home]
    }

    /// Why it saves there.
    pub fn why(&self) -> Home {
        self.why
    }

    /// The spaces the agent reads, the one it saves to first.
    pub fn readable(&self) -> impl Iterator<Item = &Reached<'a>> {
        std::iter::once(&self.spaces[self.home])
            .chain(
                self.spaces
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != self.home)
                    .map(|(_, s)| s),
            )
            .filter(|s| s.readable)
    }

    /// Whether the agent reads more than one space, so results say which each comes from.
    pub fn is_several(&self) -> bool {
        self.readable().count() > 1
    }

    /// The space that has the node `id`, with the node, the one it saves to first. A space
    /// that cannot answer is passed over; only the error of the space it saves to is returned,
    /// when no other has the node.
    pub fn find(&self, id: &NodeId) -> BackendResult<Option<(&Reached<'a>, Node)>> {
        self.first(|backend| backend.get_node(id))
    }

    /// The first space with a node of this title, the one it saves to first, with the nodes
    /// of that title there.
    pub fn find_title(
        &self,
        title: &str,
    ) -> BackendResult<Option<(&Reached<'a>, Vec<NodeSummary>)>> {
        self.first(|backend| {
            backend
                .find_by_title(title)
                .map(|found| Some(found).filter(|f| !f.is_empty()))
        })
    }

    fn first<T>(
        &self,
        ask: impl Fn(&dyn Backend) -> BackendResult<Option<T>>,
    ) -> BackendResult<Option<(&Reached<'a>, T)>> {
        let home = self.home();
        let mut home_error = None;
        for space in self.readable() {
            match ask(space.backend) {
                Ok(Some(found)) => return Ok(Some((space, found))),
                Ok(None) => {}
                Err(e) if std::ptr::eq(space, home) => home_error = Some(e),
                Err(_) => {}
            }
        }
        home_error.map_or(Ok(None), Err)
    }

    /// The space a node reference (an id, or an exact title) belongs to: the first that has
    /// it, else the one the agent saves to, so a missing node is reported as before.
    pub fn space_of(&self, reference: &str) -> &Reached<'a> {
        let reference = reference.trim();
        if !self.is_several() || reference.is_empty() {
            return self.home();
        }
        let found = match NodeId::parse(reference) {
            Ok(id) => self.find(&id).ok().flatten().map(|(s, _)| s),
            Err(_) => self.find_title(reference).ok().flatten().map(|(s, _)| s),
        };
        found.unwrap_or_else(|| self.home())
    }

    /// A sentence for the agent's instructions: which spaces it reads and where it saves, when
    /// it reads more than one.
    pub fn describe(&self) -> Option<String> {
        if !self.is_several() {
            return None;
        }
        let names: Vec<String> = self
            .readable()
            .map(|s| format!("\"{}\"", s.name.unwrap_or("?")))
            .collect();
        let home = self
            .home()
            .name
            .unwrap_or("the space you were started with");
        let why = match self.why {
            Home::Given => "the space Knode was started with",
            Home::Folder => "the space of the project folder you work in",
            Home::Default => {
                "the default space; no project folder of a space you may write to holds the folder you work in"
            }
        };
        Some(format!(
            "You read {} spaces: {}. search, projects and recent cover all of them and say which \
             space each result comes from. write_node and link_nodes save to \"{home}\" only \
             ({why}).",
            names.len(),
            names.join(", ")
        ))
    }
}

/// The key two paths of one space compare by: the path with links resolved, as Knode Service
/// opens spaces.
fn same_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The space, of `spaces` (each with its folders, in order of preference), whose project folder
/// is nearest to `folder`: the rule of section 15.5, shared with capture.
#[cfg(unix)]
fn nearest_space(
    spaces: impl Iterator<Item = (PathBuf, Vec<Folder>)>,
    folder: &Path,
) -> Option<PathBuf> {
    let spaces: Vec<knode_service::spaces::SpaceFolders> = spaces
        .map(|(space, folders)| knode_service::spaces::SpaceFolders {
            space,
            name: String::new(),
            folders,
            capture: true,
        })
        .collect();
    knode_service::spaces::working_space(&spaces, folder).map(|s| s.space.clone())
}

/// Without Knode Service, an agent reads and writes one space: there is nothing to choose.
#[cfg(not(unix))]
fn nearest_space(
    _spaces: impl Iterator<Item = (PathBuf, Vec<Folder>)>,
    _folder: &Path,
) -> Option<PathBuf> {
    None
}

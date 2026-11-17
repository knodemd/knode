//! The files of a chat export: a `.zip` as downloaded, the folder it was extracted to, or one
//! chat file and the files beside it.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use crate::exports::source::Source;

/// The files of an export, by their path inside it with `/` between folders.
pub(crate) struct Files {
    source: Source,
    names: BTreeSet<String>,
    /// The file the person gave, when it is one chat file rather than a whole export.
    pub single: Option<String>,
}

impl Files {
    /// Opens a folder, a `.zip`, or one file (a `.txt` or `.json`) with the files beside it
    /// (not the folders beside it).
    pub fn open(path: &Path) -> io::Result<Files> {
        if path.is_dir() {
            let source = Source::open(path)?;
            let names = source.names()?.into_iter().collect();
            return Ok(Files {
                source,
                names,
                single: None,
            });
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("txt" | "json")) {
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut names = BTreeSet::new();
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.') && entry.file_type()?.is_file() {
                    names.insert(name);
                }
            }
            let single = path.file_name().map(|n| n.to_string_lossy().into_owned());
            return Ok(Files {
                source: Source::open(&dir)?,
                names,
                single,
            });
        }
        let source = Source::open(path)?;
        let names = source.names()?.into_iter().collect();
        Ok(Files {
            source,
            names,
            single: None,
        })
    }

    /// Every file, sorted.
    pub fn names(&self) -> &BTreeSet<String> {
        &self.names
    }

    /// Whether the export has this file.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// A reader over one file.
    pub fn open_file(&mut self, name: &str) -> io::Result<Box<dyn Read + '_>> {
        self.source.open_file(name)
    }

    /// One whole file, as text (invalid UTF-8 replaced).
    pub fn read_to_string(&mut self, name: &str) -> io::Result<String> {
        self.source.read_to_string(name)
    }

    /// The first `max` bytes of one file.
    pub fn peek(&mut self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.source.peek(name, max)
    }
}

/// The file name of a path inside an export.
pub(crate) fn file_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// The folder of a path inside an export, empty at the top.
pub(crate) fn parent(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `dir/name`, or `name` at the top.
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The name of what the person gave, without its extension: the `.zip` or the folder.
pub(crate) fn stem(path: &Path) -> String {
    let name = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    };
    name.map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

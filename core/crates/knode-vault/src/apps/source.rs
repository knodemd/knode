//! The files of an export: a folder, a `.zip` (with the `.zip` files inside it, as Notion splits
//! large exports), or one file.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

type Archive = zip::ZipArchive<Box<dyn ReadSeek>>;

enum Root {
    Dir(PathBuf),
    File(PathBuf),
    Zip(Vec<Archive>),
}

/// The files of an export, by their path inside it with `/` between folders, sorted. Hidden
/// files and folders (`.DS_Store`, `__MACOSX/`) are left out.
pub(crate) struct Source {
    root: Root,
    names: Vec<String>,
    /// For a zip: the archive and the entry of each name.
    entries: HashMap<String, (usize, usize)>,
}

impl Source {
    /// Opens a folder, a `.zip` file, or any other file as the only file.
    pub(crate) fn open(path: &Path) -> io::Result<Source> {
        if path.is_dir() {
            let mut names = Vec::new();
            walk(path, "", &mut names)?;
            names.sort();
            return Ok(Source {
                root: Root::Dir(path.to_path_buf()),
                names,
                entries: HashMap::new(),
            });
        }
        let mut file = File::open(path)?;
        let mut magic = [0u8; 4];
        let is_zip = file.read(&mut magic)? == 4 && &magic == b"PK\x03\x04";
        if !is_zip {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Ok(Source {
                root: Root::File(path.to_path_buf()),
                names: vec![name],
                entries: HashMap::new(),
            });
        }
        let outer: Box<dyn ReadSeek> = Box::new(BufReader::new(File::open(path)?));
        let mut archives = vec![zip::ZipArchive::new(outer).map_err(zip_error)?];
        let mut entries = HashMap::new();
        let mut inner = Vec::new();
        list(&mut archives[0], 0, &mut entries, &mut inner);
        // One level of zips inside the zip.
        for index in inner {
            let mut bytes = Vec::new();
            archives[0]
                .by_index(index)
                .map_err(zip_error)?
                .read_to_end(&mut bytes)?;
            let reader: Box<dyn ReadSeek> = Box::new(Cursor::new(bytes));
            let Ok(mut archive) = zip::ZipArchive::new(reader) else {
                continue;
            };
            let n = archives.len();
            list(&mut archive, n, &mut entries, &mut Vec::new());
            archives.push(archive);
        }
        let mut names: Vec<String> = entries.keys().cloned().collect();
        names.sort();
        Ok(Source {
            root: Root::Zip(archives),
            names,
            entries,
        })
    }

    /// Every file, sorted.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// Whether the export has this file.
    pub(crate) fn has(&self, name: &str) -> bool {
        match &self.root {
            Root::Zip(_) => self.entries.contains_key(name),
            _ => self
                .names
                .binary_search_by(|n| n.as_str().cmp(name))
                .is_ok(),
        }
    }

    /// One whole file.
    pub(crate) fn read(&mut self, name: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        match &mut self.root {
            Root::Dir(root) => {
                let mut path = root.clone();
                path.extend(name.split('/'));
                return fs::read(path);
            }
            Root::File(path) => return fs::read(path),
            Root::Zip(archives) => {
                let (archive, index) = *self
                    .entries
                    .get(name)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, name.to_string()))?;
                archives[archive]
                    .by_index(index)
                    .map_err(zip_error)?
                    .read_to_end(&mut bytes)?;
            }
        }
        Ok(bytes)
    }

    /// One whole file as text, invalid UTF-8 replaced.
    pub(crate) fn read_string(&mut self, name: &str) -> io::Result<String> {
        let bytes = self.read(name)?;
        let text = String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        Ok(text
            .strip_prefix('\u{feff}')
            .map(str::to_string)
            .unwrap_or(text))
    }

    /// The file on disk for a name, for a folder or a single file.
    pub(crate) fn disk_path(&self, name: &str) -> Option<PathBuf> {
        match &self.root {
            Root::Dir(root) => {
                let mut path = root.clone();
                path.extend(name.split('/'));
                Some(path)
            }
            Root::File(path) => Some(path.clone()),
            Root::Zip(_) => None,
        }
    }
}

/// Whether a path inside an export is hidden or macOS metadata.
fn hidden(name: &str) -> bool {
    name.split('/')
        .any(|part| part.starts_with('.') || part == "__MACOSX")
}

fn list(
    archive: &mut Archive,
    n: usize,
    entries: &mut HashMap<String, (usize, usize)>,
    zips: &mut Vec<usize>,
) {
    for index in 0..archive.len() {
        let Some(name) = archive.name_for_index(index) else {
            continue;
        };
        let name = name.trim_start_matches("./").to_string();
        if name.ends_with('/') || name.is_empty() || hidden(&name) {
            continue;
        }
        if name.to_lowercase().ends_with(".zip") {
            zips.push(index);
            continue;
        }
        entries.entry(name).or_insert((n, index));
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            walk(&entry.path(), &rel, out)?;
        } else if meta.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// The folder of a path inside an export, `""` at the top.
pub(crate) fn parent(name: &str) -> &str {
    name.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// The last part of a path inside an export.
pub(crate) fn file_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Joins a folder and a relative path, resolving `.` and `..`; `None` when it leaves the
/// export.
pub(crate) fn resolve(dir: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = if relative.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|p| !p.is_empty()).collect()
    };
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths() {
        assert_eq!(resolve("a/b", "c.png").as_deref(), Some("a/b/c.png"));
        assert_eq!(resolve("a/b", "../c.png").as_deref(), Some("a/c.png"));
        assert_eq!(resolve("", "./c.png").as_deref(), Some("c.png"));
        assert_eq!(resolve("", "../c.png"), None);
        assert_eq!(parent("a/b/c.md"), "a/b");
        assert_eq!(file_name("a/b/c.md"), "c.md");
    }
}

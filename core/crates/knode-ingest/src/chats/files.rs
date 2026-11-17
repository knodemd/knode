//! The files of a chat export: a `.zip` as downloaded, the folder it was extracted to, or one
//! chat file and the files beside it.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use crate::exports::source::Source;

/// The most bytes read from one file of an export: a chat's text or JSON, or a file to attach
/// (which the space then limits to 100 MB). The `result.json` of a Telegram account export,
/// read one chat at a time, has this limit on each chat instead. A larger file, such as a `.zip` made to unpack to
/// far more than it holds, fails with a clear error instead of filling the memory or the disk.
pub(crate) const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;

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

    /// The folder the export's own files sit in: the top, or, when the top holds one folder
    /// and nothing else (a `.zip` of the export's folder), that folder. A folder that macOS
    /// adds to the archives it makes, `__MACOSX`, does not count.
    pub fn root(&self) -> String {
        if self.single.is_some() {
            return String::new();
        }
        let mut top = self
            .names
            .iter()
            .filter(|n| !n.starts_with("__MACOSX/"))
            .map(|n| {
                n.split_once('/')
                    .map(|(dir, _)| (dir, true))
                    .unwrap_or((n, false))
            });
        let Some((first, is_dir)) = top.next() else {
            return String::new();
        };
        if is_dir && top.all(|(dir, is_dir)| is_dir && dir == first) {
            first.to_string()
        } else {
            String::new()
        }
    }

    /// Whether the export has this file.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// A reader over one file, which fails past [`MAX_FILE_SIZE`] bytes.
    pub fn open_file(&mut self, name: &str) -> io::Result<Box<dyn Read + '_>> {
        let inner = self.source.open_file(name)?;
        Ok(Box::new(Limited {
            inner,
            left: MAX_FILE_SIZE,
            name: name.to_string(),
        }))
    }

    /// A reader over one file that fails once it has given `limit` bytes since `read` was last
    /// set back to zero, and the count it keeps: for a file read in parts, such as the chats of
    /// a Telegram account export one by one, where the limit is on each part.
    pub fn open_file_counted(
        &mut self,
        name: &str,
        limit: u64,
        read: std::rc::Rc<std::cell::Cell<u64>>,
    ) -> io::Result<Box<dyn Read + '_>> {
        let inner = self.source.open_file(name)?;
        Ok(Box::new(Counted {
            inner,
            limit,
            read,
            name: name.to_string(),
        }))
    }

    /// One whole file, as text (invalid UTF-8 replaced).
    pub fn read_to_string(&mut self, name: &str) -> io::Result<String> {
        let mut bytes = Vec::new();
        self.open_file(name)?.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The first `max` bytes of one file.
    pub fn peek(&mut self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.source.peek(name, max)
    }
}

/// A reader that fails once it has given `left` bytes and there are more.
struct Limited<R> {
    inner: R,
    left: u64,
    name: String,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} is larger than {} MB, the most Knode reads from one file of an export",
                        self.name,
                        MAX_FILE_SIZE / (1024 * 1024)
                    ),
                )),
            };
        }
        let max = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// A reader that fails once `read` passes `limit`; whoever reads sets `read` back to zero.
struct Counted<R> {
    inner: R,
    limit: u64,
    read: std::rc::Rc<std::cell::Cell<u64>>,
    name: String,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let read = self.read.get() + n as u64;
        self.read.set(read);
        if read > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a chat in {} is larger than {} MB, the most Knode reads of one chat",
                    self.name,
                    self.limit / (1024 * 1024)
                ),
            ));
        }
        Ok(n)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_reading_past_the_limit() {
        let mut ok = Limited {
            inner: &b"abc"[..],
            left: 3,
            name: "a".into(),
        };
        let mut out = Vec::new();
        ok.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc");
        let mut big = Limited {
            inner: &b"abcd"[..],
            left: 3,
            name: "_chat.txt".into(),
        };
        let error = big.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("_chat.txt is larger than 512 MB"),
            "{error}"
        );
    }
}

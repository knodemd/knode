//! Reading an export as downloaded (a `.zip`) or extracted (a folder), and streaming the JSON
//! arrays inside it one element at a time.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::de::{self, DeserializeSeed, SeqAccess, Visitor};
use serde_json::Value;

/// An export: the files of a `.zip` or of a folder, by their path inside it with `/` between
/// folders.
pub(crate) enum Source {
    Dir(PathBuf),
    Zip(Box<zip::ZipArchive<BufReader<File>>>),
}

impl Source {
    /// Opens a folder, or a file as a zip archive.
    pub(crate) fn open(path: &Path) -> io::Result<Source> {
        if path.is_dir() {
            return Ok(Source::Dir(path.to_path_buf()));
        }
        let file = File::open(path)?;
        let archive = zip::ZipArchive::new(BufReader::new(file)).map_err(zip_error)?;
        Ok(Source::Zip(Box::new(archive)))
    }

    /// Every file, sorted.
    pub(crate) fn names(&self) -> io::Result<Vec<String>> {
        let mut names = match self {
            Source::Dir(root) => {
                let mut names = Vec::new();
                walk(root, "", &mut names)?;
                names
            }
            Source::Zip(archive) => archive
                .file_names()
                .filter(|name| !name.ends_with('/'))
                .map(|name| name.trim_start_matches("./").to_string())
                .collect(),
        };
        names.sort();
        Ok(names)
    }

    /// A reader over one file.
    pub(crate) fn open_file(&mut self, name: &str) -> io::Result<Box<dyn Read + '_>> {
        match self {
            Source::Dir(root) => {
                let mut path = root.clone();
                path.extend(name.split('/'));
                Ok(Box::new(BufReader::new(File::open(path)?)))
            }
            Source::Zip(archive) => {
                let index = archive
                    .index_for_name(name)
                    .or_else(|| archive.index_for_name(&format!("./{name}")))
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, name.to_string()))?;
                let file = archive.by_index(index).map_err(zip_error)?;
                Ok(Box::new(BufReader::new(file)))
            }
        }
    }

    /// The first `max` bytes of one file.
    pub(crate) fn peek(&mut self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_file(name)?.take(max).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// One whole file.
    pub(crate) fn read_to_string(&mut self, name: &str) -> io::Result<String> {
        let mut bytes = Vec::new();
        self.open_file(name)?.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    match error {
        zip::result::ZipError::Io(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), &rel, names)?;
        } else if kind.is_file() {
            names.push(rel);
        }
    }
    Ok(())
}

/// The file name of a path inside an export.
pub(crate) fn file_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// The folder of a path inside an export, empty at the top.
pub(crate) fn parent(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Why streaming an array stopped.
#[derive(Debug)]
pub(crate) enum StreamError<E> {
    /// The file is not a JSON array.
    Json(serde_json::Error),
    /// The callback failed.
    Callback(E),
}

impl<E: fmt::Display> fmt::Display for StreamError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Json(error) => error.fmt(f),
            StreamError::Callback(error) => error.fmt(f),
        }
    }
}

/// Reads a JSON array from `reader` one element at a time, so a file of hundreds of megabytes
/// never sits in memory whole, and hands each element to `each`.
pub(crate) fn each_element<R, E, F>(reader: R, each: F) -> Result<(), StreamError<E>>
where
    R: Read,
    F: FnMut(Value) -> Result<(), E>,
{
    let mut failed = None;
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let seed = Elements {
        each,
        failed: &mut failed,
    };
    let outcome = seed.deserialize(&mut deserializer);
    if let Some(error) = failed {
        return Err(StreamError::Callback(error));
    }
    outcome.map_err(StreamError::Json)?;
    deserializer.end().map_err(StreamError::Json)
}

struct Elements<'a, E, F> {
    each: F,
    failed: &'a mut Option<E>,
}

impl<'de, E, F> DeserializeSeed<'de> for Elements<'_, E, F>
where
    F: FnMut(Value) -> Result<(), E>,
{
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, E, F> Visitor<'de> for Elements<'_, E, F>
where
    F: FnMut(Value) -> Result<(), E>,
{
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON array")
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(element) = seq.next_element::<Value>()? {
            if let Err(error) = (self.each)(element) {
                *self.failed = Some(error);
                return Err(de::Error::custom("stopped"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_array_elements() {
        let mut seen = Vec::new();
        each_element::<_, (), _>(&b"[{\"a\":1}, 2, \"x\"]"[..], |value| {
            seen.push(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0]["a"], 1);
    }

    #[test]
    fn stops_when_the_callback_fails() {
        let mut count = 0;
        let outcome = each_element(&b"[1, 2, 3]"[..], |_| {
            count += 1;
            if count == 2 { Err("no") } else { Ok(()) }
        });
        assert!(matches!(outcome, Err(StreamError::Callback("no"))));
        assert_eq!(count, 2);
    }

    #[test]
    fn rejects_what_is_not_an_array() {
        assert!(matches!(
            each_element::<_, (), _>(&b"{\"a\": 1}"[..], |_| Ok(())),
            Err(StreamError::Json(_))
        ));
    }

    #[test]
    fn splits_paths() {
        assert_eq!(file_name("a/b/c.json"), "c.json");
        assert_eq!(parent("a/b/c.json"), "a/b");
        assert_eq!(parent("c.json"), "");
    }
}

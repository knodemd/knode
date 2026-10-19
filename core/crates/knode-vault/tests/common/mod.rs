//! Helpers shared by the tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Every file under `dir`, hidden ones included, by path relative to `dir` with `/`.
pub fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out);
    out
}

fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type().unwrap().is_dir() {
            walk(&entry.path(), &rel, out);
        } else {
            out.insert(rel, fs::read(entry.path()).unwrap());
        }
    }
}

/// Writes `content` to `dir/rel`, making folders.
pub fn put(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Asserts two trees are equal, showing the first file that differs as text.
pub fn assert_same(left: &BTreeMap<String, Vec<u8>>, right: &BTreeMap<String, Vec<u8>>) {
    assert_eq!(
        left.keys().collect::<Vec<_>>(),
        right.keys().collect::<Vec<_>>(),
        "the same files"
    );
    for (path, bytes) in left {
        assert_eq!(
            String::from_utf8_lossy(bytes),
            String::from_utf8_lossy(&right[path]),
            "{path}"
        );
    }
}

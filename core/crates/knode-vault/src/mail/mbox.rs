//! `mbox` files: messages one after the other, each after a `From ` line.
//!
//! A message starts at a line beginning with `From ` at the start of the file or after an empty
//! line. Lines of a message that begin with `From ` were escaped as `>From ` (and `>From ` as
//! `>>From `, the "mboxrd" rule Google Takeout follows); one `>` is taken away again. The file is
//! read as a stream, one message at a time, so a large export does not have to fit in memory.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::message::{self, Collector, Origin};
use super::{MailError, MailReport, file_name, read_error};

/// How deep a folder is looked into for `.mbox` files.
const DEPTH: usize = 3;

/// Whether the file starts with a `From ` line.
pub(crate) fn starts_like_mbox(path: &Path) -> bool {
    let mut head = [0u8; 5];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| &head == b"From ")
}

/// A mailbox Apple Mail exported: an `.mbox` folder with an `mbox` file in it.
pub(crate) fn is_exported_mailbox(dir: &Path) -> bool {
    file_name(dir).ends_with(".mbox") && dir.join("mbox").is_file()
}

/// Whether every file and folder in `dir` (hidden ones aside) is an `.mbox` file, and there is
/// at least one.
pub(crate) fn only_mbox_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut any = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = file_name(&path);
        if name.starts_with('.') {
            continue;
        }
        if !(path.is_file() && name.ends_with(".mbox")) {
            return false;
        }
        any = true;
    }
    any
}

/// The `mbox` files at `path`, each with the folder its messages are in when they have no
/// Gmail labels: the file's name without `.mbox`.
pub(crate) fn files(path: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    if path.is_file() {
        out.push((path.to_path_buf(), folder_name(path)));
    } else if is_exported_mailbox(path) {
        out.push((path.join("mbox"), folder_name(path)));
    } else {
        collect(path, 0, &mut out);
    }
    out
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = file_name(&path);
        if name.starts_with('.') {
            continue;
        }
        if path.is_file() && name.ends_with(".mbox") {
            out.push((path.clone(), folder_name(&path)));
        } else if path.is_dir() && is_exported_mailbox(&path) {
            out.push((path.join("mbox"), folder_name(&path)));
        } else if path.is_dir() && depth < DEPTH {
            collect(&path, depth + 1, out);
        }
    }
}

fn folder_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".mbox")
        .or_else(|| name.strip_suffix(".MBOX"))
        .unwrap_or(&name)
        .to_string()
}

/// Reads every message of the `mbox` files at `path` into `collected`.
pub(crate) fn read(
    path: &Path,
    collected: &mut Collector<'_>,
    report: &mut MailReport,
) -> Result<(), MailError> {
    let files = files(path);
    if files.is_empty() {
        return Err(MailError::NotRecognized {
            path: path.to_path_buf(),
            what: "an .mbox file or a folder of them".into(),
        });
    }
    for (file, folder) in files {
        let mut any = false;
        each(&file, |offset, len, raw| {
            any = true;
            let origin = Origin::Mbox {
                file: file.clone(),
                offset,
                len,
            };
            let order = collected.next_order();
            match message::parse(&raw, origin, Some(&folder), order) {
                Some(mail) => collected.add(mail),
                None => report.skip(
                    format!("{} (message {order})", file.display()),
                    "not a mail message",
                ),
            }
        })?;
        if !any && path.is_file() {
            return Err(MailError::NotRecognized {
                path: file,
                what: "an mbox file: no line starts a message (From ...)".into(),
            });
        }
    }
    Ok(())
}

/// Calls `each` with the offset and length of every message in the file (after its `From `
/// line, still escaped) and its bytes, unescaped.
fn each(path: &Path, mut each: impl FnMut(u64, u64, Vec<u8>)) -> Result<(), MailError> {
    let file = File::open(path).map_err(read_error(path))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut line = Vec::new();
    let mut offset: u64 = 0;
    let mut current: Option<(u64, Vec<u8>)> = None;
    let mut previous_blank = true;
    loop {
        line.clear();
        let n = reader
            .read_until(b'\n', &mut line)
            .map_err(read_error(path))?;
        if n == 0 {
            break;
        }
        let start = previous_blank && line.starts_with(b"From ");
        previous_blank = line == b"\n" || line == b"\r\n";
        offset += n as u64;
        if start {
            if let Some((at, raw)) = current.take() {
                emit(at, raw, &mut each);
            }
            current = Some((offset, Vec::new()));
        } else if let Some((_, raw)) = current.as_mut() {
            raw.extend_from_slice(&line);
        }
    }
    if let Some((at, raw)) = current.take() {
        emit(at, raw, &mut each);
    }
    Ok(())
}

fn emit(at: u64, mut raw: Vec<u8>, each: &mut impl FnMut(u64, u64, Vec<u8>)) {
    // The empty line before the next `From ` line is not part of the message.
    if raw.ends_with(b"\r\n\r\n") {
        raw.truncate(raw.len() - 2);
    } else if raw.ends_with(b"\n\n") {
        raw.truncate(raw.len() - 1);
    }
    let len = raw.len() as u64;
    each(at, len, unescape(&raw));
}

/// Takes one `>` away from every line that is `>From ` after any number of `>`.
fn unescape(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let quotes = line.iter().take_while(|b| **b == b'>').count();
        if quotes > 0 && line[quotes..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// The bytes of a message [`each`] found, read again, unescaped.
pub(crate) fn read_at(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>, MailError> {
    let mut file = File::open(path).map_err(read_error(path))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(read_error(path))?;
    let mut raw = Vec::new();
    file.take(len)
        .read_to_end(&mut raw)
        .map_err(read_error(path))?;
    Ok(unescape(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_messages_and_unescapes_from_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("All mail.mbox");
        std::fs::write(
            &file,
            "From 1@xxx Mon Mar 02 10:00:00 +0000 2026\nSubject: One\n\nHello\n>From here\n\
>>From there\nFrom the middle of a paragraph\n\nFrom 2@xxx Tue Mar 03 10:00:00 +0000 2026\n\
Subject: Two\n\nBye\n",
        )
        .unwrap();
        let mut found = Vec::new();
        each(&file, |offset, len, raw| found.push((offset, len, raw))).unwrap();
        assert_eq!(found.len(), 2);
        let first = String::from_utf8(found[0].2.clone()).unwrap();
        assert_eq!(
            first,
            "Subject: One\n\nHello\nFrom here\n>From there\nFrom the middle of a paragraph\n"
        );
        assert_eq!(
            String::from_utf8(found[1].2.clone()).unwrap(),
            "Subject: Two\n\nBye\n"
        );
        let again = read_at(&file, found[0].0, found[0].1).unwrap();
        assert_eq!(again, found[0].2);
        assert!(starts_like_mbox(&file));
        assert_eq!(files(&file)[0].1, "All mail");
    }
}

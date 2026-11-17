//! The Apple Mail store: `~/Library/Mail/V<n>/<account>/<mailbox>.mbox/.../Messages/<n>.emlx`.
//!
//! An `.emlx` file is the length of the message in bytes on its first line, the message, then a
//! property list Apple Mail keeps its flags in. A `.partial.emlx` is a message whose attachments
//! Apple Mail keeps in a folder of their own; its text is read, its attachments are not. A
//! message's mailbox is the path of `.mbox` folders it is in, without `.mbox`: `INBOX`,
//! `Archive`, `[Gmail]/All Mail`. macOS lets only apps with Full Disk Access read
//! `~/Library/Mail`: a refusal is [`MailError::NoAccess`].

use std::path::{Component, Path, PathBuf};

use super::message::{self, Collector, Origin};
use super::{MailError, MailOptions, MailReport, file_name, read_error};

/// How many entries [`has_emlx`] looks at before giving up.
const LOOK_AT: usize = 2000;

/// Whether `path` is Apple Mail's `~/Library/Mail`, or a `V<n>` folder in it. Told from the
/// path alone, so that it is recognised even when macOS keeps it from being read.
pub(crate) fn is_store(path: &Path) -> bool {
    let names: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let n = names.len();
    let is_mail = |i: usize| names[i] == "Mail" && i > 0 && names[i - 1] == "Library";
    (n >= 2 && is_mail(n - 1)) || (n >= 3 && is_version(&names[n - 1]) && is_mail(n - 2))
}

/// `V2` to `V10` and on.
fn is_version(name: &str) -> bool {
    name.strip_prefix('V')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `dir` is a mailbox folder of Apple Mail's store (`INBOX.mbox`), with `.emlx` files
/// in it.
pub(crate) fn is_mailbox(dir: &Path) -> bool {
    file_name(dir).ends_with(".mbox") && has_emlx(dir)
}

/// Whether there are `.emlx` files in the folder, looking at a limited number of entries.
pub(crate) fn has_emlx(dir: &Path) -> bool {
    let mut seen = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > LOOK_AT {
                return false;
            }
            let path = entry.path();
            if file_name(&path).ends_with(".emlx") {
                return true;
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    false
}

/// Where to read from: in `~/Library/Mail`, the newest `V<n>` folder (older ones are left
/// behind when macOS moves the store to a new version).
fn root(path: &Path) -> Result<PathBuf, MailError> {
    if !path.is_dir() || file_name(path) != "mail" {
        return Ok(path.to_path_buf());
    }
    let entries = std::fs::read_dir(path).map_err(read_error(path))?;
    let newest = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name.strip_prefix('V')?.parse().ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n);
    Ok(newest.map(|(_, p)| p).unwrap_or_else(|| path.to_path_buf()))
}

/// Every `.emlx` file under `root`, in order, with its mailbox.
fn files(root: &Path, report: &mut MailReport) -> Result<Vec<(PathBuf, String)>, MailError> {
    let mut out = Vec::new();
    if root.is_file() {
        out.push((root.to_path_buf(), String::new()));
        return Ok(out);
    }
    // The store itself must be readable; without Full Disk Access it is not.
    let entries = std::fs::read_dir(root).map_err(read_error(root))?;
    walk(root, entries, &mut out, report)?;
    Ok(out)
}

/// Walks a folder without following symbolic links. macOS refusing a folder is
/// [`MailError::NoAccess`]; a folder that cannot be read for another reason is skipped and
/// listed.
fn walk(
    root: &Path,
    entries: std::fs::ReadDir,
    out: &mut Vec<(PathBuf, String)>,
    report: &mut MailReport,
) -> Result<(), MailError> {
    let mut found = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => match entry.file_type() {
                Ok(kind) => found.push((entry.path(), kind)),
                Err(e) => skip_unreadable(&entry.path(), e, report)?,
            },
            Err(e) => skip_unreadable(root, e, report)?,
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, kind) in found {
        let name = file_name(&path);
        // `MailData` holds Apple Mail's index, not messages.
        if name.starts_with('.') || name == "maildata" || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            match std::fs::read_dir(&path) {
                Ok(entries) => walk(root, entries, out, report)?,
                Err(e) => skip_unreadable(&path, e, report)?,
            }
        } else if kind.is_file() && name.ends_with(".emlx") {
            out.push((path.clone(), mailbox(root, &path)));
        }
    }
    Ok(())
}

fn skip_unreadable(
    path: &Path,
    error: std::io::Error,
    report: &mut MailReport,
) -> Result<(), MailError> {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return Err(MailError::NoAccess {
            path: path.to_path_buf(),
        });
    }
    report.skip(
        path.display().to_string(),
        format!("could not be read: {error}"),
    );
    Ok(())
}

/// The mailbox of a message: the `.mbox` folders between the store and the file. A folder of
/// `.emlx` files outside an `.mbox` folder is named after itself.
fn mailbox(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<String> = relative
        .components()
        .filter_map(|c| {
            let name = c.as_os_str().to_string_lossy();
            name.strip_suffix(".mbox")
                .or_else(|| name.strip_suffix(".imapmbox"))
                .map(str::to_string)
        })
        .collect();
    if !parts.is_empty() {
        return parts.join("/");
    }
    let start = if root.is_dir() {
        root
    } else {
        root.parent().unwrap_or(root)
    };
    let name = start
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".mbox").unwrap_or(&name).to_string()
}

/// The message in an `.emlx` file: the bytes its first line counts.
pub(crate) fn read_emlx(path: &Path) -> Result<Vec<u8>, MailError> {
    let bytes = std::fs::read(path).map_err(read_error(path))?;
    message_of(bytes).ok_or_else(|| MailError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the length on its first line is not one a message can have",
        ),
    })
}

/// The message of an `.emlx` file's bytes; `None` when the length it gives cannot be.
fn message_of(mut bytes: Vec<u8>) -> Option<Vec<u8>> {
    let Some(newline) = bytes.iter().position(|b| *b == b'\n') else {
        return Some(bytes);
    };
    let Ok(length) = std::str::from_utf8(&bytes[..newline])
        .unwrap_or("")
        .trim()
        .parse::<usize>()
    else {
        // No count: the whole file is the message.
        return Some(bytes);
    };
    let start = newline + 1;
    let end = start.checked_add(length)?.min(bytes.len());
    bytes.truncate(end);
    bytes.drain(..start);
    Some(bytes)
}

/// Reads every message of the store at `path` into `collected`.
pub(crate) fn read(
    path: &Path,
    options: &MailOptions,
    collected: &mut Collector<'_>,
    report: &mut MailReport,
) -> Result<(), MailError> {
    // A mailbox Apple Mail exported is an mbox.
    if super::mbox::is_exported_mailbox(path)
        || (path.is_file() && !file_name(path).ends_with(".emlx"))
    {
        return super::mbox::read(path, collected, report);
    }
    let root = root(path)?;
    let files = files(&root, report)?;
    if files.is_empty() {
        return Err(MailError::NotRecognized {
            path: path.to_path_buf(),
            what: "Apple Mail's store or a folder of .emlx files: there are no .emlx files in it"
                .into(),
        });
    }
    read_files(files, options, collected, report)
}

/// Reads the `.emlx` files given, each in its mailbox under the store `root` (a `V<n>`
/// folder), into `collected`: what a connector that knows which messages are new reads.
pub(crate) fn read_some(
    root: &Path,
    files: &[PathBuf],
    options: &MailOptions,
    collected: &mut Collector<'_>,
    report: &mut MailReport,
) -> Result<(), MailError> {
    let files = files
        .iter()
        .map(|file| (file.clone(), mailbox(root, file)))
        .collect();
    read_files(files, options, collected, report)
}

fn read_files(
    files: Vec<(PathBuf, String)>,
    options: &MailOptions,
    collected: &mut Collector<'_>,
    report: &mut MailReport,
) -> Result<(), MailError> {
    for (file, mailbox) in files {
        let raw = match read_emlx(&file) {
            Ok(raw) => raw,
            Err(e @ MailError::NoAccess { .. }) => return Err(e),
            Err(e) => {
                report.skip(file.display().to_string(), e.to_string());
                continue;
            }
        };
        let partial = file_name(&file).ends_with(".partial.emlx");
        let folder = (!mailbox.is_empty()).then_some(mailbox.as_str());
        let order = collected.next_order();
        match message::parse(&raw, Origin::Emlx(file.clone()), folder, order) {
            Some(mut mail) => {
                if partial && options.attachments {
                    // The files are not in the message: they are named, not attached.
                    report.limit(
                        "Apple Mail keeps the attachments of some messages outside them; those \
                         were named in the thread, not attached.",
                    );
                    mail.origin = Origin::Elsewhere;
                }
                collected.add(mail);
            }
            None => report.skip(file.display().to_string(), "not a mail message"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_store_from_its_path() {
        assert!(is_store(Path::new("/Users/example/Library/Mail")));
        assert!(is_store(Path::new("/Users/example/Library/Mail/V10")));
        assert!(!is_store(Path::new(
            "/Users/example/Library/Mail/V10/INBOX.mbox"
        )));
        assert!(!is_store(Path::new("/Users/example/Mail")));
    }

    #[test]
    fn reads_the_counted_message() {
        let message = b"Subject: Hi\n\nHello\n".to_vec();
        let mut file = format!("{}\n", message.len()).into_bytes();
        file.extend_from_slice(&message);
        file.extend_from_slice(b"<?xml version=\"1.0\"?><plist></plist>\n");
        assert_eq!(message_of(file), Some(message));
        // A length that does not fit is refused, not a panic.
        let huge = format!("{}\nSubject: Hi\n", usize::MAX).into_bytes();
        assert_eq!(message_of(huge), None);
    }

    #[test]
    fn names_mailboxes() {
        let root = Path::new("/store/V10");
        let file = root.join("ACC/[Gmail].mbox/All Mail.mbox/UUID/Data/1/Messages/1.emlx");
        assert_eq!(mailbox(root, &file), "[Gmail]/All Mail");
    }
}

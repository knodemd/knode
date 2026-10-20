//! Bear: a backup (`.bear2bk`, `.bearbk`: a zip of TextBundles), TextBundles (`.textbundle`
//! folders, `.textpack` zips, or a folder of them), its database (`database.sqlite` in the group
//! container `9K33E3U3T4.net.shinyfrog.bear`, read from a copy), or a folder of notes exported as
//! Markdown.
//!
//! A TextBundle is `text.md` (or `text.markdown`, `text.txt`), `info.json` and `assets/`. Bear
//! adds to `info.json` an object `net.shinyfrog.bear` with `uniqueIdentifier`, `creationDate`,
//! `modificationDate` (ISO 8601), `trashed` and `archived`.
//!
//! In the database, notes are rows of `ZSFNOTE` (`ZUNIQUEIDENTIFIER`, `ZTITLE`, `ZTEXT`,
//! `ZCREATIONDATE`, `ZMODIFICATIONDATE` as Core Data seconds, `ZTRASHED`, `ZARCHIVED`,
//! `ZENCRYPTED`, `ZPERMANENTLYDELETED`), and files rows of `ZSFNOTEFILE` (`ZNOTE`,
//! `ZUNIQUEIDENTIFIER`, `ZFILENAME`) whose file is
//! `Local Files/Note Images/<id>/<name>` or `Local Files/Note Files/<id>/<name>`.
//!
//! The first line of a note is its title. Tags are Bear's: `#tag`, `#nested/tag` and
//! `#multi word tag#`. `[[Title]]` links stay as they are: Bear links notes by title, as Knode
//! does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::Connection;

use super::db::{self, pick, sql_error};
use super::folder;
use super::markdown::{self, LinkRef, Names};
use super::source::{self, Source};
use super::write::{Batch, Blob, Draft};
use super::{ImportError, NotesReport, dates, io_error, keys};

/// Bear's group container, in `~/Library/Group Containers`.
pub(crate) const CONTAINER: &str = "9K33E3U3T4.net.shinyfrog.bear";
/// Its database.
pub(crate) const DATABASE: &str = "database.sqlite";

/// Whether the file is Bear's database.
pub(crate) fn is_database(path: &Path) -> bool {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()
        .is_some_and(|conn| !db::columns(&conn, "ZSFNOTE").is_empty())
}

/// Whether a path inside an export is in a TextBundle.
pub(crate) fn in_bundle(name: &str) -> bool {
    bundle_of(name).is_some()
}

/// The TextBundle folder a path is in: `Notes/Plan.textbundle` for
/// `Notes/Plan.textbundle/text.md`.
fn bundle_of(name: &str) -> Option<&str> {
    let lower = name.to_lowercase();
    let at = lower.find(".textbundle/")?;
    Some(&name[..at + ".textbundle".len()])
}

/// Bear's tags in a body: `#tag`, `#nested/tag`, and `#multi word#` (as `multi-word`).
pub(crate) fn tags(body: &str) -> BTreeSet<String> {
    let mut out = crate::tags::inline(body);
    // `#multi word tag#`: a `#`, words, and a `#` closing it on the same line.
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let starts = chars[i] == '#'
                && (i == 0 || chars[i - 1].is_whitespace())
                && chars
                    .get(i + 1)
                    .is_some_and(|c| !c.is_whitespace() && *c != '#');
            if starts && let Some(len) = chars[i + 1..].iter().position(|c| *c == '#') {
                let inner: String = chars[i + 1..i + 1 + len].iter().collect();
                let closes = chars
                    .get(i + 2 + len)
                    .is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation());
                if inner.contains(' ') && !inner.ends_with(' ') && closes {
                    if let Some(tag) = markdown::tag(&inner) {
                        // The first word was read as a tag of its own: not one.
                        if let Some(first) = inner.split_whitespace().next() {
                            out.remove(&first.to_lowercase());
                        }
                        out.insert(tag);
                    }
                    i += len + 2;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

pub(crate) fn read(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let database = if path.is_dir() {
        [
            path.join(DATABASE),
            path.join("Application Data").join(DATABASE),
        ]
        .into_iter()
        .find(|p| p.is_file())
    } else {
        (path.file_name().is_some_and(|n| n == DATABASE)).then(|| path.to_path_buf())
    };
    if let Some(database) = database {
        return read_database(&database, report);
    }
    let source = Source::open(path).map_err(io_error(path))?;
    let single = path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("textbundle") || e.eq_ignore_ascii_case("textpack")
    });
    if single || source.names().iter().any(|n| in_bundle(n)) {
        return read_bundles(source, path, single, report);
    }
    folder::read(path, report, tags)
}

/// A note's title and body from Bear's text: the first line, without its `#`s, is the title.
fn split(text: &str) -> (String, String) {
    let trimmed = text.trim_start_matches(['\n', '\r']);
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let title = first.trim().trim_start_matches('#').trim();
    (markdown::title(title), rest.to_string())
}

fn read_bundles(
    mut source: Source,
    path: &Path,
    single: bool,
    report: &mut NotesReport,
) -> Result<Batch, ImportError> {
    // The bundles, by folder: `Plan.textbundle` -> its files.
    let mut bundles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in source.names() {
        let bundle = if single {
            // A `.textbundle` folder or `.textpack` zip: its files are at the top, or in one
            // folder.
            bundle_of(name).unwrap_or("").to_string()
        } else {
            match bundle_of(name) {
                Some(b) => b.to_string(),
                None => continue,
            }
        };
        bundles.entry(bundle).or_default().push(name.clone());
    }
    let mut drafts = Vec::new();
    for (bundle, files) in bundles {
        let inside = |name: &str| -> String {
            if bundle.is_empty() {
                name.to_string()
            } else {
                format!("{bundle}/{name}")
            }
        };
        let Some(text_file) = ["text.md", "text.markdown", "text.txt"]
            .iter()
            .map(|n| inside(n))
            .find(|n| files.contains(n))
        else {
            continue;
        };
        let label = if bundle.is_empty() {
            path.display().to_string()
        } else {
            bundle.clone()
        };
        let text = match source.read_string(&text_file) {
            Ok(text) => text,
            Err(e) => {
                report.skip(label, e.to_string());
                continue;
            }
        };
        let info: serde_json::Value = source
            .read(&inside("info.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let bear = &info["net.shinyfrog.bear"];
        if flag(&bear["trashed"]) {
            continue;
        }
        let (title, body) = split(&text);
        let mut names = Names::default();
        let mut attachments = Vec::new();
        let body = markdown::rewrite_links(&body, |link: &LinkRef| {
            if markdown::is_url(&link.dest) {
                return None;
            }
            let decoded = markdown::decode_path(&link.dest);
            let path = source::resolve(&bundle, &decoded)?;
            if !files.contains(&path) {
                return None;
            }
            let name = names.take(source::file_name(&path));
            attachments.push((name.clone(), Blob::Entry(path)));
            Some(if link.image {
                format!("![[{name}]]")
            } else {
                markdown::wiki(&name, &link.text)
            })
        });
        let id = bear["uniqueIdentifier"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| bundle.clone());
        let date = |v: &serde_json::Value| match v {
            serde_json::Value::String(s) => dates::iso(s),
            serde_json::Value::Number(n) => n.as_f64().and_then(dates::core_data),
            _ => None,
        };
        let mut draft = Draft {
            source_id: id,
            tags: tags(&body),
            title,
            body: markdown::tidy(&body),
            created: date(&bear["creationDate"]),
            updated: date(&bear["modificationDate"]),
            attachments,
            ..Draft::default()
        };
        if flag(&bear["archived"]) {
            draft
                .properties
                .insert(keys::ARCHIVED.into(), "true".into());
        }
        drafts.push(draft);
    }
    Ok(Batch {
        drafts,
        source: Some(source),
    })
}

fn flag(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or(false) || value.as_i64().unwrap_or(0) != 0
}

struct NoteRow {
    pk: i64,
    id: String,
    title: String,
    text: String,
    created: Option<f64>,
    modified: Option<f64>,
    archived: bool,
    encrypted: bool,
}

fn read_database(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let copy = db::open_copy(path)?;
    let conn = &copy.conn;
    let cols = db::columns(conn, "ZSFNOTE");
    if cols.is_empty() {
        return Err(ImportError::NotRecognized {
            path: path.to_path_buf(),
            what: "Bear's database".into(),
        });
    }
    let err = sql_error(path);
    let sql = format!(
        "SELECT n.Z_PK, n.ZUNIQUEIDENTIFIER, {}, {}, {}, {}, {}, {} FROM ZSFNOTE n \
         WHERE COALESCE({}, 0) = 0 AND COALESCE({}, 0) = 0 ORDER BY n.Z_PK",
        pick(&cols, "n", &["ZTITLE"]),
        pick(&cols, "n", &["ZTEXT"]),
        pick(&cols, "n", &["ZCREATIONDATE"]),
        pick(&cols, "n", &["ZMODIFICATIONDATE"]),
        pick(&cols, "n", &["ZARCHIVED"]),
        pick(&cols, "n", &["ZENCRYPTED"]),
        pick(&cols, "n", &["ZTRASHED"]),
        pick(&cols, "n", &["ZPERMANENTLYDELETED"]),
    );
    let mut stmt = conn.prepare(&sql).map_err(&err)?;
    let notes: Vec<NoteRow> = stmt
        .query_map([], |r| {
            Ok(NoteRow {
                pk: r.get(0)?,
                id: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                text: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                created: r.get(4)?,
                modified: r.get(5)?,
                archived: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                encrypted: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
            })
        })
        .map_err(&err)?
        .filter_map(Result::ok)
        .collect();

    // Files, by note: name in the text -> file on disk.
    let mut files: BTreeMap<i64, Vec<(String, String)>> = BTreeMap::new();
    let file_cols = db::columns(conn, "ZSFNOTEFILE");
    if file_cols.contains("ZNOTE") && file_cols.contains("ZFILENAME") {
        let mut stmt = conn
            .prepare("SELECT ZNOTE, ZUNIQUEIDENTIFIER, ZFILENAME FROM ZSFNOTEFILE WHERE ZNOTE IS NOT NULL")
            .map_err(&err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .map_err(&err)?;
        for (note, id, name) in rows.filter_map(Result::ok) {
            files.entry(note).or_default().push((id, name));
        }
    }
    let local = path.parent().unwrap_or(Path::new(".")).join("Local Files");

    let mut drafts = Vec::new();
    for note in notes {
        let (first_line_title, body) = split(&note.text);
        let title = if note.title.trim().is_empty() {
            first_line_title
        } else {
            markdown::title(&note.title)
        };
        if note.encrypted {
            report.skip(title, "encrypted in Bear: decrypt it there to import it");
            continue;
        }
        let note_files = files.get(&note.pk).cloned().unwrap_or_default();
        let find = |wanted: &str| -> Option<std::path::PathBuf> {
            // `<id>/<name>` (Bear 1's `[image:...]`) or a bare file name (Bear 2).
            let (id, name) = match wanted.split_once('/') {
                Some((id, name)) => (Some(id), name),
                None => (None, wanted),
            };
            note_files
                .iter()
                .filter(|(fid, fname)| fname == name && id.is_none_or(|id| id == fid))
                .chain(
                    note_files
                        .iter()
                        .filter(|(fid, _)| Some(fid.as_str()) == id),
                )
                .flat_map(|(fid, fname)| {
                    ["Note Images", "Note Files"].map(|dir| local.join(dir).join(fid).join(fname))
                })
                .find(|p| p.is_file())
        };
        let mut names = Names::default();
        let mut attachments = Vec::new();
        let mut attach = |file: std::path::PathBuf, image: bool, text: &str| -> String {
            let raw = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = names.take(&raw);
            attachments.push((name.clone(), Blob::File(file)));
            if image {
                format!("![[{name}]]")
            } else {
                markdown::wiki(&name, text)
            }
        };
        // Bear 1: `[image:<id>/<name>]` and `[file:<id>/<name>]`.
        let mut text = String::with_capacity(body.len());
        let mut rest = body.as_str();
        while let Some(at) = rest.find('[') {
            text.push_str(&rest[..at]);
            let tail = &rest[at..];
            let kind = if tail.starts_with("[image:") {
                Some(true)
            } else if tail.starts_with("[file:") {
                Some(false)
            } else {
                None
            };
            let end = tail.find(']');
            match (kind, end) {
                (Some(image), Some(end)) => {
                    let inner = &tail[if image { 7 } else { 6 }..end];
                    match find(inner) {
                        Some(file) => text.push_str(&attach(file, image, "")),
                        None => text.push_str(&tail[..=end]),
                    }
                    rest = &tail[end + 1..];
                }
                _ => {
                    text.push('[');
                    rest = &tail[1..];
                }
            }
        }
        text.push_str(rest);
        // Bear 2: `![](name.png)`, `[name.pdf](name.pdf)`.
        let body = markdown::rewrite_links(&text, |link: &LinkRef| {
            if markdown::is_url(&link.dest) {
                return None;
            }
            let decoded = markdown::decode_path(&link.dest);
            let file = find(&decoded)?;
            Some(attach(file, link.image, &link.text))
        });
        let mut draft = Draft {
            source_id: note.id.clone(),
            tags: tags(&body),
            title,
            body: markdown::tidy(&body),
            created: note.created.and_then(dates::core_data),
            updated: note.modified.and_then(dates::core_data),
            attachments,
            ..Draft::default()
        };
        if note.archived {
            draft
                .properties
                .insert(keys::ARCHIVED.into(), "true".into());
        }
        drafts.push(draft);
    }
    Ok(Batch {
        drafts,
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bear_tags() {
        let found: Vec<String> =
            tags("#work and #home/garden and #multi word tag# here\n```\n#code#\n```")
                .into_iter()
                .collect();
        assert_eq!(found, ["home/garden", "multi-word-tag", "work"]);
    }

    #[test]
    fn bundles() {
        assert_eq!(
            bundle_of("a/Plan.textbundle/text.md"),
            Some("a/Plan.textbundle")
        );
        assert_eq!(bundle_of("Plan.md"), None);
        assert_eq!(
            split("# Plan\nBody"),
            ("Plan".to_string(), "Body".to_string())
        );
    }
}

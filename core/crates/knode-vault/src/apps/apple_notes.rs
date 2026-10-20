//! Apple Notes: its database, `NoteStore.sqlite` in the group container
//! `group.com.apple.notes`, or a folder of notes exported as Markdown.
//!
//! # The database
//!
//! The database is Core Data's: notes, folders and attachments are rows of
//! `ZICCLOUDSYNCINGOBJECT`, and the body of each note is a row of `ZICNOTEDATA`, its `ZDATA` a
//! gzipped protobuf. Apple documents none of it; what is read here is what the open-source
//! parsers of the format describe (macOS 10.11 and later):
//!
//! - a note row has `ZIDENTIFIER`, a title (`ZTITLE1`, `ZTITLE` before), dates as Core Data
//!   seconds (`ZCREATIONDATE3`/`1`, `ZMODIFICATIONDATE1`), its folder (`ZFOLDER`),
//!   `ZMARKEDFORDELETION` and `ZISPASSWORDPROTECTED`;
//! - a folder row has its name in `ZTITLE2`, its parent in `ZPARENT` and `ZFOLDERTYPE` 1 for
//!   Recently Deleted;
//! - an attachment row has `ZIDENTIFIER`, `ZTYPEUTI`, `ZNOTE`, and `ZMEDIA` (a row with
//!   `ZIDENTIFIER` and `ZFILENAME`, the file being under `Accounts/<account>/Media/<id>/`),
//!   `ZALTTEXT` and `ZTOKENCONTENTIDENTIFIER` (hashtags, mentions, links to notes) or
//!   `ZURLSTRING` (links);
//! - the body is `NoteStoreProto { 2: Document { 3: Note { 2: text, 5: repeated AttributeRun } } }`,
//!   each run `{ 1: length in UTF-16 units, 2: ParagraphStyle { 1: style, 4: indent,
//!   5: Checklist { 2: done }, 8: quote }, 5: font weight (1 bold, 2 italic, 3 both),
//!   7: strikethrough, 9: link, 12: AttachmentInfo { 1: id, 2: type } }`. Styles: 0 title,
//!   1 heading, 2 subheading, 4 monospaced, 100 and 101 bullets, 102 numbers, 103 checklist.
//!
//! Not read: tables (their cells are a separate CRDT), drawings and scans without a fallback
//! image, and colors, fonts and underlining, which Markdown does not have. Locked notes are
//! encrypted and skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use super::db::{self, pick, sql_error};
use super::markdown::{self, Names, escape, wrap};
use super::proto::Message;
use super::write::{Batch, Blob, Draft};
use super::{ImportError, NoteApp, NotesReport, dates, folder};

/// The group container of Apple Notes, in `~/Library/Group Containers`.
pub(crate) const CONTAINER: &str = "group.com.apple.notes";
/// The database in it.
pub(crate) const DATABASE: &str = "NoteStore.sqlite";

const TABLES_LIMIT: &str =
    "Apple Notes tables are not imported yet: each leaves a line in its note.";
const DRAWINGS_LIMIT: &str =
    "Apple Notes drawings and scans are imported as their preview image when the database has one.";

/// Fails with [`ImportError::NoAccess`] when macOS keeps the path from being read.
pub(crate) fn check_access(path: &Path) -> Result<(), ImportError> {
    let denied = |e: &std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
    let no_access = || ImportError::NoAccess {
        app: NoteApp::AppleNotes,
        path: path.to_path_buf(),
    };
    match fs::metadata(path) {
        Err(e) if denied(&e) => return Err(no_access()),
        Err(_) => return Ok(()),
        Ok(meta) if meta.is_dir() => {
            if let Err(e) = fs::read_dir(path) {
                if denied(&e) {
                    return Err(no_access());
                }
            }
            match fs::File::open(path.join(DATABASE)) {
                Err(e) if denied(&e) => return Err(no_access()),
                _ => {}
            }
        }
        Ok(_) => {
            if let Err(e) = fs::File::open(path) {
                if denied(&e) {
                    return Err(no_access());
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn read(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let database = if path.is_dir() {
        path.join(DATABASE)
    } else {
        path.to_path_buf()
    };
    if database.is_file() {
        return read_database(&database, report);
    }
    if path.is_dir()
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        return folder::read(path, report, crate::tags::inline);
    }
    Err(ImportError::NotRecognized {
        path: path.to_path_buf(),
        what: "the Apple Notes database or a folder of notes exported from Apple Notes".into(),
    })
}

/// A row of the database for a note.
struct NoteRow {
    identifier: String,
    title: String,
    created: Option<f64>,
    modified: Option<f64>,
    folder: Option<i64>,
    deleted: bool,
    locked: bool,
    data: Option<Vec<u8>>,
}

/// An attachment row, with its file when it has one.
#[derive(Debug, Clone, Default)]
struct AttachmentRow {
    uti: String,
    alt: Option<String>,
    token: Option<String>,
    url: Option<String>,
    title: Option<String>,
    media_id: Option<String>,
    file_name: Option<String>,
}

fn read_database(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let copy = db::open_copy(path)?;
    let conn = &copy.conn;
    let cols = db::columns(conn, "ZICCLOUDSYNCINGOBJECT");
    if cols.is_empty() || db::columns(conn, "ZICNOTEDATA").is_empty() {
        return Err(ImportError::NotRecognized {
            path: path.to_path_buf(),
            what: "an Apple Notes database (macOS 10.11 or later)".into(),
        });
    }
    let container = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let err = sql_error(path);

    // Folders: name, parent, and whether it is Recently Deleted.
    let mut folders: BTreeMap<i64, (String, Option<i64>, bool)> = BTreeMap::new();
    if cols.contains("ZTITLE2") {
        let sql = format!(
            "SELECT o.Z_PK, o.ZTITLE2, {}, {} FROM ZICCLOUDSYNCINGOBJECT o WHERE o.ZTITLE2 IS NOT NULL",
            pick(&cols, "o", &["ZPARENT"]),
            pick(&cols, "o", &["ZFOLDERTYPE"]),
        );
        let mut stmt = conn.prepare(&sql).map_err(&err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(&err)?;
        for row in rows.filter_map(Result::ok) {
            folders.insert(row.0, (row.1, row.2, row.3 == Some(1)));
        }
    }

    // Attachments, by identifier, with their media.
    let mut attachments: BTreeMap<String, AttachmentRow> = BTreeMap::new();
    let uti = pick(&cols, "o", &["ZTYPEUTI", "ZTYPEUTI1"]);
    if uti != "NULL" {
        let sql = format!(
            "SELECT o.ZIDENTIFIER, {uti}, {}, {}, {}, {}, m.ZIDENTIFIER, {} \
             FROM ZICCLOUDSYNCINGOBJECT o LEFT JOIN ZICCLOUDSYNCINGOBJECT m ON m.Z_PK = {} \
             WHERE {uti} IS NOT NULL AND o.ZIDENTIFIER IS NOT NULL",
            pick(&cols, "o", &["ZALTTEXT"]),
            pick(&cols, "o", &["ZTOKENCONTENTIDENTIFIER"]),
            pick(&cols, "o", &["ZURLSTRING"]),
            pick(&cols, "o", &["ZTITLE", "ZUSERTITLE"]),
            pick(&cols, "m", &["ZFILENAME"]),
            pick(&cols, "o", &["ZMEDIA"]),
        );
        let mut stmt = conn.prepare(&sql).map_err(&err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    AttachmentRow {
                        uti: r.get::<_, String>(1)?,
                        alt: r.get(2)?,
                        token: r.get(3)?,
                        url: r.get(4)?,
                        title: r.get(5)?,
                        media_id: r.get(6)?,
                        file_name: r.get(7)?,
                    },
                ))
            })
            .map_err(&err)?;
        for (id, row) in rows.filter_map(Result::ok) {
            attachments.insert(id, row);
        }
    }

    // Notes.
    let sql = format!(
        "SELECT n.Z_PK, n.ZIDENTIFIER, {}, {}, {}, {}, {}, {}, d.ZDATA \
         FROM ZICCLOUDSYNCINGOBJECT n JOIN ZICNOTEDATA d ON d.ZNOTE = n.Z_PK \
         WHERE n.ZIDENTIFIER IS NOT NULL ORDER BY n.Z_PK",
        pick(&cols, "n", &["ZTITLE1", "ZTITLE"]),
        pick(
            &cols,
            "n",
            &["ZCREATIONDATE3", "ZCREATIONDATE1", "ZCREATIONDATE"]
        ),
        pick(&cols, "n", &["ZMODIFICATIONDATE1", "ZMODIFICATIONDATE"]),
        pick(&cols, "n", &["ZFOLDER"]),
        pick(&cols, "n", &["ZMARKEDFORDELETION"]),
        pick(&cols, "n", &["ZISPASSWORDPROTECTED"]),
    );
    let mut stmt = conn.prepare(&sql).map_err(&err)?;
    let notes: Vec<NoteRow> = stmt
        .query_map([], |r| {
            Ok(NoteRow {
                identifier: r.get(1)?,
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                created: r.get(3)?,
                modified: r.get(4)?,
                folder: r.get(5)?,
                deleted: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                locked: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                data: r.get(8)?,
            })
        })
        .map_err(&err)?
        .filter_map(Result::ok)
        .collect();

    let folder_path = |mut pk: Option<i64>| -> (Option<String>, bool) {
        let mut parts = Vec::new();
        let mut trash = false;
        let mut seen = BTreeSet::new();
        while let Some(id) = pk {
            if !seen.insert(id) {
                break;
            }
            let Some((name, parent, deleted)) = folders.get(&id) else {
                break;
            };
            trash |= *deleted;
            parts.push(name.replace('/', "-"));
            pk = *parent;
        }
        parts.reverse();
        ((!parts.is_empty()).then(|| parts.join("/")), trash)
    };

    let notes: Vec<NoteRow> = notes
        .into_iter()
        .filter(|n| !n.deleted && !folder_path(n.folder).1)
        .collect();
    let titles: BTreeMap<String, String> = notes
        .iter()
        .map(|n| (n.identifier.to_uppercase(), markdown::title(&n.title)))
        .collect();

    let mut drafts = Vec::new();
    for note in notes {
        let title = markdown::title(&note.title);
        if note.locked {
            report.skip(
                title,
                "locked with a password in Apple Notes: unlock it there (remove the lock) to import it",
            );
            continue;
        }
        let Some(body) = note.data.as_deref().and_then(decode_note) else {
            if note.data.as_deref().is_some_and(|d| !d.is_empty()) {
                report.skip(title, "its body is not in a format Knode reads");
                continue;
            }
            drafts.push(Draft {
                source_id: note.identifier.clone(),
                title,
                folder: folder_path(note.folder).0,
                created: note.created.and_then(dates::core_data),
                updated: note.modified.and_then(dates::core_data),
                ..Draft::default()
            });
            continue;
        };
        let mut ctx = Render {
            attachments: &attachments,
            titles: &titles,
            container: &container,
            names: Names::default(),
            files: Vec::new(),
            tags: BTreeSet::new(),
            report,
        };
        let markdown = ctx.render(&body, &title);
        let Render { files, tags, .. } = ctx;
        drafts.push(Draft {
            source_id: note.identifier.clone(),
            title,
            body: markdown::tidy(&markdown),
            tags,
            folder: folder_path(note.folder).0,
            created: note.created.and_then(dates::core_data),
            updated: note.modified.and_then(dates::core_data),
            attachments: files,
            ..Draft::default()
        });
    }
    Ok(Batch {
        drafts,
        source: None,
    })
}

/// A note body: its text and attribute runs.
#[derive(Debug, Default)]
pub(crate) struct Body {
    text: Vec<u16>,
    runs: Vec<Run>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Paragraph {
    style: Option<u64>,
    indent: u64,
    checked: Option<bool>,
    quote: bool,
}

#[derive(Debug, Clone, Default)]
struct Run {
    len: usize,
    paragraph: Paragraph,
    weight: u64,
    strike: bool,
    link: Option<String>,
    attachment: Option<(String, String)>,
}

/// Gunzips and reads a note's `ZDATA`.
pub(crate) fn decode_note(data: &[u8]) -> Option<Body> {
    let mut raw = Vec::new();
    let bytes: &[u8] = if data.starts_with(&[0x1f, 0x8b]) {
        GzDecoder::new(data).read_to_end(&mut raw).ok()?;
        &raw
    } else {
        data
    };
    let store = Message::parse(bytes)?;
    let note = store.message(2)?.message(3)?;
    let text = note.string(2).unwrap_or_default();
    let runs = note
        .messages(5)
        .into_iter()
        .map(|run| {
            let style = run.message(2);
            Run {
                len: run.int(1).unwrap_or(0) as usize,
                paragraph: style
                    .map(|s| Paragraph {
                        style: s.int(1),
                        indent: s.int(4).unwrap_or(0),
                        checked: s.message(5).map(|c| c.int(2).unwrap_or(0) != 0),
                        quote: s.int(8).unwrap_or(0) != 0,
                    })
                    .unwrap_or_default(),
                weight: run.int(5).unwrap_or(0),
                strike: run.int(7).unwrap_or(0) != 0,
                link: run.string(9).filter(|l| !l.is_empty()),
                attachment: run
                    .message(12)
                    .and_then(|a| Some((a.string(1)?, a.string(2).unwrap_or_default()))),
            }
        })
        .collect();
    Some(Body {
        text: text.encode_utf16().collect(),
        runs,
    })
}

/// One line of a note: its paragraph style, as Markdown and as plain text.
struct Line {
    paragraph: Paragraph,
    markdown: String,
    plain: String,
}

struct Render<'a> {
    attachments: &'a BTreeMap<String, AttachmentRow>,
    titles: &'a BTreeMap<String, String>,
    container: &'a Path,
    names: Names,
    files: Vec<(String, Blob)>,
    tags: BTreeSet<String>,
    report: &'a mut NotesReport,
}

/// The note id of an `applenotes:note/<id>?...` link.
fn note_link(url: &str) -> Option<String> {
    let rest = url.strip_prefix("applenotes:note/")?;
    let id = rest.split(['?', '/', '#']).next()?;
    (!id.is_empty()).then(|| id.to_uppercase())
}

impl Render<'_> {
    fn render(&mut self, body: &Body, title: &str) -> String {
        let mut lines: Vec<Line> = Vec::new();
        let mut current = Line {
            paragraph: Paragraph::default(),
            markdown: String::new(),
            plain: String::new(),
        };
        let mut pos = 0usize;
        let mut runs = body.runs.clone();
        let covered: usize = runs.iter().map(|r| r.len).sum();
        if covered < body.text.len() {
            runs.push(Run {
                len: body.text.len() - covered,
                ..Run::default()
            });
        }
        for run in &runs {
            let end = (pos + run.len).min(body.text.len());
            let text = String::from_utf16_lossy(&body.text[pos.min(end)..end]);
            pos = end;
            let mut pieces = text.split('\n').peekable();
            while let Some(piece) = pieces.next() {
                if !piece.is_empty() {
                    let md = self.inline(run, piece);
                    current.markdown.push_str(&md);
                    current.plain.push_str(piece);
                }
                current.paragraph = run.paragraph.clone();
                if pieces.peek().is_some() {
                    lines.push(std::mem::replace(
                        &mut current,
                        Line {
                            paragraph: Paragraph::default(),
                            markdown: String::new(),
                            plain: String::new(),
                        },
                    ));
                }
            }
        }
        if !current.plain.is_empty() || !current.markdown.is_empty() {
            lines.push(current);
        }
        // The first line is the title Notes shows.
        if let Some(first) = lines.iter().position(|l| !l.plain.trim().is_empty())
            && markdown::title(&lines[first].plain) == title
        {
            lines.drain(..=first);
        }
        block_lines(&lines)
    }

    /// One piece of a run, inside one line, as Markdown.
    fn inline(&mut self, run: &Run, piece: &str) -> String {
        if let Some((id, uti)) = &run.attachment {
            return piece
                .chars()
                .map(|c| {
                    if c == '\u{fffc}' {
                        self.attachment(id, uti)
                    } else {
                        escape(&c.to_string())
                    }
                })
                .collect();
        }
        let piece = piece.replace('\u{fffc}', "");
        if run.paragraph.style == Some(4) {
            return piece;
        }
        let mut md = escape(&piece);
        md = match run.weight {
            1 => wrap(&md, "**"),
            2 => wrap(&md, "*"),
            3 => wrap(&md, "***"),
            _ => md,
        };
        if run.strike {
            md = wrap(&md, "~~");
        }
        if let Some(link) = &run.link {
            if let Some(target) = note_link(link).and_then(|id| self.titles.get(&id)) {
                return markdown::wiki(target, &piece);
            }
            return format!("[{md}](<{}>)", link.replace(['<', '>'], ""));
        }
        md
    }

    fn attachment(&mut self, id: &str, uti: &str) -> String {
        let row = self
            .attachments
            .get(id)
            .cloned()
            .unwrap_or_else(|| AttachmentRow {
                uti: uti.to_string(),
                ..AttachmentRow::default()
            });
        let uti = row.uti.as_str();
        let alt = row.alt.clone().unwrap_or_default();
        if uti.ends_with(".hashtag") {
            let tag = alt.trim().trim_start_matches('#').to_string();
            if !tag.is_empty() {
                self.tags.insert(tag.clone());
                return format!("#{}", tag.split_whitespace().collect::<Vec<_>>().join("-"));
            }
            return String::new();
        }
        if uti.ends_with(".mention") {
            return escape(&alt);
        }
        if uti.ends_with(".link") {
            let target = row
                .token
                .as_deref()
                .and_then(note_link)
                .and_then(|id| self.titles.get(&id));
            return match target {
                Some(title) => markdown::wiki(title, &alt),
                None => escape(&alt),
            };
        }
        if uti == "com.apple.notes.table" {
            self.report.limit(TABLES_LIMIT);
            return "\n*(A table from Apple Notes: tables are not imported yet.)*\n".into();
        }
        if uti == "public.url" {
            let url = row.url.clone().unwrap_or_default();
            let text = row
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or(url.clone());
            return if url.is_empty() {
                escape(&text)
            } else {
                format!("[{}](<{}>)", escape(&text), url.replace(['<', '>'], ""))
            };
        }
        let drawing = uti.starts_with("com.apple.drawing")
            || uti.starts_with("com.apple.paper")
            || uti == "com.apple.notes.gallery";
        let file = if drawing {
            self.report.limit(DRAWINGS_LIMIT);
            fallback_image(self.container, id)
        } else {
            row.media_id
                .as_deref()
                .and_then(|media| media_file(self.container, media, row.file_name.as_deref()))
        };
        match file {
            Some(path) => {
                let raw = row
                    .file_name
                    .clone()
                    .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "attachment".into());
                let name = self.names.take(&raw);
                self.files.push((name.clone(), Blob::File(path)));
                markdown::embed(&name)
            }
            None if drawing => "\n*(A drawing or scan from Apple Notes, not imported.)*\n".into(),
            None => {
                let name = row.file_name.clone().unwrap_or_else(|| uti.to_string());
                format!("*(Attachment not found: {})*", escape(&name))
            }
        }
    }
}

/// The lines of a note as Markdown blocks.
fn block_lines(lines: &[Line]) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut numbers: Vec<u64> = Vec::new();
    let mut previous_list = false;
    while i < lines.len() {
        let line = &lines[i];
        let p = &line.paragraph;
        let quote = if p.quote { "> " } else { "" };
        if p.style == Some(4) {
            let mut code = Vec::new();
            while i < lines.len() && lines[i].paragraph.style == Some(4) {
                code.push(lines[i].plain.as_str());
                i += 1;
            }
            out.push_str("\n```\n");
            out.push_str(&code.join("\n"));
            out.push_str("\n```\n\n");
            previous_list = false;
            numbers.clear();
            continue;
        }
        let indent = "  ".repeat(p.indent as usize);
        let is_list = matches!(p.style, Some(100..=103));
        if !(is_list && previous_list) {
            out.push('\n');
        }
        if p.style != Some(102) {
            numbers.truncate(p.indent as usize);
        }
        let text = line.markdown.trim_end();
        match p.style {
            Some(0) => out.push_str(&format!("{quote}# {text}\n")),
            Some(1) => out.push_str(&format!("{quote}## {text}\n")),
            Some(2) => out.push_str(&format!("{quote}### {text}\n")),
            Some(100 | 101) => out.push_str(&format!("{quote}{indent}- {text}\n")),
            Some(102) => {
                let level = p.indent as usize;
                numbers.resize(level + 1, 0);
                numbers[level] += 1;
                let indent = "   ".repeat(level);
                out.push_str(&format!("{quote}{indent}{}. {text}\n", numbers[level]));
            }
            Some(103) => {
                let mark = if p.checked == Some(true) { "x" } else { " " };
                out.push_str(&format!("{quote}{indent}- [{mark}] {text}\n"));
            }
            _ => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("{quote}{text}\n"));
                }
            }
        }
        if !is_list {
            numbers.clear();
        }
        previous_list = is_list;
        i += 1;
    }
    out
}

/// The file of a media row: under `Accounts/<account>/Media/<media id>/` (or `Media/<media id>/`
/// in older versions), the one named `file_name` when there is one, else the first file.
fn media_file(container: &Path, media: &str, file_name: Option<&str>) -> Option<PathBuf> {
    let mut dirs = vec![container.join("Media").join(media)];
    if let Ok(accounts) = fs::read_dir(container.join("Accounts")) {
        for account in accounts.filter_map(Result::ok) {
            dirs.push(account.path().join("Media").join(media));
        }
    }
    let mut found = Vec::new();
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        files_under(dir, &mut found, 3);
    }
    found.sort();
    file_name
        .and_then(|name| {
            found
                .iter()
                .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
        })
        .or(found.first())
        .cloned()
}

/// The preview image Notes keeps of a drawing: `Accounts/<account>/FallbackImages/<id>.png`
/// (or `.jpg`, or in a folder of that name).
fn fallback_image(container: &Path, id: &str) -> Option<PathBuf> {
    let accounts = fs::read_dir(container.join("Accounts")).ok()?;
    let mut found = Vec::new();
    for account in accounts.filter_map(Result::ok) {
        let dir = account.path().join("FallbackImages");
        for ext in ["png", "jpg", "jpeg"] {
            let file = dir.join(format!("{id}.{ext}"));
            if file.is_file() {
                return Some(file);
            }
        }
        let folder = dir.join(id);
        if folder.is_dir() {
            files_under(&folder, &mut found, 2);
        }
    }
    found.sort();
    found.into_iter().next()
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if hidden {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => out.push(path),
            Ok(m) if m.is_dir() && depth > 0 => files_under(&path, out, depth - 1),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_links() {
        assert_eq!(
            note_link("applenotes:note/ab-cd?ownerIdentifier=x").as_deref(),
            Some("AB-CD")
        );
        assert_eq!(note_link("https://x"), None);
    }
}

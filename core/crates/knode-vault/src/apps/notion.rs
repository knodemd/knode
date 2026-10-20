//! Notion: the "Markdown & CSV" export of a page or a workspace, as downloaded (a `.zip`, with
//! the `.zip` parts Notion splits large exports into) or extracted.
//!
//! Every page is `<Title> <id>.md`, its id 32 hexadecimal characters, and its sub-pages and
//! files are in the folder `<Title> <id>/` next to it. A database is `<Title> <id>.csv` (and
//! `<Title> <id>_all.csv`, with every row, which is read when it is there), and its rows are
//! pages in the folder `<Title> <id>/`, each starting with `Column: value` lines. A page starts
//! with `# Title`. Links between pages are relative paths, percent-encoded.
//!
//! Each page becomes a node, found again by its Notion id, with its parent pages in
//! [`keys::FOLDER`](super::keys::FOLDER) and a `part-of` link to its parent. A database becomes
//! a node with the database as a Markdown table, and each row a node with its columns as
//! `notion.<column>` properties (a column `Tags` gives tags; `Created` and `Last edited` give
//! dates, read as UTC since the export does not say the time zone).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::folder::stem;
use super::markdown::{self, LinkRef, Names};
use super::source::{self, Source};
use super::write::{Batch, Blob, Draft};
use super::{ImportError, NotesReport, dates, io_error, keys};

/// Splits a Notion file stem, `Plan 0123...cdef`, into its title and id.
fn split_id(stem: &str) -> Option<(&str, &str)> {
    let (title, id) = stem.rsplit_once(' ')?;
    let is_id = id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit());
    is_id.then_some((title, id))
}

/// Whether a file of an export is named as Notion names its pages and databases.
pub(crate) fn is_notion_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    (lower.ends_with(".md") || lower.ends_with(".csv"))
        && split_id(stem(name).trim_end_matches("_all")).is_some()
}

/// A Notion id in a web address: `https://www.notion.so/Plan-0123...cdef?pvs=4`.
fn id_in_url(url: &str) -> Option<String> {
    if !url.contains("notion.so/") && !url.contains("notion.site/") {
        return None;
    }
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    let id = last.rsplit('-').next()?;
    let id = id.replace('-', "");
    (id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_lowercase())
}

/// One page or database of the export.
#[derive(Debug, Clone)]
struct Item {
    /// The file.
    path: String,
    /// The Notion id, or the path when the file has none.
    id: String,
    title: String,
    database: bool,
}

/// A key as a property key: lower case letters, digits and `-`.
fn slug(key: &str) -> String {
    let mut out = String::new();
    for c in key.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    let out = if out.is_empty() {
        "property".into()
    } else {
        out
    };
    let max = 64 - keys::NOTION_PREFIX.len();
    format!("{}{}", keys::NOTION_PREFIX, &out[..out.len().min(max)])
}

/// Reads CSV text (RFC 4180): fields separated by `,`, quoted with `"` when they hold `,`, `"`
/// or line breaks.
pub(crate) fn csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                any = false;
            }
            c => field.push(c),
        }
    }
    if any {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

/// A table cell: `|` escaped, line breaks as `<br>`.
fn cell(text: &str) -> String {
    markdown::escape(text.trim())
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

pub(crate) fn read(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let mut source = Source::open(path).map_err(io_error(path))?;
    let names: Vec<String> = source.names().to_vec();
    if !names.iter().any(|n| is_notion_name(n)) {
        return Err(ImportError::NotRecognized {
            path: path.to_path_buf(),
            what: "a Notion export (Markdown & CSV)".into(),
        });
    }
    let all: BTreeSet<&str> = names
        .iter()
        .filter_map(|n| n.strip_suffix("_all.csv"))
        .collect();
    // Pages and databases, by path; databases by their path without `_all`.
    let mut items: BTreeMap<String, Item> = BTreeMap::new();
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for name in &names {
        let lower = name.to_lowercase();
        let database = lower.ends_with(".csv");
        if !(lower.ends_with(".md") || database) {
            continue;
        }
        // `X.csv` when `X_all.csv` is there: the `_all` one is read.
        if database && !name.ends_with("_all.csv") && all.contains(&name[..name.len() - 4]) {
            continue;
        }
        let key = name.strip_suffix("_all.csv").map(|s| format!("{s}.csv"));
        let key = key.unwrap_or_else(|| name.clone());
        let text = match source.read_string(name) {
            Ok(text) => text,
            Err(e) => {
                report.skip(name.clone(), e.to_string());
                continue;
            }
        };
        let file_stem = stem(&key);
        let (stem_title, id) = match split_id(file_stem) {
            Some((title, id)) => (title.to_string(), id.to_lowercase()),
            None => (file_stem.to_string(), key.clone()),
        };
        let title = if database {
            markdown::title(&stem_title)
        } else {
            super::folder::split_title(&text, &stem_title).0
        };
        items.insert(
            key.clone(),
            Item {
                path: key.clone(),
                id,
                title,
                database,
            },
        );
        texts.insert(key, text);
    }
    // The page or database whose folder a path is in.
    let parent_of = |path: &str| -> Option<&Item> {
        let dir = source::parent(path);
        if dir.is_empty() {
            return None;
        }
        items
            .get(&format!("{dir}.md"))
            .or_else(|| items.get(&format!("{dir}.csv")))
    };
    let folder_of = |path: &str| -> Option<String> {
        let mut parts = Vec::new();
        let mut current = path.to_string();
        let mut guard = 0;
        while let Some(parent) = parent_of(&current) {
            parts.push(parent.title.replace('/', "-"));
            current = parent.path.clone();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        parts.reverse();
        (!parts.is_empty()).then(|| parts.join("/"))
    };
    let by_path: BTreeMap<String, String> = items
        .values()
        .map(|i| (i.path.clone(), i.title.clone()))
        .collect();
    let by_id: BTreeMap<String, String> = items
        .values()
        .map(|i| (i.id.clone(), i.title.clone()))
        .collect();
    // Row pages of a database, by their folder and title.
    let mut rows_by_title: BTreeMap<(String, String), String> = BTreeMap::new();
    for item in items.values() {
        if let Some(parent) = parent_of(&item.path).filter(|p| p.database) {
            rows_by_title.insert(
                (parent.path.clone(), item.title.clone()),
                item.title.clone(),
            );
        }
    }

    let mut drafts = Vec::new();
    for item in items.values() {
        let text = &texts[&item.path];
        let parent = parent_of(&item.path);
        let mut draft = Draft {
            source_id: item.id.clone(),
            title: item.title.clone(),
            folder: folder_of(&item.path),
            parent: parent.map(|p| p.id.clone()),
            ..Draft::default()
        };
        if item.database {
            draft.body = database_table(text, &item.path, &rows_by_title);
            drafts.push(draft);
            continue;
        }
        let (_, mut body) = super::folder::split_title(text, &item.title);
        if parent.is_some_and(|p| p.database) {
            body = take_properties(&body, &mut draft);
        }
        let dir = source::parent(&item.path).to_string();
        let mut names = Names::default();
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut attachments = Vec::new();
        let body = markdown::rewrite_links(&body, |link: &LinkRef| {
            if markdown::is_url(&link.dest) {
                let title = id_in_url(&link.dest).and_then(|id| by_id.get(&id))?;
                return Some(markdown::wiki(title, &link.text));
            }
            let dest = link.dest.split('#').next().unwrap_or("");
            let decoded = markdown::decode_path(dest);
            let target = source::resolve(&dir, &decoded)?;
            let target = target
                .strip_suffix("_all.csv")
                .map(|s| format!("{s}.csv"))
                .unwrap_or(target);
            if let Some(title) = by_path.get(&target) {
                return Some(markdown::wiki(title, &link.text));
            }
            if !source.has(&target) {
                return None;
            }
            let name = taken
                .entry(target.clone())
                .or_insert_with(|| {
                    let name = names.take(source::file_name(&target));
                    attachments.push((name.clone(), Blob::Entry(target.clone())));
                    name
                })
                .clone();
            Some(if link.image || markdown::is_image(&name) {
                format!("![[{name}]]")
            } else {
                markdown::wiki(&name, &link.text)
            })
        });
        draft.body = markdown::tidy(&body);
        draft.attachments = attachments;
        drafts.push(draft);
    }
    // Parents before children, so links find them; the order of the export otherwise.
    let depth = |d: &Draft| d.folder.as_deref().map_or(0, |f| f.split('/').count());
    drafts.sort_by_key(depth);
    Ok(Batch {
        drafts,
        source: Some(source),
    })
}

/// The `Column: value` lines at the start of a database row's page, as properties (and tags
/// and dates); returns the body after them.
fn take_properties(body: &str, draft: &mut Draft) -> String {
    let trimmed = body.trim_start_matches(['\n', '\r']);
    let mut lines = trimmed.lines().peekable();
    let mut used = 0usize;
    while let Some(line) = lines.peek() {
        let Some((key, value)) = line.split_once(": ") else {
            break;
        };
        if key.is_empty() || key.len() > 100 || key.starts_with(['#', '-', '*', '>', '!', '[']) {
            break;
        }
        apply_column(draft, key, value);
        used += line.len() + 1;
        lines.next();
    }
    trimmed
        .get(used.min(trimmed.len())..)
        .unwrap_or("")
        .to_string()
}

/// Keeps one column of a row: as a property, and as tags or dates when it names them.
fn apply_column(draft: &mut Draft, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let lower = key.trim().to_lowercase();
    match lower.as_str() {
        "tags" | "tag" => {
            draft.tags.extend(
                value
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty()),
            );
        }
        "created" | "created time" | "date created" => draft.created = dates::notion(value),
        "last edited" | "last edited time" | "updated" => draft.updated = dates::notion(value),
        _ => {}
    }
    draft.properties.insert(slug(key), value.to_string());
}

/// A database as a Markdown table, the first column linking to the row pages.
fn database_table(text: &str, path: &str, rows: &BTreeMap<(String, String), String>) -> String {
    let table = csv(text);
    let Some((header, body)) = table.split_first() else {
        return String::new();
    };
    let width = header.len().max(1);
    let mut out = String::new();
    let line = |cells: Vec<String>| format!("| {} |\n", cells.join(" | "));
    out.push_str(&line(header.iter().map(|h| cell(h)).collect()));
    out.push_str(&line(vec!["---".to_string(); width]));
    for row in body {
        let mut cells: Vec<String> = (0..width)
            .map(|i| cell(row.get(i).map(String::as_str).unwrap_or("")))
            .collect();
        if let Some(first) = row.first() {
            let title = markdown::title(first);
            if let Some(target) = rows.get(&(path.to_string(), title)) {
                cells[0] = markdown::wiki(target, target).replace('|', "\\|");
            }
        }
        out.push_str(&line(cells));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(split_id(&format!("My Plan {id}")), Some(("My Plan", id)));
        assert!(split_id("My Plan").is_none());
        assert!(is_notion_name(&format!("Tasks {id}_all.csv")));
        assert_eq!(
            id_in_url(&format!("https://www.notion.so/My-Plan-{id}?pvs=4")).as_deref(),
            Some(id)
        );
        assert_eq!(slug("Due Date!"), "notion.due-date");
    }

    #[test]
    fn reads_csv() {
        let rows = csv("\u{feff}Name,Notes\r\n\"a, b\",\"say \"\"hi\"\"\nthere\"\nc,\n");
        assert_eq!(
            rows,
            vec![
                vec!["\u{feff}Name".to_string(), "Notes".into()],
                vec!["a, b".into(), "say \"hi\"\nthere".into()],
                vec!["c".into(), "".into()],
            ]
        );
    }
}

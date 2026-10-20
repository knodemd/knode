//! Readwise exports: the CSV export and the Markdown export.

use std::fs;
use std::path::Path;

use knode_format::{Provenance, Space};

use super::book::{self, Book, Highlight};
use super::{ReadingReport, dates, sources};
use crate::{Result, Skipped, io_error};

/// Imports the CSV export of Readwise: one node of kind `source` per book or article, written
/// by `by`, with its highlights as sections (their note, tags, location and date), and the
/// document tags as the node's tags.
///
/// Importing the file again only adds the highlights that are new; the same book from Kindle is
/// the same node.
pub fn import_readwise_csv(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(io_error(path))?;
    let file = file_name(path);
    let mut report = ReadingReport::default();
    match parse_csv(&bytes) {
        Ok(books) => write_books(space, &file, books, &mut report, by)?,
        Err(reason) => report.skipped.push(Skipped { path: file, reason }),
    }
    Ok(report)
}

/// Imports the Markdown export of Readwise: a `.md` file, or a folder of them (as the export
/// unzips, `Books/`, `Articles/`...). Each file is a book or an article: its `## Metadata`
/// gives the author, URL, category and document tags, its `## Highlights` the highlights, with
/// their notes, tags and locations. See [`import_readwise_csv`].
pub fn import_readwise_markdown(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let mut files = Vec::new();
    collect_md(path, &mut files)?;
    let mut report = ReadingReport::default();
    for file in files {
        let text = fs::read_to_string(&file).map_err(io_error(&file))?;
        let name = file
            .strip_prefix(path)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        match parse_markdown(&text, &file_stem(&file)) {
            Some(b) => write_books(space, &name, vec![b], &mut report, by)?,
            None => report.skipped.push(Skipped {
                path: name,
                reason: "no Readwise highlights found".into(),
            }),
        }
    }
    Ok(report)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The `.md` files at `path`, sorted; hidden files and folders are skipped.
fn collect_md(path: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let meta = fs::metadata(path).map_err(io_error(path))?;
    if meta.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(io_error(path))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| !file_name(p).starts_with('.'))
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_md(&entry, out)?;
        } else if entry
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            out.push(entry);
        }
    }
    Ok(())
}

pub(super) fn write_books(
    space: &mut Space,
    file: &str,
    books: Vec<Book>,
    report: &mut ReadingReport,
    by: &Provenance,
) -> Result<()> {
    for b in books {
        match book::write(space, &b, sources::READWISE, by) {
            Ok(imported) => report.imported.push(imported),
            Err(e) => report.skipped.push(Skipped {
                path: format!("{file}: {}", b.title),
                reason: e.to_string(),
            }),
        }
    }
    Ok(())
}

/// Splits a list of tags written `a, b` or `#a #b` or `[[a]] [[b]]`.
fn split_tags(text: &str) -> Vec<String> {
    text.split([',', ' '])
        .map(|t| t.trim().trim_start_matches("[[").trim_end_matches("]]"))
        .map(|t| t.trim_start_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// A location as Readwise gives it: `Location 120`, `Page 3`.
pub(super) fn place(kind: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match kind.trim().to_lowercase().as_str() {
        "page" => Some(format!("Page {value}")),
        "location" => Some(format!("Location {value}")),
        "time" => Some(format!("Time {value}")),
        _ => None,
    }
}

/// Reads the CSV export into books, in the order they first appear.
pub(crate) fn parse_csv(bytes: &[u8]) -> std::result::Result<Vec<Book>, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| format!("not a Readwise CSV export: {e}"))?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();
    let col = |name: &str| headers.iter().position(|h| h == name);
    let (Some(text_col), Some(title_col)) = (col("highlight"), col("book title")) else {
        return Err("not a Readwise CSV export: no Highlight and Book Title columns".into());
    };
    let author_col = col("book author");
    let note_col = col("note");
    let tags_col = col("tags");
    let kind_col = col("location type");
    let location_col = col("location");
    let date_col = col("highlighted at");
    let doc_tags_col = col("document tags");

    let mut books: Vec<Book> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("not a Readwise CSV export: {e}"))?;
        let get = |c: Option<usize>| {
            c.and_then(|c| record.get(c))
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let Some(title) = get(Some(title_col)) else {
            continue;
        };
        let author = get(author_col);
        let i = match books
            .iter()
            .position(|b| b.title == title && b.author == author)
        {
            Some(i) => i,
            None => {
                books.push(Book {
                    title,
                    author,
                    ..Book::default()
                });
                books.len() - 1
            }
        };
        if let Some(tags) = get(doc_tags_col) {
            for tag in split_tags(&tags) {
                if !books[i].tags.contains(&tag) {
                    books[i].tags.push(tag);
                }
            }
        }
        let text = get(Some(text_col)).unwrap_or_default();
        let note = get(note_col);
        if text.is_empty() && note.is_none() {
            continue;
        }
        books[i].highlights.push(Highlight {
            text,
            note,
            location: place(
                &get(kind_col).unwrap_or_default(),
                &get(location_col).unwrap_or_default(),
            ),
            chapter: None,
            date: get(date_col).and_then(|d| dates::readwise(&d)),
            tags: get(tags_col).map(|t| split_tags(&t)).unwrap_or_default(),
        });
    }
    books.retain(|b| !b.highlights.is_empty());
    Ok(books)
}

/// The text of a link label, or the text itself: `[[Ada]]` is `Ada`.
fn unlink(text: &str) -> String {
    text.trim()
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .trim()
        .to_string()
}

/// Splits a trailing `([Location 120](url))` or `(Location 120)` off a highlight.
fn split_location(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim_end();
    if !trimmed.ends_with(')') {
        return (trimmed.to_string(), None);
    }
    for marker in [" ([", " ("] {
        if let Some(at) = trimmed.rfind(marker) {
            let inner = &trimmed[at + 2..trimmed.len() - 1];
            let label = inner
                .strip_prefix('[')
                .and_then(|r| r.split_once("](").map(|(l, _)| l))
                .unwrap_or(inner);
            let lower = label.to_lowercase();
            if ["location", "page", "time"]
                .iter()
                .any(|w| lower.starts_with(w))
            {
                return (
                    trimmed[..at].trim_end().to_string(),
                    Some(label.to_string()),
                );
            }
        }
    }
    (trimmed.to_string(), None)
}

/// Reads one file of the Markdown export into its book. `None` when it has no highlights.
pub(crate) fn parse_markdown(text: &str, stem: &str) -> Option<Book> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut book = Book {
        title: stem.to_string(),
        ..Book::default()
    };
    let mut full_title = None;
    #[derive(PartialEq)]
    enum Section {
        None,
        Metadata,
        Highlights,
    }
    let mut section = Section::None;
    let mut current: Option<Highlight> = None;
    let mut in_frontmatter = text.starts_with("---\n");
    for (n, line) in text.lines().enumerate() {
        if in_frontmatter {
            if n > 0 && line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if let Some(title) = line.strip_prefix("# ") {
            book.title = title.trim().to_string();
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            let heading = heading.trim().to_lowercase();
            section = if heading == "metadata" {
                Section::Metadata
            } else if heading.contains("highlights") {
                Section::Highlights
            } else {
                Section::None
            };
            continue;
        }
        match section {
            Section::Metadata => {
                let Some(item) = line.trim().strip_prefix("- ") else {
                    continue;
                };
                let Some((key, value)) = item.split_once(':') else {
                    continue;
                };
                let value = value.trim();
                match key.trim().to_lowercase().as_str() {
                    "author" => book.author = Some(unlink(value)).filter(|a| !a.is_empty()),
                    "full title" => full_title = Some(value.to_string()).filter(|t| !t.is_empty()),
                    "url" => book.url = Some(value.to_string()).filter(|u| !u.is_empty()),
                    "category" => {
                        book.category = Some(value.trim_start_matches('#').to_string())
                            .filter(|c| !c.is_empty());
                    }
                    "document tags" | "tags" => book.tags.extend(split_tags(value)),
                    _ => {}
                }
            }
            Section::Highlights => {
                let indented = line.starts_with(' ') || line.starts_with('\t');
                let trimmed = line.trim();
                if !indented && let Some(item) = trimmed.strip_prefix("- ") {
                    if let Some(h) = current.take() {
                        book.highlights.push(h);
                    }
                    let (text, location) = split_location(item);
                    current = Some(Highlight {
                        text,
                        location,
                        ..Highlight::default()
                    });
                    continue;
                }
                let Some(h) = current.as_mut() else {
                    continue;
                };
                if let Some(sub) = trimmed.strip_prefix("- ") {
                    let plain = sub.replace("**", "");
                    let lower = plain.to_lowercase();
                    if lower.starts_with("note:") {
                        h.note = Some(plain[5..].trim().to_string()).filter(|n| !n.is_empty());
                    } else if lower.starts_with("tags:") {
                        h.tags.extend(split_tags(&plain[5..]));
                    } else if let Some(note) = &mut h.note {
                        note.push('\n');
                        note.push_str(sub);
                    }
                } else if !trimmed.is_empty() {
                    // A highlight on more than one line.
                    h.text.push('\n');
                    h.text.push_str(trimmed);
                }
            }
            Section::None => {}
        }
    }
    if let Some(h) = current.take() {
        book.highlights.push(h);
    }
    if let Some(full) = full_title
        && book.title.trim().is_empty()
    {
        book.title = full;
    }
    book.highlights.retain(|h| !h.text.trim().is_empty());
    (!book.highlights.is_empty()).then_some(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_locations_off_highlights() {
        assert_eq!(
            split_location("Text here. ([Location 120](https://readwise.io/to_kindle?x=1))"),
            ("Text here.".to_string(), Some("Location 120".to_string()))
        );
        assert_eq!(
            split_location("Text (Page 3)"),
            ("Text".to_string(), Some("Page 3".to_string()))
        );
        assert_eq!(
            split_location("Text (an aside)"),
            ("Text (an aside)".to_string(), None)
        );
    }
}

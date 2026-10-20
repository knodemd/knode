//! Kindle highlights: `My Clippings.txt` and the notebook a Kindle app exports as HTML.

use std::fs;
use std::path::Path;

use dom_query::Document as Html;
use knode_format::{Provenance, Space};

use super::book::{self, Book, Highlight};
use super::{ReadingReport, dates, sources};
use crate::{Result, Skipped, io_error};

/// Imports the highlights and notes of `My Clippings.txt`, the file a Kindle keeps in its
/// `documents` folder: one node of kind `source` per book, written by `by`.
///
/// Each highlight is a section of its book's node, with its page and location, and the date it
/// was added. A note goes with the highlight it was written on (the highlight that ends where
/// the note is); a note on no highlight is a section of its own. Bookmarks are left out.
/// Importing the file again only adds the highlights that are new; the same book from
/// [`import_kindle_notebook`] or Readwise is the same node.
pub fn import_kindle_clippings(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(io_error(path))?;
    write_books(space, path, parse_clippings(&text), by)
}

/// Imports the notebook a Kindle app exports (Export notebook, as HTML): one node of kind
/// `source` for the book, its highlights and notes as sections, under their chapter. See
/// [`import_kindle_clippings`].
pub fn import_kindle_notebook(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(io_error(path))?;
    let books = parse_notebook(&String::from_utf8_lossy(&bytes));
    write_books(space, path, books, by)
}

fn write_books(
    space: &mut Space,
    path: &Path,
    books: Vec<Book>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut report = ReadingReport::default();
    if books.is_empty() {
        report.skipped.push(Skipped {
            path: file,
            reason: "no Kindle highlights found".into(),
        });
        return Ok(report);
    }
    for b in books {
        match book::write(space, &b, sources::KINDLE, by) {
            Ok(imported) => report.imported.push(imported),
            Err(e) => report.skipped.push(Skipped {
                path: format!("{file}: {}", b.title),
                reason: e.to_string(),
            }),
        }
    }
    Ok(report)
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Highlight,
    Note,
    Bookmark,
}

/// A location as `40-41`: its first and last numbers.
fn span(location: &str) -> Option<(u64, u64)> {
    let digits = |s: &str| s.trim().parse::<u64>().ok();
    match location.split_once('-') {
        Some((a, b)) => {
            let a = digits(a)?;
            // `1234-56` means 1234-1256 in some exports.
            let b_text = b.trim();
            let mut b = digits(b_text)?;
            if b < a {
                let a_text = a.to_string();
                let keep = a_text.len().saturating_sub(b_text.len());
                b = format!("{}{b_text}", &a_text[..keep]).parse().ok()?;
            }
            Some((a, b))
        }
        None => digits(location).map(|n| (n, n)),
    }
}

/// Splits `Title (Author)` into the title and the author.
fn title_and_author(line: &str) -> (String, Option<String>) {
    let line = line.trim();
    if !line.ends_with(')') {
        return (line.to_string(), None);
    }
    let mut depth = 0usize;
    for (i, c) in line.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    let title = line[..i].trim();
                    let author = line[i + 1..line.len() - 1].trim();
                    if title.is_empty() {
                        break;
                    }
                    return (
                        title.to_string(),
                        Some(author.to_string()).filter(|a| !a.is_empty()),
                    );
                }
            }
            _ => {}
        }
    }
    (line.to_string(), None)
}

/// Reads `My Clippings.txt` into books, in the order they first appear.
pub(crate) fn parse_clippings(text: &str) -> Vec<Book> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut books: Vec<Book> = Vec::new();
    // For each highlight: its book, its index and its location span, to pair notes.
    let mut spans: Vec<(usize, usize, (u64, u64))> = Vec::new();
    for entry in text.split("==========") {
        let mut lines = entry
            .trim_matches(|c: char| c == '\n' || c == '\u{feff}')
            .lines();
        let Some(title_line) = lines.next() else {
            continue;
        };
        let title_line = title_line.trim_start_matches('\u{feff}');
        let Some(meta) = lines.next() else {
            continue;
        };
        let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        let (title, author) = title_and_author(title_line);
        if title.is_empty() {
            continue;
        }

        let lower = meta.to_lowercase();
        let kind = if lower.contains("bookmark") {
            Kind::Bookmark
        } else if lower.contains("note") && !lower.contains("highlight") {
            Kind::Note
        } else {
            Kind::Highlight
        };
        if kind == Kind::Bookmark || body.is_empty() {
            continue;
        }
        let mut page = None;
        let mut location = None;
        let mut date = None;
        for part in meta.trim_start_matches(['-', ' ']).split('|') {
            let part = part.trim();
            let lower = part.to_lowercase();
            if let Some(at) = lower.find("added on") {
                let raw = part[at + "added on".len()..].trim();
                date = Some(dates::kindle(raw).unwrap_or_else(|| raw.to_string()));
            } else if lower.contains("page") {
                page = part.split_whitespace().last().map(str::to_string);
            } else if lower.contains("location") || lower.starts_with("loc") {
                location = part.split_whitespace().last().map(str::to_string);
            }
        }
        let place = match (&page, &location) {
            (Some(p), Some(l)) => Some(format!("Page {p}, Location {l}")),
            (Some(p), None) => Some(format!("Page {p}")),
            (None, Some(l)) => Some(format!("Location {l}")),
            (None, None) => None,
        };

        let b = match books
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

        if kind == Kind::Note {
            // The latest highlight of the book that ends where the note is, or covers it.
            let at = location.as_deref().and_then(span);
            let target = at.and_then(|(n, _)| {
                spans
                    .iter()
                    .rev()
                    .filter(|(book, _, _)| *book == b)
                    .find(|(_, _, (start, end))| *end == n || (*start..=*end).contains(&n))
                    .map(|(_, i, _)| *i)
            });
            match target {
                Some(i) if books[b].highlights[i].note.is_none() => {
                    books[b].highlights[i].note = Some(body);
                }
                _ => books[b].highlights.push(Highlight {
                    note: Some(body),
                    location: place,
                    date,
                    ..Highlight::default()
                }),
            }
            continue;
        }

        if let Some(s) = location.as_deref().and_then(span) {
            spans.push((b, books[b].highlights.len(), s));
        }
        books[b].highlights.push(Highlight {
            text: body,
            location: place,
            date,
            ..Highlight::default()
        });
    }
    books.retain(|b| !b.highlights.is_empty());
    books
}

/// Reads a Kindle notebook export into its book.
pub(crate) fn parse_notebook(html: &str) -> Vec<Book> {
    let doc = Html::from(html);
    let mut book = Book::default();
    let mut chapter: Option<String> = None;
    // The heading of the entry whose text comes next.
    let mut pending: Option<(Kind, Option<String>)> = None;
    let text_of =
        |n: &dom_query::NodeRef<'_>| n.text().split_whitespace().collect::<Vec<_>>().join(" ");
    for node in doc
        .select(".bookTitle, .authors, .sectionHeading, .noteHeading, .noteText")
        .nodes()
    {
        let class = node
            .attr("class")
            .map(|c| c.to_string())
            .unwrap_or_default();
        let has = |name: &str| class.split_whitespace().any(|c| c == name);
        let text = text_of(node);
        if has("bookTitle") {
            book.title = text;
        } else if has("authors") {
            book.author = Some(text).filter(|a| !a.is_empty());
        } else if has("sectionHeading") {
            chapter = Some(text).filter(|c| !c.is_empty());
        } else if has("noteHeading") {
            let (kind_part, place) = match text.split_once(" - ") {
                Some((k, p)) => (k.trim().to_lowercase(), Some(p.trim().to_string())),
                None => (text.to_lowercase(), None),
            };
            let kind = if kind_part.starts_with("note") {
                Kind::Note
            } else if kind_part.starts_with("bookmark") {
                Kind::Bookmark
            } else {
                Kind::Highlight
            };
            let place = place.map(|p| {
                p.split(['·', '|'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            pending = Some((kind, place.filter(|p| !p.is_empty())));
        } else if has("noteText") {
            let Some((kind, place)) = pending.take() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            match kind {
                Kind::Bookmark => {}
                Kind::Note => match book.highlights.last_mut() {
                    // A note follows the highlight it was written on.
                    Some(last) if last.note.is_none() && !last.text.is_empty() => {
                        last.note = Some(text);
                    }
                    _ => book.highlights.push(Highlight {
                        note: Some(text),
                        location: place,
                        chapter: chapter.clone(),
                        ..Highlight::default()
                    }),
                },
                Kind::Highlight => book.highlights.push(Highlight {
                    text,
                    location: place,
                    chapter: chapter.clone(),
                    ..Highlight::default()
                }),
            }
        }
    }
    if book.title.trim().is_empty() || book.highlights.is_empty() {
        return Vec::new();
    }
    vec![book]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_title_and_author() {
        assert_eq!(
            title_and_author("A Book (Second Edition) (Ada Lovelace)"),
            (
                "A Book (Second Edition)".to_string(),
                Some("Ada Lovelace".to_string())
            )
        );
        assert_eq!(title_and_author("No Author"), ("No Author".into(), None));
    }

    #[test]
    fn reads_location_spans() {
        assert_eq!(span("40-41"), Some((40, 41)));
        assert_eq!(span("1234-56"), Some((1234, 1256)));
        assert_eq!(span("7"), Some((7, 7)));
    }
}

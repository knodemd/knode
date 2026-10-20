//! PDF files: their text, page by page, with the PDF attached.

use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use knode_format::{MAX_ATTACHMENT_SIZE, Properties, Provenance, Space};
use lopdf::Object;

use super::document::{self, Document};
use super::markdown::escape_text;
use super::{ReadingReport, dates, keys, sources};
use crate::{Result, Skipped, io_error};

/// What is written in place of the text of a PDF that has none, such as a scanned document.
pub(crate) const NO_TEXT: &str = "No text was found in this PDF. It may be a scanned document: \
     Knode does not read text from images. The original file is attached.";

/// Imports a PDF as a node of kind `source`, written by `by`.
///
/// - The title is the PDF's own title when it has one, else the file name; the author and the
///   creation date go to [`reading.author`](keys::AUTHOR) and
///   [`reading.published`](keys::PUBLISHED), the number of pages to [`reading.pages`](keys::PAGES).
/// - The body is the text of each page after a `<!-- page n -->` marker. Lines that are entries of
///   the PDF's outline (its bookmarks) become headings; other text is kept as it reads, escaped
///   so that it stays text.
/// - The PDF is attached to the node under its file name, and linked from the top of the body.
/// - A PDF without text (a scanned document) still becomes a node, with a note that no text was
///   found and the file attached. Knode does not read text from images.
///
/// The same file imported again is the same node ([`Outcome::Unchanged`](super::Outcome)).
/// A file that cannot be read as a PDF is listed in [`ReadingReport::skipped`].
pub fn import_pdf(
    space: &mut Space,
    path: impl AsRef<Path>,
    by: &Provenance,
) -> Result<ReadingReport> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.pdf".into());
    let mut report = ReadingReport::default();
    let size = fs::metadata(path).map_err(io_error(path))?.len();
    if size > MAX_ATTACHMENT_SIZE {
        report.skipped.push(Skipped {
            path: name,
            reason: format!(
                "larger than {} MB, the most a space attaches",
                MAX_ATTACHMENT_SIZE / (1024 * 1024)
            ),
        });
        return Ok(report);
    }
    let bytes = fs::read(path).map_err(io_error(path))?;
    match read(&bytes, &name) {
        Ok(doc) => report.imported.push(document::write(space, doc, by)?),
        Err(reason) => report.skipped.push(Skipped { path: name, reason }),
    }
    Ok(report)
}

/// Reads a PDF into the document of its node.
fn read(bytes: &[u8], name: &str) -> std::result::Result<Document, String> {
    let pdf = lopdf::Document::load_mem(bytes).map_err(|e| format!("not a readable PDF: {e}"))?;
    let pages = pdf.get_pages().len();
    let info = info(&pdf);
    let outline = outline(&pdf);

    // Text extraction can fail, or panic, on unusual files: the PDF is still imported.
    let text = catch_unwind(AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem_by_pages(bytes)
    }));
    let (pages_text, note) = match text {
        Ok(Ok(pages)) => (pages, None),
        Ok(Err(e)) => (Vec::new(), Some(format!("the text could not be read: {e}"))),
        Err(_) => (Vec::new(), Some("the text could not be read".to_string())),
    };
    let has_text = pages_text.iter().any(|p| !p.trim().is_empty());

    let mut body = vec![format!("Original file: [[{name}]]")];
    if has_text {
        for (i, page) in pages_text.iter().enumerate() {
            body.push(format!("<!-- page {} -->", i + 1));
            let text = page_markdown(page, &outline);
            if !text.is_empty() {
                body.push(text);
            }
        }
    } else {
        body.push(NO_TEXT.to_string());
    }
    let note = match (has_text, note) {
        (true, _) => None,
        (false, Some(n)) => Some(n),
        (false, None) => Some("no text found: it may be a scanned document".into()),
    };

    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let title = info
        .get("Title")
        .filter(|t| !super::clean_title(t).is_empty())
        .cloned()
        .unwrap_or(stem);
    let mut props = Properties::new();
    props.insert(keys::FILE.into(), name.into());
    props.insert(keys::PAGES.into(), pages.to_string());
    if let Some(author) = info.get("Author").filter(|a| !a.trim().is_empty()) {
        props.insert(keys::AUTHOR.into(), author.trim().into());
    }
    if let Some(date) = info.get("CreationDate").and_then(|d| dates::pdf(d)) {
        props.insert(keys::PUBLISHED.into(), date);
    }
    Ok(Document {
        key: format!("pdf:{}", super::sha256(bytes)),
        by_content: true,
        source: sources::PDF,
        title,
        body: body.join("\n\n"),
        props,
        file: Some((name.to_string(), bytes.to_vec())),
        note,
    })
}

/// The text of a page as Markdown: paragraphs split at blank lines, outline entries as headings.
fn page_markdown(page: &str, outline: &HashMap<String, usize>) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<String>| {
        if !para.is_empty() {
            blocks.push(escape_text(&para.join("\n")));
            para.clear();
        }
    };
    for line in page.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut para, &mut blocks);
            continue;
        }
        if let Some(level) = outline.get(&heading_key(line)) {
            flush(&mut para, &mut blocks);
            let hashes = "#".repeat((level + 1).min(6));
            blocks.push(format!("{hashes} {}", super::markdown::escape_inline(line)));
            continue;
        }
        para.push(line);
    }
    flush(&mut para, &mut blocks);
    blocks.join("\n\n")
}

/// How a line and an outline title are compared: lower case, whitespace collapsed.
fn heading_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The titles of the PDF's outline, by [`heading_key`], with their level (1 for the top).
fn outline(pdf: &lopdf::Document) -> HashMap<String, usize> {
    let toc = catch_unwind(AssertUnwindSafe(|| pdf.get_toc()));
    match toc {
        Ok(Ok(toc)) => toc
            .toc
            .into_iter()
            .filter(|t| !t.title.trim().is_empty())
            .map(|t| (heading_key(&t.title), t.level.max(1)))
            .collect(),
        _ => HashMap::new(),
    }
}

/// The text entries of the PDF's document information dictionary.
fn info(pdf: &lopdf::Document) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(info) = pdf.trailer.get(b"Info") else {
        return out;
    };
    let dict = match info {
        Object::Reference(id) => pdf.get_dictionary(*id).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    };
    let Some(dict) = dict else {
        return out;
    };
    for (key, value) in dict.iter() {
        let value = match value {
            Object::Reference(id) => pdf.get_object(*id).ok(),
            other => Some(other),
        };
        if let Some(Object::String(bytes, _)) = value {
            let text = decode_text(bytes);
            if !text.trim().is_empty() {
                out.insert(String::from_utf8_lossy(key).into_owned(), text);
            }
        }
    }
    out
}

/// A PDF text string: UTF-16 with a byte order mark, UTF-8 with one, else PDFDocEncoding (read
/// as Latin-1, which it matches for letters).
fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| b as char).collect()
}

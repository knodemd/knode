//! Reading brought into a space: PDFs, web pages, Kindle highlights and Readwise exports.
//!
//! Every document becomes one node of kind [`source`](knode_format::kinds::SOURCE), with its
//! title, and its author, URL, file and dates in [`properties`](keys):
//!
//! - [`import_pdf`] reads the text of a PDF, page by page, and attaches the PDF;
//! - [`import_html`] reads a saved web page and [`import_url`] fetches one (only when asked),
//!   keeping the readable article as Markdown and the page's canonical URL;
//! - [`import_kindle_clippings`] reads `My Clippings.txt` and [`import_kindle_notebook`] the
//!   notebook a Kindle app exports as HTML, one node per book;
//! - [`import_readwise_csv`] and [`import_readwise_markdown`] read the CSV and Markdown exports of
//!   Readwise, one node per book or article.
//!
//! ```
//! use knode_format::{Provenance, Space};
//!
//! # let tmp = tempfile::tempdir().unwrap();
//! let me = Provenance::person("Ada");
//! let mut space = Space::create(tmp.path().join("Reading.knode"), "Reading")?;
//! let clippings = tmp.path().join("My Clippings.txt");
//! std::fs::write(&clippings, "Walden (Henry David Thoreau)\n\
//!     - Your Highlight on page 3 | Location 40-41 | Added on Monday, January 5, 2026 9:15:02 PM\n\n\
//!     I went to the woods because I wished to live deliberately.\n==========\n")?;
//!
//! let report = knode_vault::reading::import_kindle_clippings(&mut space, &clippings, &me)?;
//! assert_eq!(report.imported[0].title, "Walden");
//! assert_eq!(report.imported[0].highlights, 1);
//!
//! // Importing again only adds what is new.
//! let again = knode_vault::reading::import_kindle_clippings(&mut space, &clippings, &me)?;
//! assert_eq!(again.imported[0].highlights, 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Highlights
//!
//! The highlights of a book are sections of the book's node, in the order the export lists
//! them: a `##` heading with the location (and the chapter, when the export has it), the
//! highlight as a quote, then the note, tags and date. They are not nodes of their own: a
//! highlight is a passage of the book, read with the book, and one node per book keeps a space
//! of a hundred books at a hundred nodes. A note that should stand alone can be made from a
//! highlight later, by hand, and linked to the book.
//!
//! The same book from Kindle and from Readwise is one node: a book is found by its title and
//! author, whatever export it came from, and a highlight by its text.
//!
//! # Importing again
//!
//! Importing the same file again never duplicates:
//!
//! - a book keeps the highlights it has and gets only the new ones, added at the end of its
//!   body; the highlights it got are listed in [`keys::HIGHLIGHTS`], so a highlight removed from
//!   the node by hand does not come back;
//! - a PDF is found by its content: the same file is the same node, and a changed file is a new
//!   node next to the old one;
//! - a web page is found by its canonical URL (a saved page without one, by its content). A page
//!   that changed updates its node, unless the node was edited in Knode since it was imported:
//!   then it is left as it is ([`Outcome::Edited`]).
//!
//! Nothing here ever runs a page's scripts: HTML is only parsed.

mod book;
mod dates;
mod document;
mod kindle;
mod markdown;
mod pdf;
mod readwise;
mod web;

use knode_format::NodeId;
use serde::{Deserialize, Serialize};

pub use kindle::{import_kindle_clippings, import_kindle_notebook};
pub use pdf::import_pdf;
pub use readwise::{import_readwise_csv, import_readwise_markdown};
#[cfg(feature = "fetch")]
pub use web::{FetchedPage, USER_AGENT, fetch, import_url};
pub use web::{import_html, import_page};

use crate::Skipped;

/// The node properties the reading importers write.
pub mod keys {
    /// Where the node came from: [`PDF`](super::sources::PDF), [`WEB`](super::sources::WEB),
    /// [`KINDLE`](super::sources::KINDLE) or [`READWISE`](super::sources::READWISE). For a book
    /// with highlights from both, the first one.
    pub const SOURCE: &str = "reading.source";
    /// What the node is found by when the same reading is imported again: `pdf:<sha-256 of the
    /// file>`, `url:<canonical URL>`, `html:<sha-256 of the page>` or `book:<sha-256 of title
    /// and author>`.
    pub const KEY: &str = "reading.key";
    /// The author or authors, as the source writes them.
    pub const AUTHOR: &str = "reading.author";
    /// The URL of the document: the canonical URL of a web page, the URL a Readwise export gives.
    pub const URL: &str = "reading.url";
    /// The name of the file it was imported from.
    pub const FILE: &str = "reading.file";
    /// When the document was published or created, as the source gives it (ISO 8601 when it
    /// can be read).
    pub const PUBLISHED: &str = "reading.published";
    /// When a web page was fetched, RFC 3339 in UTC.
    pub const FETCHED: &str = "reading.fetched";
    /// The number of pages of a PDF.
    pub const PAGES: &str = "reading.pages";
    /// The name of the site of a web page.
    pub const SITE: &str = "reading.site";
    /// The Readwise category: `books`, `articles`, `tweets`, `podcasts`...
    pub const CATEGORY: &str = "reading.category";
    /// The ids of the highlights the node got, separated by spaces: the first 12 hexadecimal
    /// digits of the SHA-256 of each highlight's text.
    pub const HIGHLIGHTS: &str = "reading.highlights";
    /// The date of the latest highlight, ISO 8601 when it can be read.
    pub const LAST_HIGHLIGHT: &str = "reading.last_highlight";
    /// The `updated_at` of the node when an import last wrote its body: a web page imported again
    /// only updates a node that has not changed since.
    pub const WRITTEN: &str = "reading.written";
}

/// The values of [`keys::SOURCE`].
pub mod sources {
    /// A PDF file.
    pub const PDF: &str = "pdf";
    /// A web page, fetched or saved.
    pub const WEB: &str = "web";
    /// Kindle highlights.
    pub const KINDLE: &str = "kindle";
    /// A Readwise export.
    pub const READWISE: &str = "readwise";
}

/// What an import did to one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The node is new.
    Created,
    /// The node changed: new highlights, or a page that changed.
    Updated,
    /// The node already had everything.
    Unchanged,
    /// The document changed, but so did its node, in Knode, since it was imported: the node was
    /// left as it is.
    Edited,
}

/// One document an import brought in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Imported {
    /// The node.
    pub id: NodeId,
    /// Its title.
    pub title: String,
    /// What happened to it.
    pub outcome: Outcome,
    /// How many highlights were added to it.
    pub highlights: usize,
    /// Something worth knowing about it, such as a PDF without text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What a reading import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingReport {
    /// The documents brought in, in the order of the file.
    pub imported: Vec<Imported>,
    /// What could not be imported, and why: `path` is the file, or the file and the book.
    pub skipped: Vec<Skipped>,
}

impl ReadingReport {
    /// How many nodes are new.
    pub fn created(&self) -> usize {
        self.count(Outcome::Created)
    }

    /// How many highlights were added, in all.
    pub fn highlights(&self) -> usize {
        self.imported.iter().map(|i| i.highlights).sum()
    }

    fn count(&self, outcome: Outcome) -> usize {
        self.imported
            .iter()
            .filter(|i| i.outcome == outcome)
            .count()
    }
}

/// The SHA-256 of `bytes`, in hexadecimal.
pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A title as the format takes it: one line, without `[`, `]` or `|`, which `[[links]]` use.
pub(crate) fn clean_title(title: &str) -> String {
    title
        .replace('[', "(")
        .replace(']', ")")
        .replace('|', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tags as the format takes them: words joined by `-`, at most 64 characters.
pub(crate) fn clean_tags<'a>(tags: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut out: Vec<String> = tags
        .into_iter()
        .map(|t| {
            t.trim()
                .trim_start_matches('#')
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
                .to_lowercase()
        })
        .filter(|t| !t.is_empty() && t.chars().count() <= 64)
        .collect();
    out.sort();
    out.dedup();
    out
}

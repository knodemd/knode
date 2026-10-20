//! Writing the highlights of a book to its node: one section per highlight, only the new ones.

use std::collections::BTreeSet;

use knode_format::{NewNode, NodeUpdate, Properties, Provenance, Space, kinds};

use super::markdown::{escape_inline, escape_text, quote};
use super::{Imported, Outcome, keys};
use crate::Result;

/// A highlight, or a note on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Highlight {
    /// The highlighted text; empty for a note without a highlight.
    pub text: String,
    pub note: Option<String>,
    /// `Page 3`, `Location 40-41`...
    pub location: Option<String>,
    /// The chapter or section the export puts it under.
    pub chapter: Option<String>,
    /// When it was made, ISO 8601 when it could be read.
    pub date: Option<String>,
    pub tags: Vec<String>,
}

impl Highlight {
    /// The id of a highlight: the first 12 hexadecimal digits of the SHA-256 of its text, with
    /// whitespace collapsed, so the same passage from two exports is one highlight.
    pub(crate) fn id(&self) -> String {
        let text = if self.text.trim().is_empty() {
            format!("note:{}", self.note.as_deref().unwrap_or_default())
        } else {
            self.text.clone()
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        super::sha256(text.as_bytes())[..12].to_string()
    }

    /// The section of the highlight in the book's node.
    fn section(&self) -> String {
        let heading = match (&self.chapter, &self.location) {
            (Some(c), Some(l)) => format!("{c} - {l}"),
            (Some(c), None) => c.clone(),
            (None, Some(l)) => l.clone(),
            (None, None) if self.text.trim().is_empty() => "Note".into(),
            (None, None) => "Highlight".into(),
        };
        let mut parts = vec![format!(
            "## {}",
            escape_inline(&heading.split_whitespace().collect::<Vec<_>>().join(" "))
        )];
        if !self.text.trim().is_empty() {
            parts.push(quote(&escape_text(self.text.trim())));
        }
        if let Some(note) = self.note.as_deref().filter(|n| !n.trim().is_empty()) {
            parts.push(format!("**Note:** {}", escape_text(note.trim())));
        }
        let mut meta = Vec::new();
        let tags = super::clean_tags(&self.tags);
        if !tags.is_empty() {
            let tags: Vec<String> = tags.iter().map(|t| format!("#{t}")).collect();
            meta.push(format!("Tags: {}", tags.join(" ")));
        }
        if let Some(date) = &self.date {
            meta.push(format!("Highlighted {}", escape_inline(date)));
        }
        if !meta.is_empty() {
            parts.push(format!("*{}*", meta.join(" - ")));
        }
        parts.join("\n\n")
    }
}

/// A book, an article or any document with highlights.
#[derive(Debug, Clone, Default)]
pub(crate) struct Book {
    pub title: String,
    pub author: Option<String>,
    pub url: Option<String>,
    pub category: Option<String>,
    /// Tags of the document.
    pub tags: Vec<String>,
    pub highlights: Vec<Highlight>,
}

impl Book {
    /// What the book is found by: `book:` and the SHA-256 of its title and author, lower case,
    /// whitespace collapsed.
    pub(crate) fn key(&self) -> String {
        let norm = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let text = format!(
            "{}\n{}",
            norm(&self.title),
            norm(self.author.as_deref().unwrap_or(""))
        );
        format!("book:{}", super::sha256(text.as_bytes()))
    }
}

/// Writes the highlights of `book` that its node does not have yet, creating the node when there
/// is none.
pub(crate) fn write(
    space: &mut Space,
    book: &Book,
    source: &str,
    by: &Provenance,
) -> Result<Imported> {
    let key = book.key();
    let existing = space
        .find_by_property(keys::KEY, &key)?
        .first()
        .map(|s| s.id);
    let had: BTreeSet<String> = match existing {
        Some(id) => space
            .property(&id, keys::HIGHLIGHTS)?
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        None => BTreeSet::new(),
    };
    let mut ids: Vec<String> = had.iter().cloned().collect();
    let mut seen = had.clone();
    let mut sections = Vec::new();
    let mut last: Option<String> = None;
    for h in &book.highlights {
        if h.text.trim().is_empty() && h.note.as_deref().is_none_or(|n| n.trim().is_empty()) {
            continue;
        }
        if let Some(date) = &h.date {
            last = last.max(Some(date.clone()));
        }
        let id = h.id();
        if seen.insert(id.clone()) {
            ids.push(id);
            sections.push(h.section());
        }
    }
    let added = sections.len();

    let mut props = Properties::new();
    props.insert(keys::KEY.into(), key);
    if let Some(author) = book.author.as_deref().filter(|a| !a.trim().is_empty()) {
        props.insert(keys::AUTHOR.into(), author.trim().into());
    }
    if let Some(url) = book.url.as_deref().filter(|u| !u.trim().is_empty()) {
        props.insert(keys::URL.into(), url.trim().into());
    }
    if let Some(category) = book.category.as_deref().filter(|c| !c.trim().is_empty()) {
        props.insert(keys::CATEGORY.into(), category.trim().into());
    }
    props.insert(keys::HIGHLIGHTS.into(), ids.join(" "));
    let tags = super::clean_tags(&book.tags);

    let Some(id) = existing else {
        if let Some(last) = &last {
            props.insert(keys::LAST_HIGHLIGHT.into(), last.clone());
        }
        props.insert(keys::SOURCE.into(), source.into());
        let title = super::clean_title(&book.title);
        let node = space.create_node_with_properties(
            NewNode {
                kind: kinds::SOURCE.into(),
                title: if title.is_empty() {
                    "Untitled".into()
                } else {
                    title
                },
                body: sections.join("\n\n"),
                tags,
            },
            &props,
            by,
        )?;
        return Ok(Imported {
            id: node.id,
            title: node.title,
            outcome: Outcome::Created,
            highlights: added,
            note: None,
        });
    };

    let node = space
        .get_node(&id)?
        .ok_or(knode_format::Error::NotFound(id))?;
    // Properties the node lacks are added; those it has are kept, as the person may have
    // changed them.
    let current = space.properties(&id)?;
    let mut missing: Properties = props
        .into_iter()
        .filter(|(k, _)| k == keys::HIGHLIGHTS || !current.contains_key(k))
        .collect();
    if let Some(last) = last {
        let newer = current
            .get(keys::LAST_HIGHLIGHT)
            .is_none_or(|before| last > *before);
        if newer {
            missing.insert(keys::LAST_HIGHLIGHT.into(), last);
        }
    }
    let mut all_tags: BTreeSet<String> = node.tags.iter().cloned().collect();
    let tags_grew = tags.iter().any(|t| all_tags.insert(t.clone()));
    if added > 0 || tags_grew {
        let body = if added == 0 {
            None
        } else if node.body.trim().is_empty() {
            Some(sections.join("\n\n"))
        } else {
            Some(format!(
                "{}\n\n{}",
                node.body.trim_end(),
                sections.join("\n\n")
            ))
        };
        space.update_node(
            &id,
            NodeUpdate {
                body,
                tags: tags_grew.then(|| all_tags.into_iter().collect()),
                ..Default::default()
            },
            by,
        )?;
    }
    space.set_properties(&id, &missing, by)?;
    Ok(Imported {
        id,
        title: node.title,
        outcome: if added > 0 || tags_grew {
            Outcome::Updated
        } else {
            Outcome::Unchanged
        },
        highlights: added,
        note: None,
    })
}

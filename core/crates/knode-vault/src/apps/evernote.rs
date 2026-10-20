//! Evernote: `.enex` exports. Evernote exports one notebook per file, so the file's name is the
//! notebook, kept in [`keys::FOLDER`](super::keys::FOLDER).
//!
//! An `.enex` file is `<en-export>` with one `<note>` per note: `<title>`, `<content>` (ENML in
//! CDATA, see [`super::enml`]), `<created>` and `<updated>` (`20201231T235959Z`), `<tag>`s,
//! `<note-attributes>` (`<source-url>`, `<author>`) and `<resource>`s, each with base64
//! `<data>`, `<mime>` and `<resource-attributes><file-name>`. The body refers to a resource by
//! the MD5 hash of its data.
//!
//! An export has no id for its notes: a note is found again by its creation time and title,
//! so a note renamed in Evernote since the last import is imported as a new one. Links between
//! notes (`evernote:///view/...`) become `[[Title]]` links when their text is the title of a
//! note of the same import.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use super::enml::{self, Media};
use super::folder::stem;
use super::markdown::{self, LinkRef, Names};
use super::source::Source;
use super::write::{Batch, Blob, Draft};
use super::{ImportError, NotesReport, dates, io_error, keys};

/// A resource of a note.
#[derive(Debug, Default)]
struct Resource {
    data: Vec<u8>,
    mime: String,
    file_name: Option<String>,
}

/// A note as the file has it.
#[derive(Debug, Default)]
struct Note {
    title: String,
    content: String,
    created: Option<String>,
    updated: Option<String>,
    tags: Vec<String>,
    url: Option<String>,
    author: Option<String>,
    resources: Vec<Resource>,
}

/// A file extension for a media type.
fn extension(mime: &str) -> &str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "video/mp4" => "mp4",
        "text/plain" => "txt",
        "text/html" => "html",
        _ => "bin",
    }
}

/// Reads the notes of one `.enex` file.
fn parse(text: &str) -> Result<Vec<Note>, String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    let mut notes = Vec::new();
    let mut note: Option<Note> = None;
    let mut resource: Option<Resource> = None;
    let mut path: Vec<String> = Vec::new();
    let mut buffer = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("not valid XML at byte {}: {e}", reader.buffer_position()))?;
        match event {
            Event::Eof => break,
            Event::Start(tag) => {
                let name = tag.local_name().as_ref().to_string();
                match name.as_str() {
                    "note" => note = Some(Note::default()),
                    "resource" => resource = Some(Resource::default()),
                    _ => {}
                }
                path.push(name);
                buffer.clear();
            }
            Event::Empty(_) => buffer.clear(),
            Event::Text(t) => buffer.push_str(&enml::decode(&t.xml10_content())),
            Event::CData(t) => buffer.push_str(&t.into_inner()),
            Event::GeneralRef(r) => {
                let name = r.xml10_content();
                buffer.push_str(&enml::entity(&name).unwrap_or_else(|| format!("&{name};")));
            }
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                let value = std::mem::take(&mut buffer);
                let parent = path.last().map(String::as_str).unwrap_or("");
                if let Some(r) = resource.as_mut() {
                    match name.as_str() {
                        "data" => {
                            let clean: String =
                                value.chars().filter(|c| !c.is_whitespace()).collect();
                            r.data = base64::engine::general_purpose::STANDARD
                                .decode(clean.as_bytes())
                                .unwrap_or_default();
                        }
                        "mime" => r.mime = value.trim().to_string(),
                        "file-name" => {
                            r.file_name = Some(value.trim().to_string()).filter(|n| !n.is_empty())
                        }
                        "resource" => {
                            if let (Some(n), Some(r)) = (note.as_mut(), resource.take()) {
                                n.resources.push(r);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                let Some(n) = note.as_mut() else {
                    continue;
                };
                match (name.as_str(), parent) {
                    ("title", "note") => n.title = value.trim().to_string(),
                    ("content", "note") => n.content = value,
                    ("created", "note") => n.created = Some(value.trim().to_string()),
                    ("updated", "note") => n.updated = Some(value.trim().to_string()),
                    ("tag", "note") => n.tags.push(value.trim().to_string()),
                    ("source-url", "note-attributes") => {
                        n.url = Some(value.trim().to_string()).filter(|v| !v.is_empty())
                    }
                    ("author", "note-attributes") => {
                        n.author = Some(value.trim().to_string()).filter(|v| !v.is_empty())
                    }
                    ("note", _) => notes.extend(note.take()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(notes)
}

/// The attachments of one note, by hash, given names as the body uses them.
struct Resources<'a> {
    by_hash: BTreeMap<String, &'a Resource>,
    names: Names,
    used: BTreeMap<String, String>,
    files: Vec<(String, Blob)>,
}

impl Media for Resources<'_> {
    fn media(&mut self, hash: &str) -> Option<String> {
        if let Some(name) = self.used.get(hash) {
            return Some(markdown::embed(name));
        }
        let resource = self.by_hash.get(hash)?;
        let raw = resource
            .file_name
            .clone()
            .unwrap_or_else(|| format!("attachment.{}", extension(&resource.mime)));
        let name = self.names.take(&raw);
        self.files
            .push((name.clone(), Blob::Bytes(resource.data.clone())));
        self.used.insert(hash.to_string(), name.clone());
        Some(markdown::embed(&name))
    }
}

pub(crate) fn read(path: &Path, report: &mut NotesReport) -> Result<Batch, ImportError> {
    let mut source = Source::open(path).map_err(io_error(path))?;
    let files: Vec<String> = source
        .names()
        .iter()
        .filter(|n| n.to_lowercase().ends_with(".enex"))
        .cloned()
        .collect();
    if files.is_empty() {
        return Err(ImportError::NotRecognized {
            path: path.to_path_buf(),
            what: "an Evernote export (.enex)".into(),
        });
    }
    let mut drafts = Vec::new();
    for file in files {
        let text = source
            .read_string(&file)
            .map_err(io_error(path.join(&file)))?;
        let notes = match parse(&text) {
            Ok(notes) => notes,
            Err(message) => {
                report.skip(file.clone(), message);
                continue;
            }
        };
        let notebook = stem(&file).replace('/', "-");
        for note in &notes {
            let title = markdown::title(&note.title);
            let mut resources = Resources {
                by_hash: note
                    .resources
                    .iter()
                    .map(|r| (format!("{:x}", md5::compute(&r.data)), r))
                    .collect(),
                names: Names::default(),
                used: BTreeMap::new(),
                files: Vec::new(),
            };
            let body = enml::to_markdown(&note.content, &mut resources);
            // Resources the body does not show, such as files of a note written elsewhere.
            let mut extra = String::new();
            let hashes: Vec<String> = resources.by_hash.keys().cloned().collect();
            for hash in hashes {
                if !resources.used.contains_key(&hash)
                    && let Some(md) = resources.media(&hash)
                {
                    extra.push_str(&format!("\n\n{md}"));
                }
            }
            let created = note.created.as_deref().and_then(dates::enex);
            let mut draft = Draft {
                source_id: format!(
                    "{}:{}",
                    note.created.clone().unwrap_or_default(),
                    note.title.trim()
                ),
                title,
                body: format!("{body}{extra}"),
                tags: note.tags.iter().cloned().collect::<BTreeSet<_>>(),
                folder: Some(notebook.clone()),
                created,
                updated: note.updated.as_deref().and_then(dates::enex),
                attachments: resources.files,
                ..Draft::default()
            };
            if let Some(url) = &note.url {
                draft.properties.insert(keys::URL.into(), url.clone());
            }
            if let Some(author) = &note.author {
                draft.properties.insert(keys::AUTHOR.into(), author.clone());
            }
            drafts.push(draft);
        }
    }
    // Links between notes: by their text, when it is the title of a note of this import.
    let titles: BTreeSet<String> = drafts.iter().map(|d| d.title.clone()).collect();
    for draft in &mut drafts {
        let body = markdown::rewrite_links(&draft.body, |link: &LinkRef| {
            if !link.dest.starts_with("evernote:") && !link.dest.contains("evernote.com/shard/") {
                return None;
            }
            let title = markdown::title(&link.text);
            titles
                .contains(&title)
                .then(|| markdown::wiki(&title, &link.text))
        });
        draft.body = markdown::tidy(&body);
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
    fn parses_enex() {
        let enex = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/evernote-export4.dtd">
<en-export export-date="20261020T000000Z" application="Evernote" version="10.0">
<note><title>A &amp; B</title><content><![CDATA[<en-note><div>Hi</div></en-note>]]></content>
<created>20261020T095000Z</created><tag>work</tag><tag>two words</tag>
<note-attributes><source-url>https://example.com</source-url></note-attributes>
<resource><data encoding="base64">aGVs
bG8=</data><mime>text/plain</mime><resource-attributes><file-name>h.txt</file-name></resource-attributes></resource>
</note></en-export>"#;
        let notes = parse(enex).unwrap();
        assert_eq!(notes.len(), 1);
        let n = &notes[0];
        assert_eq!(n.title, "A & B");
        assert_eq!(n.content, "<en-note><div>Hi</div></en-note>");
        assert_eq!(n.tags, ["work", "two words"]);
        assert_eq!(n.url.as_deref(), Some("https://example.com"));
        assert_eq!(n.resources[0].data, b"hello");
        assert_eq!(n.resources[0].file_name.as_deref(), Some("h.txt"));
    }
}

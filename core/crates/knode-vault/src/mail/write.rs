//! Writing threads and people into the space.

use std::collections::{BTreeSet, HashMap};

use chrono::SecondsFormat;
use knode_format::{
    ImportedNode, MAX_ATTACHMENT_SIZE, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space,
    Timestamp, link_kinds,
};

use super::message::{self, Mail, Origin, Person};
use super::thread::Thread;
use super::{
    ImportedThread, MailError, MailOptions, MailReport, MailSource, apple, keys, kinds, mbox,
};
use crate::reading::clean_title;
use crate::reading::markdown::escape_inline;

/// The first words of the line that starts the section of a message.
const MARKER: &str = "<!-- mail ";
/// The date of a section whose message has none.
const UNDATED: &str = "undated";

/// One message of a thread's body.
#[derive(Debug, Clone)]
struct Section {
    id: String,
    /// RFC 3339 in UTC, or [`UNDATED`].
    date: String,
    text: String,
}

/// The files of the messages of a thread: by message, `(name, bytes)`.
type Files = Vec<Vec<(String, Vec<u8>)>>;

pub(crate) fn write(
    space: &mut Space,
    source: MailSource,
    threads: Vec<Thread>,
    options: &MailOptions,
    by: &Provenance,
    report: &mut MailReport,
) -> Result<(), MailError> {
    // A person is named by the first name any message gives them, not only the first message.
    let mut names: HashMap<String, String> = HashMap::new();
    for person in threads
        .iter()
        .flat_map(|t| &t.messages)
        .flat_map(Mail::people)
    {
        if let Some(name) = &person.name {
            names
                .entry(person.address.clone())
                .or_insert_with(|| name.clone());
        }
    }
    let mut people = People {
        names,
        nodes: HashMap::new(),
    };
    for thread in threads {
        write_thread(space, source, &thread, options, by, &mut people, report)?;
    }
    Ok(())
}

/// The people of the import: the name each address goes by, and the node of each one written.
struct People {
    names: HashMap<String, String>,
    nodes: HashMap<String, NodeId>,
}

fn write_thread(
    space: &mut Space,
    source: MailSource,
    thread: &Thread,
    options: &MailOptions,
    by: &Provenance,
    people: &mut People,
    report: &mut MailReport,
) -> Result<(), MailError> {
    let files = if options.attachments {
        read_files(thread, report)
    } else {
        Vec::new()
    };
    let mut names: Vec<Vec<String>> = thread
        .messages
        .iter()
        .enumerate()
        .map(|(i, mail)| match files.get(i) {
            Some(read) if !read.is_empty() => read.iter().map(|(n, _)| clean_name(n)).collect(),
            _ => mail.attachments.iter().map(|a| a.name.clone()).collect(),
        })
        .collect();
    let title = title(&thread.messages[0]);
    let existing = find(space, thread)?;

    let (node_id, created) = match existing {
        Some(id) => (id, false),
        None => {
            let sections = sections(thread, &names, &files);
            let body = join(&format!("# {title}"), &sections);
            let properties = properties(source, thread, &sections, None);
            let first = thread.messages.first().and_then(Mail::utc);
            let last = thread.messages.iter().filter_map(Mail::utc).max();
            let created_at = first
                .map(|d| Timestamp(d.timestamp_millis()))
                .unwrap_or_else(Timestamp::now);
            let updated_at = last
                .map(|d| Timestamp(d.timestamp_millis()))
                .filter(|t| *t >= created_at)
                .unwrap_or(created_at);
            let id = space.new_node_id();
            let node = space.import_node(
                ImportedNode {
                    id,
                    node: NewNode::note(title.clone(), body).with_kind(kinds::THREAD),
                    created_at,
                    updated_at,
                    created_by: by.clone(),
                    updated_by: by.clone(),
                },
                &properties,
                by,
            )?;
            (node.id, true)
        }
    };

    // Files first: the names they get in the node are the names the body links to.
    if !files.is_empty() {
        let before: BTreeSet<(String, String)> = space
            .attachments(&node_id)?
            .into_iter()
            .map(|a| (a.name, a.hash))
            .collect();
        for (i, read) in files.iter().enumerate() {
            for (j, (name, bytes)) in read.iter().enumerate() {
                if bytes.len() as u64 > MAX_ATTACHMENT_SIZE {
                    report.skip(
                        name.clone(),
                        format!(
                            "larger than {} MB, the most a space attaches",
                            MAX_ATTACHMENT_SIZE / (1024 * 1024)
                        ),
                    );
                    continue;
                }
                match space.attach(&node_id, name, bytes.as_slice(), by) {
                    Ok(attachment) => {
                        if !before.contains(&(attachment.name.clone(), attachment.hash.clone())) {
                            report.attached += 1;
                        }
                        names[i][j] = attachment.name;
                    }
                    Err(e) => report.skip(name.clone(), e.to_string()),
                }
            }
        }
    }

    let node = space
        .get_node(&node_id)?
        .ok_or(knode_format::Error::NotFound(node_id))?;
    let fresh = sections(thread, &names, &files);
    let (prefix, mut merged) = parse(&node.body);
    for section in fresh {
        match merged.iter_mut().find(|s| s.id == section.id) {
            Some(old) => *old = section,
            None => merged.push(section),
        }
    }
    merged.sort_by(|a, b| (a.date == UNDATED, &a.date).cmp(&(b.date == UNDATED, &b.date)));
    let prefix = if prefix.trim().is_empty() {
        format!("# {title}")
    } else {
        prefix
    };
    let body = join(&prefix, &merged);
    let current = space.properties(&node_id)?;
    let wanted = properties(source, thread, &merged, Some(&current));
    let changed_properties: Properties = wanted
        .into_iter()
        .filter(|(k, v)| current.get(k) != Some(v))
        .collect();
    let body_changed = body != node.body;
    if body_changed {
        space.update_node(
            &node_id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    if !changed_properties.is_empty() {
        space.set_properties(&node_id, &changed_properties, by)?;
    }

    for person in thread.messages.iter().flat_map(Mail::people) {
        let person = person_node(space, person, by, people, report)?;
        space.link(&node_id, &person, link_kinds::RELATED, by)?;
    }

    report.messages += thread.messages.len();
    let imported = ImportedThread {
        node_id,
        thread: current
            .get(keys::THREAD)
            .cloned()
            .unwrap_or_else(|| thread.key.clone()),
        title: node.title.clone(),
        messages: merged.len(),
    };
    if created {
        report.created.push(imported);
    } else if body_changed || !changed_properties.is_empty() {
        report.updated.push(imported);
    } else {
        report.unchanged += 1;
    }
    Ok(())
}

/// The thread's node from an earlier import: by its key, then by the keys of its other
/// messages, then, for a reply that names no other message, by its subject.
fn find(space: &Space, thread: &Thread) -> Result<Option<NodeId>, MailError> {
    for key in std::iter::once(&thread.key).chain(&thread.other_keys) {
        if let Some(found) = space
            .find_by_property(keys::THREAD, key)?
            .into_iter()
            .find(|n| n.kind == kinds::THREAD)
        {
            return Ok(Some(found.id));
        }
    }
    if thread.by_subject {
        if let Some(found) = space
            .find_by_property(keys::SUBJECT, &thread.subject)?
            .into_iter()
            .find(|n| n.kind == kinds::THREAD)
        {
            return Ok(Some(found.id));
        }
    }
    Ok(None)
}

/// The node of a person, found by their address or created. A node found keeps what it has,
/// except a title that is only the address, which becomes the person's name once a message
/// gives it.
fn person_node(
    space: &mut Space,
    person: &Person,
    by: &Provenance,
    people: &mut People,
    report: &mut MailReport,
) -> Result<NodeId, MailError> {
    if let Some(id) = people.nodes.get(&person.address) {
        return Ok(*id);
    }
    let name = people
        .names
        .get(&person.address)
        .map(|n| clean_title(n))
        .filter(|n| !n.is_empty());
    let found = space
        .find_by_property(keys::PERSON_EMAIL, &person.address)?
        .into_iter()
        .find(|n| n.kind == kinds::PERSON);
    let id = match found {
        Some(node) => {
            if let Some(name) = name.filter(|_| node.title == person.address) {
                space.update_node(
                    &node.id,
                    NodeUpdate {
                        title: Some(name),
                        ..Default::default()
                    },
                    by,
                )?;
            }
            node.id
        }
        None => {
            let mut properties = Properties::new();
            properties.insert(keys::PERSON_EMAIL.to_string(), person.address.clone());
            let title = name.unwrap_or_else(|| person.address.clone());
            let node = space.create_node_with_properties(
                NewNode::note(title, format!("Email: <{}>", person.address))
                    .with_kind(kinds::PERSON),
                &properties,
                by,
            )?;
            report.people += 1;
            node.id
        }
    };
    people.nodes.insert(person.address.clone(), id);
    Ok(id)
}

/// The title of a thread: the subject of its first message without `Re:` and `Fwd:`.
fn title(first: &Mail) -> String {
    let title = clean_title(&first.topic);
    if title.is_empty() {
        "No subject".to_string()
    } else {
        title
    }
}

/// A name as the space keeps it.
fn clean_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '[' | ']' | '|' | '#') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// The files of every message of the thread, read again from the mailbox.
fn read_files(thread: &Thread, report: &mut MailReport) -> Files {
    thread
        .messages
        .iter()
        .map(|mail| {
            if mail.attachments.is_empty() {
                return Vec::new();
            }
            let raw = match &mail.origin {
                Origin::Mbox { file, offset, len } => mbox::read_at(file, *offset, *len),
                Origin::Emlx(path) => apple::read_emlx(path),
                Origin::Elsewhere => return Vec::new(),
            };
            match raw {
                Ok(raw) => message::attachment_bytes(&raw),
                Err(e) => {
                    report.skip(
                        mail.subject.clone(),
                        format!("its files could not be read: {e}"),
                    );
                    Vec::new()
                }
            }
        })
        .collect()
}

/// The sections of the messages of the thread.
fn sections(thread: &Thread, names: &[Vec<String>], files: &Files) -> Vec<Section> {
    thread
        .messages
        .iter()
        .enumerate()
        .map(|(i, mail)| {
            let attached = files.get(i).is_some_and(|f| !f.is_empty());
            section(mail, &names[i], attached)
        })
        .collect()
}

fn section(mail: &Mail, names: &[String], attached: bool) -> Section {
    let date = mail
        .utc()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| UNDATED.to_string());
    let who = mail
        .from
        .first()
        .map(|p| escape_inline(p.label()))
        .unwrap_or_else(|| "Unknown sender".to_string());
    let mut text = format!("{MARKER}{} {date} -->\n## {who}", mail.id);
    if let Some(sent) = mail.date {
        text.push_str(&format!(" - {}", sent.format("%Y-%m-%d %H:%M")));
    }
    text.push_str("\n\n");
    for (label, list) in [("From", &mail.from), ("To", &mail.to), ("Cc", &mail.cc)] {
        if !list.is_empty() {
            let list: Vec<String> = list.iter().map(address).collect();
            text.push_str(&format!("- **{label}:** {}\n", list.join(", ")));
        }
    }
    if !mail.body.is_empty() {
        text.push('\n');
        text.push_str(&mail.body);
        text.push('\n');
    }
    if !names.is_empty() {
        text.push('\n');
        for (j, name) in names.iter().enumerate() {
            let image = mail.attachments.get(j).is_some_and(|a| a.image);
            let line = match (attached, image) {
                (true, true) => format!("![[{name}]]"),
                (true, false) => format!("[[{name}]]"),
                (false, _) => format!("_[attached: {}]_", name.replace(['[', ']'], "")),
            };
            text.push_str(&line);
            text.push('\n');
        }
    }
    Section {
        id: mail.id.clone(),
        date,
        text: text.trim_end().to_string(),
    }
}

fn address(person: &Person) -> String {
    match &person.name {
        Some(name) => format!("{} <{}>", escape_inline(name), person.address),
        None => format!("<{}>", person.address),
    }
}

/// The body: what comes before the first section, then the sections.
fn join(prefix: &str, sections: &[Section]) -> String {
    let mut body = prefix.trim_end().to_string();
    for section in sections {
        body.push_str("\n\n");
        body.push_str(&section.text);
    }
    body.push('\n');
    body
}

/// A body split into what comes before the first section and the sections.
fn parse(body: &str) -> (String, Vec<Section>) {
    let mut prefix = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    for line in body.lines() {
        if let Some((id, date)) = marker(line) {
            sections.push(Section {
                id,
                date,
                text: line.to_string(),
            });
        } else if let Some(section) = sections.last_mut() {
            section.text.push('\n');
            section.text.push_str(line);
        } else {
            prefix.push(line);
        }
    }
    for section in &mut sections {
        section.text = section.text.trim_end().to_string();
    }
    (prefix.join("\n").trim_end().to_string(), sections)
}

/// The id and date of a section's first line.
fn marker(line: &str) -> Option<(String, String)> {
    let inner = line.strip_prefix(MARKER)?.strip_suffix(" -->")?;
    let mut parts = inner.split(' ');
    let id = parts.next()?.to_string();
    let date = parts.next()?.to_string();
    (parts.next().is_none() && !id.is_empty()).then_some((id, date))
}

/// The properties of a thread's node with these sections.
fn properties(
    source: MailSource,
    thread: &Thread,
    sections: &[Section],
    current: Option<&Properties>,
) -> Properties {
    let mut properties = Properties::new();
    let current_key = current.and_then(|c| c.get(keys::THREAD));
    properties.insert(
        keys::THREAD.to_string(),
        current_key.cloned().unwrap_or_else(|| thread.key.clone()),
    );
    if !thread.subject.is_empty() {
        properties.insert(keys::SUBJECT.to_string(), thread.subject.clone());
    }
    properties.insert(keys::SOURCE.to_string(), source.id().to_string());
    properties.insert(keys::MESSAGES.to_string(), sections.len().to_string());
    let mut folders: BTreeSet<String> = current
        .and_then(|c| c.get(keys::FOLDERS))
        .map(|f| {
            f.split(", ")
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for mail in &thread.messages {
        folders.extend(mail.folders.iter().map(|f| f.replace(", ", " ")));
    }
    if !folders.is_empty() {
        properties.insert(
            keys::FOLDERS.to_string(),
            folders.into_iter().collect::<Vec<_>>().join(", "),
        );
    }
    let dated: Vec<&str> = sections
        .iter()
        .map(|s| s.date.as_str())
        .filter(|d| *d != UNDATED)
        .collect();
    if let (Some(first), Some(last)) = (dated.iter().min(), dated.iter().max()) {
        properties.insert(keys::STARTED_AT.to_string(), first.to_string());
        properties.insert(keys::ENDED_AT.to_string(), last.to_string());
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sections_back() {
        let body = "# Plans\n\n<!-- mail a@example.com 2026-03-01T10:00:00Z -->\n## Ada\n\nHi\n\n\
<!-- mail b@example.com undated -->\n## Bob\n\nBye\n";
        let (prefix, sections) = parse(body);
        assert_eq!(prefix, "# Plans");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].id, "a@example.com");
        assert_eq!(
            sections[0].text,
            "<!-- mail a@example.com 2026-03-01T10:00:00Z -->\n## Ada\n\nHi"
        );
        assert_eq!(sections[1].date, UNDATED);
        assert_eq!(join(&prefix, &sections), body);
        assert_eq!(marker("<!-- mail a b c -->"), None);
    }
}

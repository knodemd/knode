//! Writing threads and people into the space.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::SecondsFormat;
use knode_format::{
    ImportedNode, MAX_ATTACHMENT_SIZE, NewNode, NodeId, NodeSummary, NodeUpdate, Properties,
    Provenance, Space, Timestamp, link_kinds,
};

use super::message::{self, Mail, Origin, Person};
use super::thread::{Thread, addresses, fits_by_subject};
use super::{
    ImportedThread, MAX_RECIPIENTS, MailError, MailOptions, MailReport, MailSource, apple, keys,
    kinds, mbox,
};
use crate::reading::clean_title;
use crate::reading::markdown::escape_inline;

/// The first words of the line that starts the section of a message.
const MARKER: &str = "<!-- mail ";
/// The date of a section whose message has none.
const UNDATED: &str = "undated";

/// One message of a thread's body.
///
/// Its first line is `<!-- mail <id> <date> <lines> <digest> -->`: the message, when it was
/// sent, and how many lines Knode wrote after it, with a digest of them. While those lines are
/// as Knode wrote them, an import may write them again; once someone changed them, the section
/// is theirs and is left as it is. Lines after them (and before the next section) are always
/// someone's own, and kept.
#[derive(Debug, Clone)]
struct Section {
    id: String,
    /// RFC 3339 in UTC, or [`UNDATED`].
    date: String,
    /// The section as it is in the body, its first line included.
    text: String,
    /// Whether the lines Knode wrote are still as it wrote them.
    pristine: bool,
    /// What someone wrote after the lines Knode wrote, when they are pristine.
    after: String,
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
        left_out: HashSet::new(),
    };
    for thread in threads {
        write_thread(space, source, thread, options, by, &mut people, report)?;
    }
    report.people_left_out += people
        .left_out
        .iter()
        .filter(|a| !people.nodes.contains_key(*a))
        .count();
    Ok(())
}

/// The people of the import: the name each address goes by, the node of each one written, and
/// the addresses left out ([`people_of`]).
struct People {
    names: HashMap<String, String>,
    nodes: HashMap<String, NodeId>,
    left_out: HashSet<String>,
}

/// Who of a message becomes a person: its senders, and its recipients when it has at most
/// [`MAX_RECIPIENTS`], `Bcc` only in sent mail. The others are added to `left_out`.
fn people_of<'a>(mail: &'a Mail, left_out: &mut HashSet<String>) -> Vec<&'a Person> {
    let bcc: &[Person] = if mail.is_sent() { &mail.bcc } else { &[] };
    if !mail.is_sent() {
        left_out.extend(mail.bcc.iter().map(|p| p.address.clone()));
    }
    let recipients: Vec<&Person> = mail.to.iter().chain(&mail.cc).chain(bcc).collect();
    let mut out: Vec<&Person> = mail.from.iter().collect();
    if recipients.len() <= MAX_RECIPIENTS {
        out.extend(recipients);
    } else {
        left_out.extend(recipients.iter().map(|p| p.address.clone()));
    }
    out
}

fn write_thread(
    space: &mut Space,
    source: MailSource,
    mut thread: Thread,
    options: &MailOptions,
    by: &Provenance,
    people: &mut People,
    report: &mut MailReport,
) -> Result<(), MailError> {
    let title = title(&thread.messages[0]);
    let read = thread.messages.len();
    let found = find(space, &thread)?;
    let existing = found.first().copied();
    // A message another node of the thread already has stays there: no message is in two
    // nodes. The nodes are linked instead.
    let mut elsewhere: HashSet<String> = HashSet::new();
    let mine: HashSet<String> = match existing {
        Some(id) => ids_in(space, &id)?,
        None => HashSet::new(),
    };
    for other in found.iter().skip(1) {
        elsewhere.extend(ids_in(space, other)?);
    }
    thread
        .messages
        .retain(|m| mine.contains(&m.id) || !elsewhere.contains(&m.id));

    let files = if options.attachments {
        read_files(&thread, report)
    } else {
        Vec::new()
    };
    let mut names: Vec<Vec<String>> = thread
        .messages
        .iter()
        .enumerate()
        .map(|(i, mail)| match files.get(i) {
            Some(read) if !read.is_empty() => read.iter().map(|(n, _)| clean_name(n)).collect(),
            _ => mail
                .attachments
                .iter()
                .map(|a| clean_name(&a.name))
                .collect(),
        })
        .collect();
    // Which files the body links to: those attached now, and those the node already has.
    let mut linked: Vec<Vec<bool>> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let read = files.get(i).is_some_and(|f| !f.is_empty());
            n.iter()
                .enumerate()
                .map(|(j, _)| {
                    read && files[i]
                        .get(j)
                        .is_some_and(|(_, b)| b.len() as u64 <= MAX_ATTACHMENT_SIZE)
                })
                .collect()
        })
        .collect();

    let (node_id, created) = match existing {
        Some(id) => (id, false),
        None => {
            let sections = sections(&thread, &names, &linked);
            let body = join(&format!("# {title}"), &sections, "");
            let properties = properties(source, &thread, &sections, None);
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
    let before: Vec<(String, String)> = space
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
                linked[i][j] = false;
                continue;
            }
            match space.attach(&node_id, name, bytes.as_slice(), by) {
                Ok(attachment) => {
                    if !before.contains(&(attachment.name.clone(), attachment.hash.clone())) {
                        report.attached += 1;
                    }
                    names[i][j] = attachment.name;
                    linked[i][j] = true;
                }
                Err(e) => {
                    report.skip(name.clone(), e.to_string());
                    linked[i][j] = false;
                }
            }
        }
    }
    // Without files this time, a file the node already has stays linked.
    for (i, list) in names.iter().enumerate() {
        for (j, name) in list.iter().enumerate() {
            if !linked[i][j] && files.get(i).is_none_or(|f| f.is_empty()) {
                linked[i][j] = before.iter().any(|(n, _)| n == name);
            }
        }
    }

    let node = space
        .get_node(&node_id)?
        .ok_or(knode_format::Error::NotFound(node_id))?;
    let fresh = sections(&thread, &names, &linked);
    let (prefix, mut merged, end) = parse(&node.body);
    for section in fresh {
        match merged.iter_mut().find(|s| s.id == section.id) {
            // Written again only while it is as Knode wrote it.
            Some(old) if old.pristine => {
                let after = std::mem::take(&mut old.after);
                *old = Section { after, ..section };
            }
            Some(_) => {}
            None => merged.push(section),
        }
    }
    merged.sort_by(|a, b| (a.date == UNDATED, &a.date).cmp(&(b.date == UNDATED, &b.date)));
    let prefix = if prefix.trim().is_empty() {
        format!("# {title}")
    } else {
        prefix
    };
    let body = join(&prefix, &merged, &end);
    let current = space.properties(&node_id)?;
    let wanted = properties(source, &thread, &merged, Some(&current));
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

    for other in found.iter().skip(1) {
        space.link(&node_id, other, link_kinds::RELATED, by)?;
    }
    let mut chosen = Vec::new();
    for mail in &thread.messages {
        chosen.extend(people_of(mail, &mut people.left_out));
    }
    for person in chosen {
        let person = person_node(space, person, by, people, report)?;
        space.link(&node_id, &person, link_kinds::RELATED, by)?;
    }

    report.messages += read;
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

/// The ids of the messages in a thread's node.
fn ids_in(space: &Space, id: &NodeId) -> Result<HashSet<String>, MailError> {
    Ok(match space.get_node(id)? {
        Some(node) => parse(&node.body).1.into_iter().map(|s| s.id).collect(),
        None => HashSet::new(),
    })
}

/// The thread's nodes from earlier imports, the oldest first: by its key and the ids its
/// messages name, then, for replies that name no other message, by the subject of a thread
/// that shares one of their people and was recent enough.
fn find(space: &Space, thread: &Thread) -> Result<Vec<NodeId>, MailError> {
    let mut found: Vec<NodeSummary> = Vec::new();
    for key in std::iter::once(&thread.key).chain(&thread.other_keys) {
        for node in space.find_by_property(keys::THREAD, key)? {
            if node.kind == kinds::THREAD && !found.iter().any(|f| f.id == node.id) {
                found.push(node);
            }
        }
    }
    if found.is_empty() && thread.by_subject {
        let people: HashSet<String> = thread.messages.iter().flat_map(addresses).collect();
        let sent = thread
            .messages
            .iter()
            .filter_map(Mail::utc)
            .map(|d| d.timestamp())
            .min();
        if let Some(sent) = sent {
            for node in space.find_by_property(keys::SUBJECT, &thread.subject)? {
                if node.kind == kinds::THREAD && fits(space, &node.id, &people, sent)? {
                    found.push(node);
                    break;
                }
            }
        }
    }
    found.sort_by_key(|a| (a.created_at, a.id));
    Ok(found.into_iter().map(|n| n.id).collect())
}

/// Whether a reply without headers fits the thread's node by its people and dates.
fn fits(
    space: &Space,
    node: &NodeId,
    people: &HashSet<String>,
    sent: i64,
) -> Result<bool, MailError> {
    let date = |key: &str| -> Result<Option<i64>, MailError> {
        Ok(space
            .property(node, key)?
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(&d).ok())
            .map(|d| d.timestamp()))
    };
    let (Some(first), Some(last)) = (date(keys::STARTED_AT)?, date(keys::ENDED_AT)?) else {
        return Ok(false);
    };
    let mut thread_people = HashSet::new();
    for link in space.links_from(node)? {
        if link.kind == link_kinds::RELATED
            && let Some(address) = space.property(&link.to, keys::PERSON_EMAIL)?
        {
            thread_people.insert(address);
        }
    }
    Ok(fits_by_subject(&thread_people, first, last, people, sent))
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
fn sections(thread: &Thread, names: &[Vec<String>], linked: &[Vec<bool>]) -> Vec<Section> {
    thread
        .messages
        .iter()
        .enumerate()
        .map(|(i, mail)| section(mail, &names[i], &linked[i]))
        .collect()
}

fn section(mail: &Mail, names: &[String], linked: &[bool]) -> Section {
    let date = mail
        .utc()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| UNDATED.to_string());
    let who = mail
        .from
        .first()
        .map(|p| escape_inline(p.label()))
        .unwrap_or_else(|| "Unknown sender".to_string());
    let mut text = format!("## {who}");
    // In the sender's time, with its offset: times of one thread can be from several zones.
    if let Some(sent) = mail.date {
        text.push_str(&format!(" - {}", sent.format("%Y-%m-%d %H:%M %:z")));
    }
    text.push_str("\n\n");
    for (label, list) in [
        ("From", &mail.from),
        ("To", &mail.to),
        ("Cc", &mail.cc),
        ("Bcc", &mail.bcc),
    ] {
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
            let line = match (linked.get(j).copied().unwrap_or(false), image) {
                (true, true) => format!("![[{name}]]"),
                (true, false) => format!("[[{name}]]"),
                (false, _) => format!("_[attached: {}]_", name.replace(['[', ']'], "")),
            };
            text.push_str(&line);
            text.push('\n');
        }
    }
    let content = text.trim_end();
    Section {
        id: mail.id.clone(),
        text: format!(
            "{MARKER}{} {date} {} {} -->\n{content}",
            mail.id,
            content.lines().count(),
            digest(content)
        ),
        date,
        pristine: true,
        after: String::new(),
    }
}

/// The digest of the lines Knode wrote for a message: 16 hexadecimal digits of their SHA-256.
fn digest(content: &str) -> String {
    let lines: Vec<&str> = content.lines().map(str::trim_end).collect();
    crate::reading::sha256(lines.join("\n").as_bytes())[..16].to_string()
}

fn address(person: &Person) -> String {
    match &person.name {
        Some(name) => format!("{} <{}>", escape_inline(name), person.address),
        None => format!("<{}>", person.address),
    }
}

/// The body: what comes before the first section, the sections (each followed by what someone
/// wrote after it) and what someone wrote after the last one.
fn join(prefix: &str, sections: &[Section], end: &str) -> String {
    let mut body = prefix.trim_end().to_string();
    for section in sections {
        body.push_str("\n\n");
        body.push_str(&section.text);
        if !section.after.is_empty() {
            body.push_str("\n\n");
            body.push_str(&section.after);
        }
    }
    if !end.is_empty() {
        body.push_str("\n\n");
        body.push_str(end);
    }
    body.push('\n');
    body
}

/// A body split into what comes before the first section, the sections, and what someone wrote
/// after the last one.
fn parse(body: &str) -> (String, Vec<Section>, String) {
    let mut prefix = Vec::new();
    let mut raw: Vec<(Marker, &str, Vec<&str>)> = Vec::new();
    for line in body.lines() {
        if let Some(found) = marker(line) {
            raw.push((found, line, Vec::new()));
        } else if let Some(last) = raw.last_mut() {
            last.2.push(line);
        } else {
            prefix.push(line);
        }
    }
    let mut sections: Vec<Section> = raw
        .into_iter()
        .map(|(Marker { id, date, written }, first, lines)| {
            let pristine = written
                .as_ref()
                .is_some_and(|(n, d)| lines.len() >= *n && digest(&lines[..*n].join("\n")) == *d);
            let (mine, after) = match (&written, pristine) {
                (Some((n, _)), true) => (&lines[..*n], &lines[*n..]),
                _ => (&lines[..], &lines[lines.len()..]),
            };
            let mut text = first.to_string();
            for line in mine {
                text.push('\n');
                text.push_str(line);
            }
            Section {
                id,
                date,
                text: text.trim_end().to_string(),
                pristine,
                after: after.join("\n").trim().to_string(),
            }
        })
        .collect();
    // What follows the last section is not part of it: it stays at the end of the body.
    let end = sections
        .last_mut()
        .map(|s| std::mem::take(&mut s.after))
        .unwrap_or_default();
    (prefix.join("\n").trim_end().to_string(), sections, end)
}

/// What a section's first line says: the message's id, its date, and the number and digest of
/// the lines Knode wrote.
struct Marker {
    id: String,
    date: String,
    written: Option<(usize, String)>,
}

/// The [`Marker`] of a section's first line.
fn marker(line: &str) -> Option<Marker> {
    let inner = line.strip_prefix(MARKER)?.strip_suffix(" -->")?;
    let parts: Vec<&str> = inner.split(' ').collect();
    match parts.as_slice() {
        [id, date] if !id.is_empty() => Some(Marker {
            id: id.to_string(),
            date: date.to_string(),
            written: None,
        }),
        [id, date, lines, digest] if !id.is_empty() => Some(Marker {
            id: id.to_string(),
            date: date.to_string(),
            written: lines.parse().ok().map(|n| (n, digest.to_string())),
        }),
        _ => None,
    }
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

    fn written(id: &str, date: &str, content: &str) -> String {
        format!(
            "{MARKER}{id} {date} {} {} -->\n{content}",
            content.lines().count(),
            digest(content)
        )
    }

    #[test]
    fn reads_sections_back_and_tells_edits() {
        let a = written("a@example.com", "2026-03-01T10:00:00Z", "## Ada\n\nHi");
        let b = written("b@example.com", "undated", "## Bob\n\nBye");
        let body = format!("# Plans\n\n{a}\n\nA note of mine.\n\n{b}\n\nAt the end.\n");
        let (prefix, sections, end) = parse(&body);
        assert_eq!(prefix, "# Plans");
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].id, "a@example.com");
        assert!(sections[0].pristine);
        assert_eq!(sections[0].text, a);
        assert_eq!(sections[0].after, "A note of mine.");
        assert_eq!(sections[1].date, UNDATED);
        assert_eq!(sections[1].after, "");
        assert_eq!(end, "At the end.");
        assert_eq!(join(&prefix, &sections, &end), body);

        let edited = body.replace("\nHi", "\nHi, and more");
        let (_, sections, _) = parse(&edited);
        assert!(!sections[0].pristine);
        assert!(sections[0].text.ends_with("A note of mine."));
        assert!(marker("<!-- mail a b c -->").is_none());
    }
}

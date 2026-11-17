//! Slack workspace exports: Workspace settings > Import/Export Data > Export, a `.zip` of JSON
//! files.
//!
//! At the top, `users.json` (every member: `id`, `name`, `real_name`, `profile`),
//! `channels.json` (public channels: `id`, `name`), and, in exports that include them,
//! `groups.json` (private channels), `mpims.json` (group messages) and `dms.json` (direct
//! messages, named by their `members`). Each conversation has a folder (its name, or its id for
//! direct messages) of one file per day, `YYYY-MM-DD.json`, with the messages of that day:
//! `user`, `text` in Slack's markup, `ts` (seconds since 1970, which is also the message's id),
//! `thread_ts` for replies and the messages that start a thread, `subtype` for what Slack wrote
//! itself (`channel_join`...), and `files`.
//!
//! A standard export holds only links to files, which need a Slack sign-in to open: they are
//! kept as links. Exports made by tools that download the files too, into
//! `__uploads/<file id>/<name>`, have them attached.
//!
//! Times are shown in the time zone of this computer.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::LazyLock;

use chrono::{Local, NaiveDateTime, TimeZone};
use knode_format::{ChangeOp, NewNode, NodeId, Properties, link_kinds};
use regex::Regex;
use serde::Deserialize;

use super::files::{Files, file_name, join, parent};
use super::model::{Chat, ChatMessage, Media};
use super::render::escape_text;
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportError};
use crate::exports::Imported;

/// The kind of the node of a Slack member.
pub(crate) const PERSON_KIND: &str = "person";

/// The properties of a Slack member's node.
pub mod person_keys {
    /// The member's id, after its team's when the export names it: `T01/U01`.
    pub const USER: &str = "slack.user";
    /// The member's handle, without `@`.
    pub const HANDLE: &str = "slack.handle";
}

/// Subtypes of what Slack wrote itself, shown as lines without a sender.
const SYSTEM_SUBTYPES: &[&str] = &[
    "channel_join",
    "channel_leave",
    "channel_topic",
    "channel_purpose",
    "channel_name",
    "channel_archive",
    "channel_unarchive",
    "group_join",
    "group_leave",
    "group_topic",
    "group_purpose",
    "group_name",
    "group_archive",
    "group_unarchive",
    "pinned_item",
    "unpinned_item",
];

/// `<...>` in Slack's markup: a mention, a channel, a link or a special word.
static ANGLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>\n]+)>").expect("a regex"));

/// The folder of a Slack export, if the files are one: where `users.json` and `channels.json`
/// are, in `root` (see [`super::in_root`]), the shallowest.
pub(crate) fn locate(files: &mut Files, root: Option<&str>) -> Option<String> {
    if files.single.is_some() {
        return None;
    }
    files
        .names()
        .iter()
        .filter(|n| super::in_root(n, root) && file_name(n) == "channels.json")
        .map(|n| parent(n).to_string())
        .filter(|dir| files.contains(&join(dir, "users.json")))
        .min_by_key(|dir| (dir.matches('/').count(), dir.len()))
}

#[derive(Debug, Deserialize)]
struct User {
    id: String,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    profile: Option<Profile>,
}

#[derive(Debug, Default, Deserialize)]
struct Profile {
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Conversation {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    bot_profile: Option<BotProfile>,
    #[serde(default)]
    user_profile: Option<Profile>,
    #[serde(default)]
    text: String,
    ts: String,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    files: Vec<SlackFile>,
    /// Secrets replaced in `text`.
    #[serde(skip)]
    redacted: usize,
}

#[derive(Debug, Deserialize)]
struct BotProfile {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackFile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    permalink: Option<String>,
    #[serde(default)]
    url_private: Option<String>,
}

/// One kind of conversation in the export: its list, what it is, and whether its folders are
/// named by id.
const LISTS: [(&str, &str, bool); 4] = [
    ("channels.json", "channel", false),
    ("groups.json", "private-channel", false),
    ("mpims.json", "group", false),
    ("dms.json", "personal", true),
];

fn read_json<T: for<'de> Deserialize<'de>>(
    files: &mut Files,
    name: &str,
) -> Result<T, ChatImportError> {
    let reader = files.open_file(name)?;
    serde_json::from_reader(reader).map_err(|error| {
        if error.is_io() {
            ChatImportError::Io(error.into())
        } else {
            ChatImportError::Json {
                file: name.to_string(),
                error,
            }
        }
    })
}

/// What the import knows of the workspace: its members and channels by id.
struct Workspace {
    users: BTreeMap<String, User>,
    /// Channel names by id.
    channels: BTreeMap<String, String>,
    /// The node titles of the members written so far, by id.
    people: BTreeMap<String, String>,
    /// Their nodes, by id.
    nodes: BTreeMap<String, NodeId>,
    /// The files of the export downloaded into `__uploads/<file id>/<name>`, by id and name.
    uploads: BTreeMap<(String, String), String>,
}

impl Workspace {
    /// What a member is called: the real name in the profile, else the one on the account,
    /// else the display name, else the handle. The real name comes first because it is the
    /// full name a person node is about, the same in every workspace, while a display name is
    /// often a nickname that is changed more often. A member's node, once written, is found by
    /// its id and keeps its title.
    fn name(&self, id: &str) -> Option<String> {
        let user = self.users.get(id)?;
        let profile = user.profile.as_ref();
        [
            profile.and_then(|p| p.real_name.clone()),
            user.real_name.clone(),
            profile.and_then(|p| p.display_name.clone()),
            Some(user.name.clone()),
        ]
        .into_iter()
        .flatten()
        .map(|n| clean_title(&n))
        .find(|n| !n.is_empty())
    }
}

/// Reads the export in `dir` and writes it: the members who wrote or are mentioned, one node
/// per channel and one per thread, linked `part-of` its channel.
pub(crate) fn import(
    files: &mut Files,
    dir: &str,
    writer: &mut ChatWriter<'_>,
) -> Result<(), ChatImportError> {
    let users: Vec<User> = read_json(files, &join(dir, "users.json"))?;
    let mut workspace = Workspace {
        users: users.into_iter().map(|u| (u.id.clone(), u)).collect(),
        channels: BTreeMap::new(),
        people: BTreeMap::new(),
        nodes: BTreeMap::new(),
        uploads: uploads(files),
    };
    let mut conversations = Vec::new();
    for (list, kind, by_id) in LISTS {
        let path = join(dir, list);
        if !files.contains(&path) {
            continue;
        }
        let read: Vec<Conversation> = read_json(files, &path)?;
        for c in read {
            if let Some(name) = &c.name {
                workspace.channels.insert(c.id.clone(), name.clone());
            }
            let folder = if by_id {
                c.id.clone()
            } else {
                c.name.clone().unwrap_or_else(|| c.id.clone())
            };
            conversations.push((c, kind, join(dir, &folder)));
        }
    }

    // Every day file of every conversation, read first, so the members can be written before
    // the channels that mention them.
    let mut read = Vec::new();
    for (conversation, kind, folder) in conversations {
        // The names are sorted, so the files under a folder are found where they start.
        let prefix = format!("{folder}/");
        let days: Vec<String> = files
            .names()
            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
            .take_while(|n| n.starts_with(&prefix))
            .filter(|n| parent(n) == folder && is_day_file(file_name(n)))
            .cloned()
            .collect();
        let mut messages: Vec<Message> = Vec::new();
        for day in days {
            let mut more: Vec<Message> = read_json(files, &day)?;
            messages.append(&mut more);
        }
        for m in &mut messages {
            let (text, n) = crate::redact::redact_tokens(&m.text);
            if n > 0 {
                m.text = text;
                m.redacted = n;
            }
        }
        messages.sort_by_key(|m| mark(&m.ts));
        read.push((conversation, kind, messages));
    }

    let mut wanted = BTreeSet::new();
    for (_, _, messages) in &read {
        for m in messages {
            wanted.extend(m.user.clone());
            for c in ANGLE.captures_iter(&m.text) {
                if let Some(id) = c[1].strip_prefix('@') {
                    wanted.insert(id.split('|').next().unwrap_or(id).to_string());
                }
            }
        }
    }
    write_people(&mut workspace, &wanted, writer)?;

    for (conversation, kind, messages) in read {
        write_conversation(&workspace, &conversation, kind, messages, files, writer)?;
    }
    Ok(())
}

fn is_day_file(name: &str) -> bool {
    name.len() == "2026-03-04.json".len()
        && name.ends_with(".json")
        && name[..10].chars().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// A `ts` as text that sorts the way the messages do.
fn mark(ts: &str) -> String {
    format!("{ts:0>20}")
}

/// The time of a `ts`, in the time zone of this computer.
fn time(ts: &str) -> Option<NaiveDateTime> {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let secs: i64 = secs.parse().ok()?;
    let micros: u32 = format!("{frac:0<6}").get(..6)?.parse().ok()?;
    Local
        .timestamp_opt(secs, micros * 1000)
        .single()
        .map(|t| t.naive_local())
}

/// Writes a node for every member in `wanted` the export knows, or finds the one written
/// before, and keeps its title for links.
fn write_people(
    workspace: &mut Workspace,
    wanted: &BTreeSet<String>,
    writer: &mut ChatWriter<'_>,
) -> Result<(), ChatImportError> {
    // Two members with the same name get their handle after it.
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for id in wanted {
        if let Some(name) = workspace.name(id) {
            *by_name.entry(name.to_lowercase()).or_insert(0) += 1;
        }
    }
    for id in wanted {
        let Some(user) = workspace.users.get(id) else {
            continue;
        };
        let Some(mut title) = workspace.name(id) else {
            continue;
        };
        if by_name.get(&title.to_lowercase()).is_some_and(|n| *n > 1) {
            title = format!("{title} ({})", clean_title(&user.name));
        }
        let key = match &user.team_id {
            Some(team) => format!("{team}/{id}"),
            None => id.clone(),
        };
        let node_id;
        let existing = writer
            .space
            .find_by_property(person_keys::USER, &key)?
            .into_iter()
            .next();
        match existing {
            Some(found) => {
                // Linked by the title it has now, which the person may have changed.
                let node = writer.space.get_node(&found.id)?;
                if let Some(node) = node {
                    title = node.title;
                }
                node_id = found.id;
            }
            None => {
                let mut props = Properties::new();
                props.insert(super::keys::APP.into(), ChatFormat::Slack.id().into());
                props.insert(person_keys::USER.into(), key.clone());
                props.insert(person_keys::HANDLE.into(), user.name.clone());
                let body = format!("Slack member `@{}`.\n", user.name.replace('`', ""));
                let node = writer.space.create_node_with_properties(
                    NewNode::note(title.clone(), body).with_kind(PERSON_KIND),
                    &props,
                    &writer.by,
                )?;
                writer.report.people.push(Imported {
                    node_id: node.id,
                    op: ChangeOp::Create,
                    export_id: key,
                    title: node.title.clone(),
                });
                title = node.title;
                node_id = node.id;
            }
        }
        workspace.people.insert(id.clone(), title);
        workspace.nodes.insert(id.clone(), node_id);
    }
    Ok(())
}

fn write_conversation(
    workspace: &Workspace,
    conversation: &Conversation,
    kind: &str,
    messages: Vec<Message>,
    files: &mut Files,
    writer: &mut ChatWriter<'_>,
) -> Result<(), ChatImportError> {
    let name = match (&conversation.name, kind) {
        (Some(name), _) if kind != "group" => clean_title(name),
        _ => {
            let names: Vec<String> = conversation
                .members
                .iter()
                .map(|m| workspace.name(m).unwrap_or_else(|| m.clone()))
                .collect();
            if names.is_empty() {
                conversation
                    .name
                    .clone()
                    .unwrap_or_else(|| conversation.id.clone())
            } else {
                names.join(", ")
            }
        }
    };
    let title = channel_title(&name);
    let mut channel = Chat::new(ChatFormat::Slack, conversation.id.clone(), title);
    channel.kind = kind.to_string();

    // Replies by the thread they belong to.
    let mut replies: BTreeMap<String, Vec<&Message>> = BTreeMap::new();
    for m in &messages {
        if let Some(thread) = &m.thread_ts
            && thread != &m.ts
        {
            replies.entry(thread.clone()).or_default().push(m);
        }
    }
    let mut threads = Vec::new();
    let mut writers: BTreeSet<String> = BTreeSet::new();
    let mut started: BTreeSet<&str> = BTreeSet::new();
    for m in &messages {
        if m.thread_ts.as_ref().is_some_and(|t| t != &m.ts) {
            continue;
        }
        started.insert(m.ts.as_str());
        let Some(mut message) = message(workspace, m, &name) else {
            continue;
        };
        if let Some(thread_replies) = replies.get(&m.ts) {
            let snippet = crate::text::one_line(&plain(workspace, &m.text), 40);
            let thread_title = clean_title(&format!(
                "{} - {} ({})",
                channel.title,
                if snippet.is_empty() {
                    "thread"
                } else {
                    &snippet
                },
                message.time.format("%Y-%m-%d %H:%M")
            ))
            .replace('#', "");
            message.notes.push(format!("Thread: [[{thread_title}]]"));
            let mut thread = Chat::new(
                ChatFormat::Slack,
                format!("{}/{}", conversation.id, m.ts),
                thread_title,
            );
            thread.kind = "thread".into();
            let thread_writers: BTreeSet<String> = std::iter::once(m)
                .chain(thread_replies.iter().copied())
                .filter_map(|r| r.user.clone())
                .collect();
            let mut first = message.clone();
            first.notes.clear();
            // Its secrets are counted once, with the channel.
            first.redacted = 0;
            thread.messages.push(first);
            for reply in thread_replies {
                if let Some(reply) = self::message(workspace, reply, &name) {
                    thread.messages.push(reply);
                }
            }
            thread.collect_participants();
            threads.push((thread, thread_writers));
        }
        writers.extend(m.user.clone());
        channel.messages.push(message);
    }
    channel.collect_participants();

    // Replies whose first message is not in the export, such as an export that starts after
    // it: kept in their thread's node, which says its start is missing.
    let mut orphans = 0;
    for (thread_ts, thread_replies) in &replies {
        if started.contains(thread_ts.as_str()) {
            continue;
        }
        let Some(start) = time(thread_ts) else {
            continue;
        };
        let thread_title = clean_title(&format!(
            "{} - thread ({})",
            channel.title,
            start.format("%Y-%m-%d %H:%M")
        ))
        .replace('#', "");
        let mut thread = Chat::new(
            ChatFormat::Slack,
            format!("{}/{thread_ts}", conversation.id),
            thread_title,
        );
        thread.kind = "thread".into();
        thread.messages.push(ChatMessage::new(
            format!("start-{thread_ts}"),
            mark(thread_ts),
            start,
            None,
            "The message that started this thread is not in this export.",
        ));
        for reply in thread_replies {
            if let Some(reply) = self::message(workspace, reply, &name) {
                thread.messages.push(reply);
                orphans += 1;
            }
        }
        thread.collect_participants();
        let thread_writers = thread_replies
            .iter()
            .filter_map(|r| r.user.clone())
            .collect();
        threads.push((thread, thread_writers));
    }
    if orphans > 0 {
        writer.report.notes.push(format!(
            "{}: {orphans} {} to threads that start before this export kept in the threads' nodes.",
            channel.title,
            if orphans == 1 { "reply" } else { "replies" }
        ));
    }

    // Every thread is linked `part-of` its channel, also one first written by a later import,
    // whose first message in the channel cannot say so any more.
    let channel_id = writer.write(&channel, files)?;
    if let Some(channel_id) = &channel_id {
        relate(workspace, writer, channel_id, &writers)?;
    }
    for (thread, thread_writers) in threads {
        if let Some(thread_id) = writer.write(&thread, files)? {
            if let Some(channel_id) = &channel_id {
                writer
                    .space
                    .link(&thread_id, channel_id, link_kinds::PART_OF, &writer.by)?;
            }
            relate(workspace, writer, &thread_id, &thread_writers)?;
        }
    }
    Ok(())
}

/// Links a chat's node `related` to the nodes of the members who wrote in it.
fn relate(
    workspace: &Workspace,
    writer: &mut ChatWriter<'_>,
    chat: &NodeId,
    writers: &BTreeSet<String>,
) -> Result<(), ChatImportError> {
    for id in writers {
        if let Some(person) = workspace.nodes.get(id) {
            writer
                .space
                .link(chat, person, link_kinds::RELATED, &writer.by)?;
        }
    }
    Ok(())
}

/// The title of a channel's node.
fn channel_title(name: &str) -> String {
    format!("Slack - {}", name.trim_start_matches('#'))
}

fn message(workspace: &Workspace, m: &Message, channel: &str) -> Option<ChatMessage> {
    let time = time(&m.ts)?;
    let system = m
        .subtype
        .as_deref()
        .is_some_and(|s| SYSTEM_SUBTYPES.contains(&s));
    let sender = if system {
        None
    } else {
        Some(
            m.user
                .as_deref()
                .and_then(|u| workspace.name(u))
                .or_else(|| {
                    m.user_profile
                        .as_ref()
                        .and_then(|p| p.real_name.clone().or(p.display_name.clone()))
                })
                .or_else(|| m.bot_profile.as_ref().and_then(|b| b.name.clone()))
                .or_else(|| m.username.clone())
                .or_else(|| m.user.clone())
                .unwrap_or_else(|| channel.to_string()),
        )
    };
    let mut message = ChatMessage::new(
        m.ts.clone(),
        mark(&m.ts),
        time,
        sender,
        markdown(workspace, &m.text),
    );
    message.media = m
        .files
        .iter()
        .map(|f| media(f, &workspace.uploads))
        .collect();
    message.redacted = m.redacted;
    Some(message)
}

fn media(file: &SlackFile, uploads: &BTreeMap<(String, String), String>) -> Media {
    let name = file
        .name
        .clone()
        .or_else(|| file.title.clone())
        .unwrap_or_else(|| file.id.clone());
    if matches!(file.mode.as_deref(), Some("tombstone" | "hidden_by_limit")) {
        return Media::Omitted {
            what: "deleted or hidden file".into(),
        };
    }
    // Tools that download the files keep them in `__uploads/<id>/<name>`.
    let entry = uploads.get(&(file.id.clone(), name.clone())).cloned();
    // A file's private URL carries a token to open it (`?t=xoxe-...`): only the address is
    // kept.
    let url = file
        .permalink
        .as_deref()
        .or(file.url_private.as_deref())
        .map(without_query);
    match (entry, url) {
        (Some(entry), _) => Media::File { name, entry },
        (None, Some(url)) => Media::Link { name, url },
        (None, None) => Media::Missing { name },
    }
}

/// The files that tools which download them keep in `__uploads/<file id>/<name>`, by id and
/// name, the first in the export's order when two are at the same id and name: read once, so
/// a file sent is found without going through every name of the export.
fn uploads(files: &Files) -> BTreeMap<(String, String), String> {
    let mut uploads = BTreeMap::new();
    for n in files.names().iter().filter(|n| n.contains("__uploads/")) {
        let key = (file_name(parent(n)).to_string(), file_name(n).to_string());
        uploads.entry(key).or_insert_with(|| n.clone());
    }
    uploads
}

/// A URL without its query string and fragment.
fn without_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

/// A name a title can hold: no `[`, `]` or `|`, spaces collapsed.
fn clean_title(name: &str) -> String {
    name.replace(['[', ']', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Slack's `&lt;`, `&gt;` and `&amp;` as the characters.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The text without markup, for a thread's title: mentions as names.
fn plain(workspace: &Workspace, text: &str) -> String {
    let replaced = ANGLE.replace_all(text, |c: &regex::Captures<'_>| {
        let inner = &c[1];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        if let Some(id) = target.strip_prefix('@') {
            return workspace
                .people
                .get(id)
                .cloned()
                .or_else(|| label.map(str::to_string))
                .unwrap_or_else(|| format!("@{id}"));
        }
        if let Some(id) = target.strip_prefix('#') {
            return label
                .map(str::to_string)
                .or_else(|| workspace.channels.get(id).cloned())
                .unwrap_or_default();
        }
        label.unwrap_or(target).trim_start_matches('!').to_string()
    });
    unescape(&replaced).replace(['*', '_', '~', '`'], "")
}

/// Slack's markup as Markdown: mentions as `[[links]]` to the members' nodes, channels as
/// links to their nodes, `<url|label>` as Markdown links, `*bold*`, `_italic_` and `~struck~`
/// in Markdown's way. Code is kept as it is.
fn markdown(workspace: &Workspace, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Outside code: fenced blocks first, then inline spans.
    for (i, block) in text.split("```").enumerate() {
        if i % 2 == 1 {
            let body = unescape(block.trim_matches('\n'));
            out.push_str(&format!("\n```\n{body}\n```\n"));
            continue;
        }
        for (j, span) in block.split('`').enumerate() {
            if j % 2 == 1 {
                out.push('`');
                out.push_str(&unescape(span));
                out.push('`');
            } else {
                out.push_str(&inline(workspace, span));
            }
        }
    }
    out.trim_matches('\n').to_string()
}

fn inline(workspace: &Workspace, text: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for c in ANGLE.captures_iter(text) {
        let whole = c.get(0).expect("the match");
        out.push_str(&emphasis(&escape_text(&unescape(
            &text[last..whole.start()],
        ))));
        out.push_str(&token(workspace, &c[1]));
        last = whole.end();
    }
    out.push_str(&emphasis(&escape_text(&unescape(&text[last..]))));
    out
}

/// One `<...>` of Slack's markup as Markdown.
fn token(workspace: &Workspace, inner: &str) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(unescape(l))),
        None => (inner, None),
    };
    if let Some(id) = target.strip_prefix('@') {
        return match workspace.people.get(id) {
            Some(title) => format!("[[{title}]]"),
            None => format!("@{}", label.unwrap_or_else(|| id.to_string())),
        };
    }
    if let Some(id) = target.strip_prefix('#') {
        let name = label.or_else(|| workspace.channels.get(id).cloned());
        return match name {
            Some(name) => format!("[[{}]]", channel_title(&clean_title(&name))),
            None => "#channel".into(),
        };
    }
    if let Some(special) = target.strip_prefix('!') {
        return match label {
            Some(label) => escape_text(&label),
            None => format!("@{}", special.split('^').next().unwrap_or(special)),
        };
    }
    let url = unescape(target).replace(['<', '>'], "");
    match label {
        Some(label) if label != url && label != url.trim_start_matches("mailto:") => {
            format!("[{}](<{url}>)", escape_text(&label).replace(['[', ']'], ""))
        }
        Some(label) if url.starts_with("mailto:") => format!("<{label}>"),
        _ => format!("<{url}>"),
    }
}

/// `*bold*`, `_italic_` and `~struck~` in Markdown: `**bold**`, `*italic*`, `~~struck~~`.
/// A marker counts only at the edge of a word, as in Slack.
fn emphasis(text: &str) -> String {
    let text = pairs(text, '*', "**");
    let text = pairs(&text, '_', "*");
    pairs(&text, '~', "~~")
}

/// The longest run of text read as one `*bold*`, `_italic_` or `~struck~`, as in Slack, which
/// does not read a marker that far away as the closing one.
const MAX_EMPHASIS: usize = 1000;

/// `marker` pairs in `text` as `markdown`. An opening marker pairs with the first closing one
/// after it on its line, found in a table made in one pass from the end of each line, so every
/// character is looked at a fixed number of times however many markers stay unpaired.
fn pairs(text: &str, marker: char, markdown: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let edge = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric());
    // `next[k]`: the first closing marker at `k` or after it on the same line.
    let mut next: Vec<Option<usize>> = vec![None; chars.len() + 1];
    for j in (0..chars.len()).rev() {
        next[j] = if chars[j] == '\n' {
            None
        } else if chars[j] == marker
            && j > 0
            && !chars[j - 1].is_whitespace()
            && edge(chars.get(j + 1))
        {
            Some(j)
        } else {
            next[j + 1]
        };
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == marker
            && edge(i.checked_sub(1).and_then(|p| chars.get(p)))
            && chars
                .get(i + 1)
                .is_some_and(|c| !c.is_whitespace() && *c != marker)
            && let Some(close) = next[i + 1].filter(|&j| j - i <= MAX_EMPHASIS)
        {
            out.push_str(markdown);
            out.extend(&chars[i + 1..close]);
            out.push_str(markdown);
            i = close + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let mut people = BTreeMap::new();
        people.insert("U01".to_string(), "Ada".to_string());
        let mut channels = BTreeMap::new();
        channels.insert("C01".to_string(), "general".to_string());
        Workspace {
            users: BTreeMap::new(),
            channels,
            people,
            nodes: BTreeMap::new(),
            uploads: BTreeMap::new(),
        }
    }

    #[test]
    fn turns_markup_into_markdown() {
        let w = workspace();
        assert_eq!(
            markdown(&w, "Hi <@U01>, see <#C01> and <#C02|random>"),
            "Hi [[Ada]], see [[Slack - general]] and [[Slack - random]]"
        );
        assert_eq!(
            markdown(&w, "<@U09> <!here> <!subteam^S1|@design> <!date^1|today>"),
            "@U09 @here @design today"
        );
        assert_eq!(
            markdown(
                &w,
                "Read <https://example.com/a?b=1&amp;c=2|the plan> or <https://example.com>"
            ),
            "Read [the plan](<https://example.com/a?b=1&c=2>) or <https://example.com>"
        );
        assert_eq!(
            markdown(&w, "<mailto:ada@example.com|ada@example.com>"),
            "<ada@example.com>"
        );
        assert_eq!(
            markdown(&w, "*bold* _italic_ ~gone~ snake_case_name 2*3*4"),
            "**bold** *italic* ~~gone~~ snake_case_name 2*3*4"
        );
        assert_eq!(
            markdown(&w, "Run `a *b*` then\n```\nx &lt; y\n```\ndone"),
            "Run `a *b*` then\n\n```\nx < y\n```\n\ndone"
        );
        assert_eq!(markdown(&w, "1 &lt; 2 [[x]]"), "1 < 2 \\[\\[x\\]\\]");
    }

    #[test]
    fn emphasis_stays_linear() {
        // 100,000 markers that never close: a search of up to MAX_EMPHASIS characters after
        // each took seconds; read in one pass, it takes milliseconds, even in a debug build.
        let many = "*a ".repeat(100_000);
        let started = std::time::Instant::now();
        assert_eq!(emphasis(&many), many);
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
        assert_eq!(emphasis("*one\ntwo*"), "*one\ntwo*");
    }

    #[test]
    fn emphasis_pairs_as_before() {
        assert_eq!(emphasis("*a* *b*"), "**a** **b**");
        assert_eq!(emphasis("*a *b*"), "**a *b**");
        assert_eq!(emphasis("x*a* *b"), "x*a* *b");
        assert_eq!(emphasis("*a\nb* c*"), "*a\nb* c*");
        assert_eq!(emphasis("_snake_case_ ~x~"), "*snake_case* ~~x~~");
        let far = format!("*{}*", "a".repeat(MAX_EMPHASIS));
        assert_eq!(emphasis(&far), far);
        let near = format!("*{}*", "a".repeat(MAX_EMPHASIS - 2));
        assert_eq!(
            emphasis(&near),
            format!("**{}**", "a".repeat(MAX_EMPHASIS - 2))
        );
    }

    #[test]
    fn names_members_by_their_real_name_first() {
        let user = |id: &str, real: Option<&str>, display: Option<&str>| User {
            id: id.into(),
            team_id: None,
            name: format!("handle{id}"),
            real_name: None,
            profile: Some(Profile {
                real_name: real.map(str::to_string),
                display_name: display.map(str::to_string),
            }),
        };
        let mut w = workspace();
        for u in [
            user("U1", Some("Ada Lovelace"), Some("ada")),
            user("U2", None, Some("grace")),
            user("U3", Some(""), Some("")),
        ] {
            w.users.insert(u.id.clone(), u);
        }
        assert_eq!(w.name("U1").as_deref(), Some("Ada Lovelace"));
        assert_eq!(w.name("U2").as_deref(), Some("grace"));
        assert_eq!(w.name("U3").as_deref(), Some("handleU3"));
    }

    #[test]
    fn reads_timestamps_and_day_files() {
        let t = time("1772625600.000200").unwrap();
        let expected = Local.timestamp_opt(1_772_625_600, 200_000).unwrap();
        assert_eq!(t, expected.naive_local());
        assert!(mark("1772625600.000200") < mark("1772625600.000300"));
        assert!(is_day_file("2026-03-04.json"));
        assert!(!is_day_file("channels.json"));
    }
}

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
/// are, the shallowest.
pub(crate) fn locate(files: &mut Files) -> Option<String> {
    if files.single.is_some() {
        return None;
    }
    files
        .names()
        .iter()
        .filter(|n| file_name(n) == "channels.json")
        .map(|n| parent(n).to_string())
        .filter(|dir| files.contains(&join(dir, "users.json")))
        .min_by_key(|dir| (dir.matches('/').count(), dir.len()))
}

/// A member of the workspace, as `users.json` of an export and `users.list` of the Web API
/// have it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct User {
    pub id: String,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub profile: Option<Profile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Profile {
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// A channel, private channel, group message or direct message.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Conversation {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
}

/// A message, as the day files of an export and `conversations.history` and
/// `conversations.replies` of the Web API have it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Message {
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    bot_profile: Option<BotProfile>,
    #[serde(default)]
    user_profile: Option<Profile>,
    #[serde(default)]
    pub text: String,
    pub ts: String,
    #[serde(default)]
    pub thread_ts: Option<String>,
    /// How many replies its thread has, for the message that starts one.
    #[serde(default)]
    pub reply_count: Option<u64>,
    /// The `ts` of the last reply of its thread, for the message that starts one.
    #[serde(default)]
    pub latest_reply: Option<String>,
    #[serde(default)]
    files: Vec<SlackFile>,
}

impl Message {
    /// Whether it is a reply in a thread rather than a message of the channel.
    pub fn is_reply(&self) -> bool {
        self.thread_ts.as_ref().is_some_and(|t| t != &self.ts)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct BotProfile {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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
pub(crate) struct Workspace {
    users: BTreeMap<String, User>,
    /// Channel names by id.
    channels: BTreeMap<String, String>,
    /// The node titles of the members written so far, by id.
    people: BTreeMap<String, String>,
    /// Their nodes, by id.
    nodes: BTreeMap<String, NodeId>,
}

impl Workspace {
    /// The workspace with these members and these channel names by id.
    pub fn new(users: impl IntoIterator<Item = User>, channels: BTreeMap<String, String>) -> Self {
        Workspace {
            users: users.into_iter().map(|u| (u.id.clone(), u)).collect(),
            channels,
            people: BTreeMap::new(),
            nodes: BTreeMap::new(),
        }
    }

    /// What a member is called: the display name chosen in the profile, else the real name,
    /// else the handle.
    pub fn name(&self, id: &str) -> Option<String> {
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
    let mut workspace = Workspace::new(users, BTreeMap::new());
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
        let days: Vec<String> = files
            .names()
            .iter()
            .filter(|n| parent(n) == folder && is_day_file(file_name(n)))
            .cloned()
            .collect();
        let mut messages: Vec<Message> = Vec::new();
        for day in days {
            let mut more: Vec<Message> = read_json(files, &day)?;
            messages.append(&mut more);
        }
        messages.sort_by_key(|m| mark(&m.ts));
        read.push((conversation, kind, messages));
    }

    let wanted = wanted(read.iter().flat_map(|(_, _, messages)| messages));
    write_people(&mut workspace, &wanted, writer)?;

    let threaded = BTreeSet::new();
    for (conversation, kind, messages) in read {
        write_conversation(
            &workspace,
            &conversation,
            kind,
            messages,
            &threaded,
            files,
            writer,
        )?;
    }
    Ok(())
}

/// The members who wrote or are mentioned in `messages`.
pub(crate) fn wanted<'a>(messages: impl IntoIterator<Item = &'a Message>) -> BTreeSet<String> {
    let mut wanted = BTreeSet::new();
    for m in messages {
        wanted.extend(m.user.clone());
        for c in ANGLE.captures_iter(&m.text) {
            if let Some(id) = c[1].strip_prefix('@') {
                wanted.insert(id.split('|').next().unwrap_or(id).to_string());
            }
        }
    }
    wanted
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
pub(crate) fn mark(ts: &str) -> String {
    format!("{ts:0>20}")
}

/// The time of a `ts`, in the time zone of this computer.
pub(crate) fn time(ts: &str) -> Option<NaiveDateTime> {
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
pub(crate) fn write_people(
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

/// Writes a conversation: its node, with the messages that are not replies, and a node for
/// every thread whose replies are among `messages`, linked `part-of` it. A message whose `ts`
/// is in `threaded` starts a thread whose replies are written later: it links to the thread's
/// node all the same.
pub(crate) fn write_conversation(
    workspace: &Workspace,
    conversation: &Conversation,
    kind: &str,
    messages: Vec<Message>,
    threaded: &BTreeSet<String>,
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
        let Some(mut message) = message(workspace, m, &name, files) else {
            continue;
        };
        if !replies.contains_key(&m.ts) && threaded.contains(&m.ts) {
            let thread_title = thread_title(workspace, &channel.title, m, &message);
            message.notes.push(format!("Thread: [[{thread_title}]]"));
        }
        if let Some(thread_replies) = replies.get(&m.ts) {
            let thread_title = thread_title(workspace, &channel.title, m, &message);
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
            thread.messages.push(first);
            for reply in thread_replies {
                if let Some(reply) = self::message(workspace, reply, &name, files) {
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
            if let Some(reply) = self::message(workspace, reply, &name, files) {
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

/// The title of the node of the thread `m` starts in the channel titled `channel`.
fn thread_title(
    workspace: &Workspace,
    channel: &str,
    m: &Message,
    message: &ChatMessage,
) -> String {
    let snippet = crate::text::one_line(&plain(workspace, &m.text), 40);
    clean_title(&format!(
        "{} - {} ({})",
        channel,
        if snippet.is_empty() {
            "thread"
        } else {
            &snippet
        },
        message.time.format("%Y-%m-%d %H:%M")
    ))
    .replace('#', "")
}

/// The title of a channel's node.
pub(crate) fn channel_title(name: &str) -> String {
    format!("Slack - {}", name.trim_start_matches('#'))
}

fn message(
    workspace: &Workspace,
    m: &Message,
    channel: &str,
    files: &Files,
) -> Option<ChatMessage> {
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
    message.media = m.files.iter().map(|f| media(f, files)).collect();
    Some(message)
}

fn media(file: &SlackFile, files: &Files) -> Media {
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
    let entry = files
        .names()
        .iter()
        .find(|n| {
            file_name(n) == name && file_name(parent(n)) == file.id && n.contains("__uploads/")
        })
        .cloned();
    match (entry, file.permalink.clone().or(file.url_private.clone())) {
        (Some(entry), _) => Media::File { name, entry },
        (None, Some(url)) => Media::Link { name, url },
        (None, None) => Media::Missing { name },
    }
}

/// A name a title can hold: no `[`, `]` or `|`, spaces collapsed.
pub(crate) fn clean_title(name: &str) -> String {
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

/// The longest run of text read as one `*bold*`, `_italic_` or `~struck~`: the search for
/// the closing marker stops there, and at the end of the line, so it stays linear in the text.
const MAX_EMPHASIS: usize = 1000;

fn pairs(text: &str, marker: char, markdown: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let edge = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == marker
            && edge(i.checked_sub(1).and_then(|p| chars.get(p)))
            && chars
                .get(i + 1)
                .is_some_and(|c| !c.is_whitespace() && *c != marker)
            && let Some(close) = (i + 1..chars.len().min(i + 1 + MAX_EMPHASIS))
                .take_while(|&j| chars[j] != '\n')
                .find(|&j| {
                    chars[j] == marker && !chars[j - 1].is_whitespace() && edge(chars.get(j + 1))
                })
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
        let many = "*a ".repeat(100_000);
        let started = std::time::Instant::now();
        assert_eq!(emphasis(&many), many);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(emphasis("*one\ntwo*"), "*one\ntwo*");
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

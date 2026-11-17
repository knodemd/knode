//! Writing a [`Chat`] into a space: one `chat` node per chat, found again by its app and id,
//! with only the messages after the last one imported added to it.

use std::collections::{BTreeMap, BTreeSet};

use knode_format::{ChangeOp, NewNode, NodeId, NodeUpdate, Properties, Provenance, Space};

use super::files::Files;
use super::model::{Chat, ChatMessage, Media};
use super::render::{Shown, append, insert_late};
use super::{CHAT_KIND, ChatFormat, ChatImportError, ChatImportReport, keys};
use crate::exports::Imported;

/// Writes chats into a space, counting what it does in a report.
pub(crate) struct ChatWriter<'a> {
    pub space: &'a mut Space,
    pub by: Provenance,
    pub report: ChatImportReport,
}

/// Where the last import of a chat stopped.
struct Mark {
    last: String,
    keys: BTreeSet<String>,
}

impl ChatWriter<'_> {
    /// The node of a chat imported before: the one with its JID, when it has one, else the one
    /// with its id that has no other JID (a chat from an export, which carries none).
    pub fn find(&self, chat: &Chat) -> Result<Option<NodeId>, ChatImportError> {
        let Some(jid) = &chat.jid else {
            return self.find_id(chat.app, &chat.id);
        };
        for summary in self.space.find_by_property(keys::JID, jid)? {
            if self.space.property(&summary.id, keys::APP)?.as_deref() == Some(chat.app.id()) {
                return Ok(Some(summary.id));
            }
        }
        for summary in self.space.find_by_property(keys::ID, &chat.id)? {
            if self.space.property(&summary.id, keys::APP)?.as_deref() == Some(chat.app.id())
                && self.space.property(&summary.id, keys::JID)?.is_none()
            {
                return Ok(Some(summary.id));
            }
        }
        Ok(None)
    }

    /// The node of the chat `id` of `app` imported before.
    pub fn find_id(&self, app: ChatFormat, id: &str) -> Result<Option<NodeId>, ChatImportError> {
        for summary in self.space.find_by_property(keys::ID, id)? {
            if self.space.property(&summary.id, keys::APP)?.as_deref() == Some(app.id()) {
                return Ok(Some(summary.id));
            }
        }
        Ok(None)
    }

    /// Writes a chat and returns its node, or `None` for a chat with no messages that was never
    /// imported.
    pub fn write(
        &mut self,
        chat: &Chat,
        files: &mut Files,
    ) -> Result<Option<NodeId>, ChatImportError> {
        let existing = self.find(chat)?;
        let mark = match existing {
            Some(id) => self.mark(&id)?,
            None => None,
        };
        // Messages that say where they arrived in the app (`seq`) are new past the last one
        // written; the others, and every message of a node no app wrote to yet, past the last
        // mark (an export's messages, or those an export already brought in).
        let last_seq = match existing {
            Some(id) => self
                .space
                .property(&id, keys::LAST_SEQ)?
                .and_then(|s| s.parse::<i64>().ok()),
            None => None,
        };
        let max_seq = chat.messages.iter().filter_map(|m| m.seq).max();
        let new: Vec<&ChatMessage> = chat
            .messages
            .iter()
            .filter(|m| match (last_seq, m.seq) {
                (Some(last), Some(seq)) => seq > last,
                _ => match &mark {
                    None => true,
                    Some(mark) => {
                        m.mark > mark.last
                            || (m.mark == mark.last
                                && !mark.keys.contains(&m.key)
                                && !m.export_key.as_ref().is_some_and(|k| mark.keys.contains(k)))
                    }
                },
            })
            .collect();
        let seq_after = match (last_seq, max_seq) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let imported = |node_id, op| Imported {
            node_id,
            op,
            export_id: chat.id.clone(),
            title: chat.title.clone(),
        };

        let Some(id) = existing else {
            if new.is_empty() {
                self.report.empty += 1;
                return Ok(None);
            }
            let id = self.create(chat, &new, files)?;
            self.count_redacted(chat, &new);
            self.report.chats.push(imported(id, ChangeOp::Create));
            return Ok(Some(id));
        };
        // Messages before where the last import stopped are not added: most were imported then.
        // Those beyond the count imported then were not, and are reported. (Messages known by
        // their place in the app were written before when they are left out.)
        let by_mark = |m: &ChatMessage| last_seq.is_none() || m.seq.is_none();
        let before = chat.messages.iter().filter(|m| by_mark(m)).count()
            - new.iter().filter(|m| by_mark(m)).count();
        let imported_before = self
            .space
            .property(&id, keys::MESSAGES)?
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        if before > imported_before {
            let n = before - imported_before;
            self.report.skip(
                chat.title.clone(),
                format!(
                    "{n} {} older than the last one imported {} not added",
                    if n == 1 { "message" } else { "messages" },
                    if n == 1 { "was" } else { "were" }
                ),
            );
        }
        if new.is_empty() {
            if let Some(seq) = seq_after
                && Some(seq) != last_seq
            {
                self.space
                    .set_property(&id, keys::LAST_SEQ, &seq.to_string(), &self.by)?;
            }
            if let Some(jid) = &chat.jid
                && self.space.property(&id, keys::JID)?.as_deref() != Some(jid.as_str())
            {
                self.space.set_property(&id, keys::JID, jid, &self.by)?;
            }
            for (key, value) in &chat.properties {
                if self.space.property(&id, key)?.as_deref() != Some(value.as_str()) {
                    self.space.set_property(&id, key, value, &self.by)?;
                }
            }
            self.report.unchanged += 1;
            return Ok(Some(id));
        }
        // A message that arrived after later ones (sent offline, say) goes into its own day;
        // the others are added at the end.
        let (on_time, late): (Vec<&ChatMessage>, Vec<&ChatMessage>) = new
            .iter()
            .copied()
            .partition(|m| mark.as_ref().is_none_or(|mark| m.mark >= mark.last));
        let mut placed = Vec::with_capacity(on_time.len());
        for message in &on_time {
            let shown = self.attach_all(&id, &message.media, files, &chat.title)?;
            placed.push((*message, shown));
        }
        let mut placed_late = Vec::with_capacity(late.len());
        for message in &late {
            let shown = self.attach_all(&id, &message.media, files, &chat.title)?;
            placed_late.push((*message, shown));
        }
        let current = self
            .space
            .get_node(&id)?
            .ok_or(knode_format::Error::NotFound(id))?;
        let mut body = append(&current.body, &placed);
        if !placed_late.is_empty() {
            body = insert_late(&body, &mut placed_late);
        }
        self.space.update_node(
            &id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            &self.by,
        )?;
        let total = imported_before + new.len();
        let started = self.space.property(&id, keys::STARTED_AT)?;
        let mut props = properties(chat, &on_time, mark.as_ref(), total, started);
        if let Some(seq) = seq_after {
            props.insert(keys::LAST_SEQ.into(), seq.to_string());
        }
        // Who wrote before stays: a connector hands over only the messages that are new.
        if let Some(before) = self.space.property(&id, keys::PARTICIPANTS)? {
            let mut everyone: Vec<String> = before
                .split(", ")
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            for p in &chat.participants {
                if !everyone.contains(p) {
                    everyone.push(p.clone());
                }
            }
            props.insert(keys::PARTICIPANTS.into(), everyone.join(", "));
        }
        self.space.set_properties(&id, &props, &self.by)?;
        self.report.messages += new.len();
        self.count_redacted(chat, &new);
        self.report.chats.push(imported(id, ChangeOp::Update));
        Ok(Some(id))
    }

    /// Counts the secrets replaced in the messages written. A message written twice, such as
    /// the first message of a Slack thread, in its channel's node and again in the thread's
    /// (whose id starts with the channel's and a `/`), is counted once.
    fn count_redacted(&mut self, chat: &Chat, written: &[&ChatMessage]) {
        let root = chat.id.split('/').next().unwrap_or(&chat.id);
        for m in written.iter().filter(|m| m.redacted > 0) {
            if self
                .report
                .redacted_seen
                .insert(format!("{}\n{root}\n{}", chat.app.id(), m.key))
            {
                self.report.redacted += m.redacted;
            }
        }
    }

    fn mark(&self, id: &NodeId) -> Result<Option<Mark>, ChatImportError> {
        let Some(last) = self.space.property(id, keys::LAST)? else {
            return Ok(None);
        };
        let keys = self
            .space
            .property(id, keys::LAST_KEYS)?
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(Some(Mark { last, keys }))
    }

    /// Creates the node of a chat imported for the first time. The body names its files as
    /// they will be attached; when one ends up under another name, or not at all, the body is
    /// written again to say so.
    fn create(
        &mut self,
        chat: &Chat,
        new: &[&ChatMessage],
        files: &mut Files,
    ) -> Result<NodeId, ChatImportError> {
        let mut planned: BTreeMap<String, String> = BTreeMap::new();
        let mut placed = Vec::with_capacity(new.len());
        for message in new {
            let shown = message
                .media
                .iter()
                .map(|media| match media {
                    Media::File { name, entry } => {
                        Shown::Attached(plan_name(&mut planned, name, entry))
                    }
                    other => self.shown_without_file(other),
                })
                .collect::<Vec<_>>();
            placed.push((*message, shown));
        }
        let body = append("", &placed);
        let mut props = properties(chat, new, None, new.len(), None);
        if let Some(seq) = chat.messages.iter().filter_map(|m| m.seq).max() {
            props.insert(keys::LAST_SEQ.into(), seq.to_string());
        }
        let node = self.space.create_node_with_properties(
            NewNode::note(chat.title.clone(), body).with_kind(CHAT_KIND),
            &props,
            &self.by,
        )?;
        let id = node.id;

        let mut changed = false;
        for (message, shown) in &mut placed {
            for (media, slot) in message.media.iter().zip(shown.iter_mut()) {
                let Media::File { name, entry } = media else {
                    continue;
                };
                let Shown::Attached(wanted) = slot.clone() else {
                    continue;
                };
                let result = self.attach_one(&id, &wanted, name, entry, files, &chat.title)?;
                if result != Shown::Attached(wanted) {
                    *slot = result;
                    changed = true;
                }
            }
        }
        if changed {
            self.space.update_node(
                &id,
                NodeUpdate {
                    body: Some(append("", &placed)),
                    ..Default::default()
                },
                &self.by,
            )?;
        }
        self.report.messages += new.len();
        Ok(id)
    }

    fn attach_all(
        &mut self,
        id: &NodeId,
        media: &[Media],
        files: &mut Files,
        chat: &str,
    ) -> Result<Vec<Shown>, ChatImportError> {
        let mut shown = Vec::with_capacity(media.len());
        for item in media {
            shown.push(match item {
                Media::File { name, entry } => {
                    self.attach_one(id, &clean_name(name), name, entry, files, chat)?
                }
                other => self.shown_without_file(other),
            });
        }
        Ok(shown)
    }

    /// Attaches one file of the export under `wanted`, and says how it is shown. A file that
    /// cannot be read or attached is reported and shown as not attached; only a failing space
    /// stops the import.
    fn attach_one(
        &mut self,
        id: &NodeId,
        wanted: &str,
        name: &str,
        entry: &str,
        files: &mut Files,
        chat: &str,
    ) -> Result<Shown, ChatImportError> {
        let not_attached = |reason: String| Shown::NotAttached {
            name: name.to_string(),
            reason,
        };
        let item = format!("{chat} ({name})");
        let reader = match files.open_file(entry) {
            Ok(reader) => reader,
            Err(e) => {
                let reason = format!("the file could not be read: {e}");
                self.report.skip(item, reason.clone());
                return Ok(not_attached(reason));
            }
        };
        match self.space.attach(id, wanted, reader, &self.by) {
            Ok(attachment) => {
                self.report.attached += 1;
                Ok(Shown::Attached(attachment.name))
            }
            Err(knode_format::Error::Invalid(reason)) => {
                self.report.skip(item, reason.clone());
                Ok(not_attached(reason))
            }
            Err(knode_format::Error::Io(e)) => {
                let reason = format!("the file could not be read: {e}");
                self.report.skip(item, reason.clone());
                Ok(not_attached(reason))
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl ChatWriter<'_> {
    /// How a file that is not in the export is shown; one missing or left out is counted.
    fn shown_without_file(&mut self, media: &Media) -> Shown {
        if matches!(
            media,
            Media::Missing { .. }
                | Media::Omitted { .. }
                | Media::NotAttached { .. }
                | Media::NotOnDevice { .. }
        ) {
            self.report.missing += 1;
        }
        shown(media)
    }
}

fn shown(media: &Media) -> Shown {
    match media {
        Media::File { name, .. } => Shown::Attached(name.clone()),
        Media::Missing { name } => Shown::Missing(name.clone()),
        Media::Omitted { what } => Shown::Omitted(what.clone()),
        Media::NotOnDevice { name } => Shown::NotOnDevice(name.clone()),
        Media::Link { name, url } => Shown::Link {
            name: name.clone(),
            url: url.clone(),
        },
        Media::NotAttached { name, reason } => Shown::NotAttached {
            name: name.clone(),
            reason: reason.clone(),
        },
    }
}

/// The name a file will be attached under in a new node: its cleaned name, or with ` 2`,
/// ` 3`... before the extension when another file of the chat took it.
fn plan_name(planned: &mut BTreeMap<String, String>, name: &str, entry: &str) -> String {
    let base = clean_name(name);
    let mut chosen = base.clone();
    let mut n = 2;
    while let Some(taken) = planned.get(&chosen) {
        if taken == entry {
            return chosen;
        }
        chosen = with_suffix(&base, &format!(" {n}"));
        n += 1;
    }
    planned.insert(chosen.clone(), entry.to_string());
    chosen
}

/// A name as the space keeps it: `/ \ [ ] | #` become `-` (see `Space::attach`).
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | '[' | ']' | '|' | '#' => '-',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned.to_string()
    }
}

fn with_suffix(name: &str, suffix: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{suffix}{}", name[..dot].trim_end(), &name[dot..]),
        _ => format!("{name}{suffix}"),
    }
}

/// The properties of a chat's node after `new` was added to what `previous` stopped at.
fn properties(
    chat: &Chat,
    new: &[&ChatMessage],
    previous: Option<&Mark>,
    total: usize,
    started: Option<String>,
) -> Properties {
    let mut props = Properties::new();
    props.insert(keys::APP.into(), chat.app.id().into());
    props.insert(keys::ID.into(), chat.id.clone());
    if let Some(jid) = &chat.jid {
        props.insert(keys::JID.into(), jid.clone());
    }
    if !chat.kind.is_empty() {
        props.insert(keys::TYPE.into(), chat.kind.clone());
    }
    if !chat.participants.is_empty() {
        props.insert(keys::PARTICIPANTS.into(), chat.participants.join(", "));
    }
    let time = |m: &ChatMessage| m.time.format("%Y-%m-%dT%H:%M:%S").to_string();
    let started = started.or_else(|| new.first().map(|m| time(m)));
    if let Some(started) = started {
        props.insert(keys::STARTED_AT.into(), started);
    }
    if let Some(last) = new.iter().copied().max_by(|a, b| a.mark.cmp(&b.mark)) {
        props.insert(keys::ENDED_AT.into(), time(last));
        let mut at_last: BTreeSet<String> = new
            .iter()
            .filter(|m| m.mark == last.mark)
            .flat_map(|m| std::iter::once(m.key.clone()).chain(m.export_key.clone()))
            .collect();
        if let Some(previous) = previous
            && previous.last == last.mark
        {
            at_last.extend(previous.keys.iter().cloned());
        }
        props.insert(keys::LAST.into(), last.mark.clone());
        props.insert(
            keys::LAST_KEYS.into(),
            at_last.into_iter().collect::<Vec<_>>().join(" "),
        );
    }
    props.insert(keys::MESSAGES.into(), total.to_string());
    props.extend(chat.properties.clone());
    props
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_unique_names() {
        let mut planned = BTreeMap::new();
        assert_eq!(
            plan_name(&mut planned, "image.png", "a/image.png"),
            "image.png"
        );
        assert_eq!(
            plan_name(&mut planned, "image.png", "a/image.png"),
            "image.png"
        );
        assert_eq!(
            plan_name(&mut planned, "image.png", "b/image.png"),
            "image 2.png"
        );
        assert_eq!(plan_name(&mut planned, "a#b.txt", "c"), "a-b.txt");
        assert_eq!(with_suffix("notes", " 2"), "notes 2");
    }
}

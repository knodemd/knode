//! The shape every chat export is read into before it is written: a [`Chat`] with its
//! participants and [`ChatMessage`]s, each with a sender, a time, Markdown text and [`Media`].

use chrono::NaiveDateTime;
use knode_format::Properties;

use super::ChatFormat;

/// One chat, channel or thread, as an export holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chat {
    /// The app the export comes from.
    pub app: ChatFormat,
    /// An id for the chat that stays the same from one export to the next, unique within the
    /// app.
    pub id: String,
    /// The chat's own id in the app, when the source has one (a WhatsApp JID). A chat is found
    /// by it first, and by [`id`](Self::id) when no node has it.
    pub jid: Option<String>,
    /// The title of its node.
    pub title: String,
    /// What it is: `personal`, `group`, `channel`, `thread`...
    pub kind: String,
    /// The people who wrote in it, in the order they first did.
    pub participants: Vec<String>,
    /// The messages, oldest first.
    pub messages: Vec<ChatMessage>,
    /// More properties of its node, such as how its dates are read.
    pub properties: Properties,
}

impl Chat {
    /// A chat with no participants or messages yet.
    pub fn new(app: ChatFormat, id: impl Into<String>, title: impl Into<String>) -> Self {
        Chat {
            app,
            id: id.into(),
            jid: None,
            title: title.into(),
            kind: String::new(),
            participants: Vec::new(),
            messages: Vec::new(),
            properties: Properties::new(),
        }
    }

    /// Adds everyone who sent a message to the participants, once each.
    pub fn collect_participants(&mut self) {
        for message in &self.messages {
            if let Some(sender) = &message.sender
                && !self.participants.contains(sender)
            {
                self.participants.push(sender.clone());
            }
        }
    }
}

/// One message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    /// Tells the message apart from every other one in the chat, the same in every export:
    /// the app's message id, or a hash of what the message says when the app has none.
    pub key: String,
    /// The key an export of the chat gives the same message, when it is read from somewhere
    /// else (an app's database): a message an export already brought in is not added again.
    pub export_key: Option<String>,
    /// Where the message arrived in the app, when the app numbers them in that order (WhatsApp
    /// for Mac's `Z_PK`): a message with a higher number is new even when it was sent before
    /// the last one written, and it goes into its own day.
    pub seq: Option<i64>,
    /// Where the message is in the chat: text that sorts the way the messages do, such as the
    /// time. Importing again adds only the messages after the last one imported.
    pub mark: String,
    /// When it was sent, in the time of the export.
    pub time: NaiveDateTime,
    /// Who sent it; `None` for what the app itself wrote, such as "Ada added Grace".
    pub sender: Option<String>,
    /// The text, in Markdown. Text from the export that is not Markdown goes through
    /// [`escape_text`](super::render::escape_text) first.
    pub text: String,
    /// Files sent with it.
    pub media: Vec<Media>,
    /// The message it answers, as people read it: "Ada at 09:15: Shall we...".
    pub reply_to: Option<String>,
    /// Lines shown after it, in Markdown, such as a link to its thread.
    pub notes: Vec<String>,
    /// How many secrets were replaced in it with `[redacted]`, counted when it is written.
    pub redacted: usize,
}

impl ChatMessage {
    /// A message with only a time and text.
    pub fn new(
        key: impl Into<String>,
        mark: impl Into<String>,
        time: NaiveDateTime,
        sender: Option<String>,
        text: impl Into<String>,
    ) -> Self {
        ChatMessage {
            key: key.into(),
            export_key: None,
            seq: None,
            mark: mark.into(),
            time,
            sender,
            text: text.into(),
            media: Vec::new(),
            reply_to: None,
            notes: Vec::new(),
            redacted: 0,
        }
    }
}

/// A file sent in a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Media {
    /// In the export, at `entry`: attached to the chat's node under `name`.
    File {
        /// Its file name.
        name: String,
        /// Its path inside the export.
        entry: String,
    },
    /// Named by the chat, but not in the export.
    Missing {
        /// Its file name, or what it was ("photo").
        name: String,
    },
    /// Known, but not attached, and why: such as a file sent in iMessage that is no longer on
    /// the Mac.
    NotAttached {
        /// Its file name.
        name: String,
        /// Why: "not on this Mac".
        reason: String,
    },
    /// Left out when the chat was exported, such as a WhatsApp export without media.
    Omitted {
        /// What it was, as the export says: "image", "media"...
        what: String,
    },
    /// Read from an app on this device, which does not have the file: never downloaded, or
    /// removed to save space.
    NotOnDevice {
        /// Its file name, or what it was ("photo").
        name: String,
    },
    /// Read live from a service by a connector, which did not download it: too large, media
    /// downloads off, or something it never downloads (a contact, a poll...).
    NotDownloaded {
        /// Its file name, or what it was ("contact").
        name: String,
        /// Why, when there is more to say: "larger than 20 MB".
        reason: Option<String>,
    },
    /// Only a link to it is in the export, such as a file on Slack.
    Link {
        /// Its file name.
        name: String,
        /// Where it is.
        url: String,
    },
}

/// A 64-bit FNV-1a hash of `parts`, as 16 hexadecimal digits: the same in every build, so a
/// key made from it today finds the same message tomorrow.
pub(crate) fn stable_hash(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain([0u8]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable() {
        assert_eq!(stable_hash(&[]), "cbf29ce484222325");
        assert_eq!(stable_hash(&["a", "b"]), stable_hash(&["a", "b"]));
        assert_ne!(stable_hash(&["ab", ""]), stable_hash(&["a", "b"]));
    }
}

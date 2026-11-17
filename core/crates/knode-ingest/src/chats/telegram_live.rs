//! Telegram through its official API, as a connector: [`TelegramConnector`].
//!
//! The connector signs in once to the person's account (phone number, the code Telegram sends,
//! and the password when the account has two-step verification) and keeps the session only in
//! the system keychain, through [`Secrets`]. Then, while Knode Service runs, it brings in every
//! chat but the ones the person excluded:
//!
//! - **Catch-up.** The first run after the service starts (or after the connection to Telegram
//!   was lost) lists the chats and reads, for each one with something new, the messages after
//!   the last one it brought in: the [`Cursor`] keeps the last message id of every chat. Chats
//!   with nothing new cost no request.
//! - **Live.** The network client keeps a connection open and buffers the messages Telegram
//!   pushes as they happen ([`TelegramApi::take_live`]); later runs only drain that buffer, so
//!   they cost no request at all.
//!
//! Messages are written by the Telegram Desktop export importer, as the same `chat` nodes: a
//! chat is found by the id Telegram gives it (the one the export calls `id`), and a message by
//! its id, so an export imported before and the connector share nodes and nothing is written
//! twice.
//!
//! Secret chats are end-to-end encrypted between two devices and never reach the API: they are
//! not read. Edited and deleted messages are left as they were first brought in.
//!
//! The network calls go through [`TelegramApi`], so everything here is tested against a mock.
//! Knode Service implements it with the MTProto client.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use knode_format::{ChangeOp, Provenance, Space, Timestamp};
use serde::{Deserialize, Serialize};

use super::files::Files;
use super::model::{Chat, ChatMessage, Media};
use super::render::{escape_name, escape_text};
use super::telegram::{Entity as Part, clean_name, entity_markdown};
use super::write::ChatWriter;
use super::{ChatFormat, ChatImportReport, keys};
use crate::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, Secrets,
    SignInState, SignInStep, SourceError, SourceInfo, Trigger, Written,
};

/// The id of the connector.
pub const SOURCE_ID: &str = "telegram";

/// The name of the secret that holds the session.
pub const SESSION_SECRET: &str = "session";

/// What the connector says when this build of Knode has no Telegram app id.
pub const NO_APP: &str = "this build of Knode has no Telegram app id: it was built without \
                          KNODE_TELEGRAM_API_ID and KNODE_TELEGRAM_API_HASH, and they are not set \
                          for Knode Service either";

/// The kind of a Telegram peer, which says how its id is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerKind {
    /// A person or a bot.
    User,
    /// A basic group.
    Group,
    /// A channel or a supergroup.
    Channel,
}

/// How to reach a chat through the API: its id (the bare id, without the `-100` of the Bot
/// API, as Telegram Desktop exports it) and the access hash Telegram gave the account for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChatRef {
    /// Its bare id.
    pub id: i64,
    /// Its kind.
    pub kind: PeerKind,
    /// Its access hash, when it has one.
    pub access_hash: Option<i64>,
}

/// What a chat is, as its node says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    /// With one person or bot, or Saved Messages.
    Personal,
    /// A group or supergroup.
    Group,
    /// A channel.
    Channel,
}

impl ChatKind {
    fn id(self) -> &'static str {
        match self {
            ChatKind::Personal => "personal",
            ChatKind::Group => "group",
            ChatKind::Channel => "channel",
        }
    }
}

/// A chat of the account, as the list of chats has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// How to reach it.
    pub chat: ChatRef,
    /// Its name: the person's name, the group's title, "Saved Messages".
    pub name: String,
    /// What it is.
    pub kind: ChatKind,
    /// The id of its last message.
    pub top_message: i64,
    /// When its last message was sent, in seconds since 1970.
    pub top_date: i64,
}

/// How a part of a message's text is formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Struck out.
    Strike,
    /// Inline code.
    Code,
    /// A block of code, maybe with its language.
    Pre,
    /// Text that links to `href`.
    TextLink,
    /// A URL written in the text.
    Url,
    /// A quote.
    Blockquote,
    /// Anything else (mentions, hashtags...): kept as plain text.
    Other,
}

impl EntityKind {
    /// Its name in a Telegram Desktop export.
    fn export_name(self) -> &'static str {
        match self {
            EntityKind::Bold => "bold",
            EntityKind::Italic => "italic",
            EntityKind::Strike => "strikethrough",
            EntityKind::Code => "code",
            EntityKind::Pre => "pre",
            EntityKind::TextLink => "text_link",
            EntityKind::Url => "link",
            EntityKind::Blockquote => "blockquote",
            EntityKind::Other => "plain",
        }
    }
}

/// A formatted part of a message's text. Offsets and lengths count UTF-16 code units, as the
/// API does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    /// How it is formatted.
    pub kind: EntityKind,
    /// Where it starts.
    pub offset: usize,
    /// How long it is.
    pub length: usize,
    /// Where a [`EntityKind::TextLink`] goes.
    pub href: Option<String>,
    /// The language of a [`EntityKind::Pre`].
    pub language: Option<String>,
}

/// What a message carries besides its text.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageMedia {
    /// A photo.
    Photo {
        /// Its size in bytes, when known.
        size: Option<u64>,
    },
    /// A file: a document, a voice message, a video...
    File {
        /// Its file name, when it has one.
        name: Option<String>,
        /// What it is, for people: `file`, `voice message`, `video`, `audio`.
        what: String,
        /// Its size in bytes, when known.
        size: Option<u64>,
    },
    /// A sticker: shown by its emoji, never downloaded.
    Sticker {
        /// Its emoji.
        emoji: Option<String>,
    },
    /// A place on a map.
    Location {
        /// Latitude.
        latitude: f64,
        /// Longitude.
        longitude: f64,
    },
    /// A poll and its answers, with their votes.
    Poll {
        /// The question.
        question: String,
        /// The answers and how many voted for each.
        answers: Vec<(String, u64)>,
    },
    /// Something else (a contact, a game, a web page preview...): named, not downloaded.
    Other {
        /// What it is, for people.
        what: String,
    },
}

/// A message, as the API gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Its chat.
    pub chat: ChatRef,
    /// The chat's name, when the update that brought it said it.
    pub chat_name: Option<String>,
    /// The chat's kind, when the update that brought it said it.
    pub chat_kind: Option<ChatKind>,
    /// Its id in the chat.
    pub id: i64,
    /// When it was sent, in seconds since 1970.
    pub date: i64,
    /// Who sent it, by name; `None` in a channel, where the channel writes.
    pub sender: Option<String>,
    /// For what Telegram itself wrote ("Ada added Grace"): that text. Its `text` is then empty.
    pub service: Option<String>,
    /// The text.
    pub text: String,
    /// How parts of the text are formatted.
    pub entities: Vec<Entity>,
    /// The message it answers.
    pub reply_to: Option<i64>,
    /// Whom it was forwarded from.
    pub forwarded_from: Option<String>,
    /// What it carries.
    pub media: Option<MessageMedia>,
}

/// Where in a chat's history to start reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFrom {
    /// After the message with this id (0 for the very first one).
    After(i64),
    /// From this time on, in seconds since 1970.
    Since(i64),
}

/// The messages that arrived live since the last call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    /// The new messages, in the order they arrived.
    pub messages: Vec<Message>,
    /// Whether some may have been missed (the connection was lost, or too many came at once):
    /// the connector then catches up from its cursors.
    pub gap: bool,
}

/// The account signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its name, for people.
    pub name: String,
}

/// What can go wrong talking to Telegram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// This build has no Telegram app id.
    NoApp,
    /// Telegram asked to wait this long before asking again (`FLOOD_WAIT`).
    FloodWait(Duration),
    /// The session is not signed in, or Telegram ended it.
    SignedOut,
    /// The account has a password; this is its hint.
    PasswordNeeded(Option<String>),
    /// The phone number is not one Telegram knows.
    BadPhone,
    /// The code is wrong or expired.
    BadCode,
    /// The password is wrong.
    BadPassword,
    /// No code was asked for, or it expired: start again.
    NoCodeRequested,
    /// The network, or Telegram, failed; trying again later may work.
    Network(String),
    /// Anything else.
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::NoApp => f.write_str(NO_APP),
            ApiError::FloodWait(wait) => {
                write!(f, "Telegram asked to wait {} s", wait.as_secs())
            }
            ApiError::SignedOut => f.write_str("Telegram is not signed in"),
            ApiError::PasswordNeeded(_) => f.write_str("the account has a password"),
            ApiError::BadPhone => f.write_str("Telegram does not know that phone number"),
            ApiError::BadCode => f.write_str("the code is not right, or it expired"),
            ApiError::BadPassword => f.write_str("the password is not right"),
            ApiError::NoCodeRequested => {
                f.write_str("no code was asked for, or it expired: start signing in again")
            }
            ApiError::Network(e) => write!(f, "Telegram could not be reached: {e}"),
            ApiError::Other(e) => f.write_str(e),
        }
    }
}

/// The calls the connector makes to Telegram. Knode Service implements it with an MTProto
/// client; tests with a mock. Calls block; each one should give up after a short timeout.
pub trait TelegramApi: Send + Sync {
    /// Connects with a saved session, or with a new one when `None`, and starts receiving
    /// updates once signed in. Returns whether the session is signed in.
    fn connect(&self, session: Option<&str>) -> Result<bool, ApiError>;
    /// The session to keep, once signed in. Only the keychain holds it.
    fn session(&self) -> Result<String, ApiError>;
    /// Asks Telegram to send a login code to this phone number. Returns where it was sent.
    fn request_code(&self, phone: &str) -> Result<Option<String>, ApiError>;
    /// Signs in with the code; [`ApiError::PasswordNeeded`] when the account has a password.
    fn sign_in(&self, code: &str) -> Result<Account, ApiError>;
    /// Signs in with the password of the account.
    fn check_password(&self, password: &str) -> Result<Account, ApiError>;
    /// The account signed in.
    fn me(&self) -> Result<Account, ApiError>;
    /// Signs out: Telegram ends the session.
    fn sign_out(&self) -> Result<(), ApiError>;
    /// Every chat of the account (never a secret chat).
    fn dialogs(&self) -> Result<Vec<Dialog>, ApiError>;
    /// At most `limit` messages of a chat from `from` on, oldest first.
    fn history(
        &self,
        chat: &ChatRef,
        from: HistoryFrom,
        limit: usize,
    ) -> Result<Vec<Message>, ApiError>;
    /// The messages received live since the last call.
    fn take_live(&self) -> Live;
    /// The file of a message's media, or `None` when it is larger than `max_bytes`.
    fn download(
        &self,
        chat: &ChatRef,
        message: i64,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, ApiError>;
}

/// How the connector behaves.
#[derive(Clone)]
pub struct TelegramOptions {
    /// The largest photo or file downloaded and attached, in bytes; larger ones are named in
    /// the text only. 0 downloads none. 20 MB by default.
    pub media_max_bytes: u64,
    /// The longest flood wait slept through inside a run; a longer one ends the run, and
    /// Telegram is not asked anything until it is over. 10 s by default.
    pub max_inline_wait: Duration,
    /// How often the service runs the connector: each run drains the live buffer, with no
    /// request. 10 s by default.
    pub every: Duration,
    /// How many live messages are kept in memory for the spaces that have not read them yet.
    pub buffer_limit: usize,
    /// How long the list of chats is reused before it is asked for again.
    pub dialogs_ttl: Duration,
    /// How the connector sleeps through a flood wait (tests replace it).
    pub sleep: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl Default for TelegramOptions {
    fn default() -> Self {
        TelegramOptions {
            media_max_bytes: 20 * 1024 * 1024,
            max_inline_wait: Duration::from_secs(10),
            every: Duration::from_secs(10),
            buffer_limit: 5000,
            dialogs_ttl: Duration::from_secs(600),
            sleep: Arc::new(std::thread::sleep),
        }
    }
}

impl fmt::Debug for TelegramOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramOptions")
            .field("media_max_bytes", &self.media_max_bytes)
            .field("max_inline_wait", &self.max_inline_wait)
            .field("every", &self.every)
            .field("buffer_limit", &self.buffer_limit)
            .field("dialogs_ttl", &self.dialogs_ttl)
            .finish_non_exhaustive()
    }
}

/// Where signing in is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    None,
    Code,
    Password(Option<String>),
}

struct Inner {
    connected: bool,
    account: Option<Account>,
    pending: Pending,
    /// Telegram is not asked anything before this.
    not_before: Option<Instant>,
    /// The spaces caught up since the connection was made.
    caught_up: HashSet<String>,
    /// Live messages by chat and id.
    buffer: BTreeMap<i64, BTreeMap<i64, Message>>,
    buffered: usize,
    /// The highest id dropped from the buffer of each chat.
    pruned: BTreeMap<i64, i64>,
    dialogs: Option<(Instant, Vec<Dialog>)>,
}

impl Inner {
    fn new() -> Self {
        Inner {
            connected: false,
            account: None,
            pending: Pending::None,
            not_before: None,
            caught_up: HashSet::new(),
            buffer: BTreeMap::new(),
            buffered: 0,
            pruned: BTreeMap::new(),
            dialogs: None,
        }
    }

    fn disconnected(&mut self) {
        self.connected = false;
        self.account = None;
        self.caught_up.clear();
        self.dialogs = None;
    }
}

/// The Telegram connector, over a [`TelegramApi`].
pub struct TelegramConnector<A: TelegramApi> {
    api: A,
    options: TelegramOptions,
    inner: Mutex<Inner>,
}

impl<A: TelegramApi> TelegramConnector<A> {
    /// The connector over `api`, with the default options.
    pub fn new(api: A) -> Self {
        Self::with_options(api, TelegramOptions::default())
    }

    /// The connector over `api`, with these options.
    pub fn with_options(api: A, options: TelegramOptions) -> Self {
        TelegramConnector {
            api,
            options,
            inner: Mutex::new(Inner::new()),
        }
    }

    /// The API it talks through.
    pub fn api(&self) -> &A {
        &self.api
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One call to Telegram, sleeping through a short flood wait and remembering a long one.
    fn call<T>(
        &self,
        inner: &mut Inner,
        f: impl Fn(&A) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut slept = 0;
        loop {
            if let Some(until) = inner.not_before {
                let now = Instant::now();
                if now < until {
                    return Err(ApiError::FloodWait(until - now));
                }
                inner.not_before = None;
            }
            match f(&self.api) {
                Err(ApiError::FloodWait(wait))
                    if wait <= self.options.max_inline_wait && slept < 3 =>
                {
                    (self.options.sleep)(wait);
                    slept += 1;
                }
                Err(ApiError::FloodWait(wait)) => {
                    inner.not_before = Some(Instant::now() + wait);
                    return Err(ApiError::FloodWait(wait));
                }
                Err(ApiError::SignedOut) => {
                    inner.disconnected();
                    return Err(ApiError::SignedOut);
                }
                other => return other,
            }
        }
    }

    /// Connects with the session in the keychain, if not connected yet.
    fn ensure_connected(
        &self,
        inner: &mut Inner,
        secrets: &dyn Secrets,
    ) -> Result<(), SourceError> {
        if inner.connected {
            return Ok(());
        }
        let session = secrets.get(SESSION_SECRET)?.ok_or_else(|| {
            SourceError::SignIn(
                "Telegram is not signed in: run `knode sources connect telegram`".into(),
            )
        })?;
        let signed_in = self
            .call(inner, |api| api.connect(Some(&session)))
            .map_err(source_error)?;
        if !signed_in {
            return Err(SourceError::SignIn(
                "Telegram ended the session: run `knode sources connect telegram` to sign in again"
                    .into(),
            ));
        }
        inner.connected = true;
        inner.caught_up.clear();
        Ok(())
    }

    fn dialogs(&self, inner: &mut Inner) -> Result<Vec<Dialog>, ApiError> {
        if let Some((at, list)) = &inner.dialogs
            && at.elapsed() < self.options.dialogs_ttl
        {
            return Ok(list.clone());
        }
        let list = self.call(inner, |api| api.dialogs())?;
        inner.dialogs = Some((Instant::now(), list.clone()));
        Ok(list)
    }

    /// Adds what arrived live to the buffer, dropping the oldest when it is full.
    fn take_live(&self, inner: &mut Inner) {
        let live = self.api.take_live();
        if live.gap {
            // What was missed shows in the chats' last messages: list them again.
            inner.caught_up.clear();
            inner.dialogs = None;
        }
        for message in live.messages {
            let chat = inner.buffer.entry(message.chat.id).or_default();
            if chat.insert(message.id, message).is_none() {
                inner.buffered += 1;
            }
        }
        while inner.buffered > self.options.buffer_limit.max(1) {
            let Some((&chat, _)) = inner.buffer.iter().max_by_key(|(_, m)| m.len()) else {
                break;
            };
            let messages = inner.buffer.get_mut(&chat).expect("the chat is there");
            if let Some((id, _)) = messages.pop_first() {
                let pruned = inner.pruned.entry(chat).or_insert(id);
                *pruned = (*pruned).max(id);
                inner.buffered -= 1;
            }
            if messages.is_empty() {
                inner.buffer.remove(&chat);
            }
        }
    }

    /// Reads a chat's history after `last` (or from the history chosen when there is none),
    /// at most `budget` messages. Returns them and whether there is more.
    fn read_chat(
        &self,
        inner: &mut Inner,
        request: &FetchRequest<'_>,
        chat: &ChatRef,
        last: Option<i64>,
        budget: usize,
    ) -> Result<(Vec<Message>, bool), ApiError> {
        let mut out = Vec::new();
        let mut last = last;
        loop {
            let want = (budget - out.len()).min(100);
            if want == 0 {
                return Ok((out, true));
            }
            let from = match (last, request.history) {
                (Some(id), _) => HistoryFrom::After(id),
                (None, History::Everything) => HistoryFrom::After(0),
                (None, History::FromToday) => HistoryFrom::Since(request.since.0.div_euclid(1000)),
            };
            let page = self.call(inner, |api| api.history(chat, from, want))?;
            let full = page.len() >= want;
            for message in page {
                if last.is_none_or(|l| message.id > l) {
                    last = Some(message.id);
                    out.push(message);
                }
            }
            if !full {
                return Ok((out, false));
            }
        }
    }

    fn fetch_locked(
        &self,
        inner: &mut Inner,
        request: &FetchRequest<'_>,
    ) -> Result<Fetched, SourceError> {
        self.ensure_connected(inner, request.secrets)?;
        self.take_live(inner);
        let mut cursor = ChatCursors::read(request.cursor);
        if request.cursor.is_none() {
            inner.caught_up.remove(request.space_id);
        }
        let budget = request.max_items.max(1);
        let mut found: Vec<Message> = Vec::new();
        let mut more = false;

        let dialogs = self.dialogs(inner).map_err(source_error)?;
        if !inner.caught_up.contains(request.space_id) {
            for dialog in &dialogs {
                let id = dialog.chat.id.to_string();
                if request.is_excluded(&id) {
                    continue;
                }
                let last = cursor.get(dialog.chat.id);
                if last.is_some_and(|l| dialog.top_message <= l) {
                    continue;
                }
                if last.is_none() && !request.wants(Timestamp(dialog.top_date.saturating_mul(1000)))
                {
                    continue;
                }
                let room = budget.saturating_sub(found.len());
                if room == 0 {
                    more = true;
                    break;
                }
                let (messages, rest) = self
                    .read_chat(inner, request, &dialog.chat, last, room)
                    .map_err(source_error)?;
                if let Some(top) = messages.iter().map(|m| m.id).max() {
                    cursor.set(dialog.chat.id, top);
                }
                found.extend(messages);
                if rest {
                    more = true;
                    break;
                }
            }
            if !more {
                inner.caught_up.insert(request.space_id.to_string());
            }
        } else {
            let chats: Vec<i64> = inner
                .buffer
                .keys()
                .chain(inner.pruned.keys())
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            for chat in chats {
                if request.is_excluded(&chat.to_string()) {
                    continue;
                }
                let last = cursor.get(chat);
                let room = budget.saturating_sub(found.len());
                if room == 0 {
                    more = true;
                    break;
                }
                let pruned = inner.pruned.get(&chat).copied();
                let messages: Vec<Message> = if pruned.is_some_and(|p| p > last.unwrap_or(0)) {
                    // Some of what this space has not read was dropped from the buffer.
                    let chat_ref = inner
                        .buffer
                        .get(&chat)
                        .and_then(|m| m.values().next())
                        .map(|m| m.chat)
                        .or_else(|| dialogs.iter().find(|d| d.chat.id == chat).map(|d| d.chat));
                    let Some(chat_ref) = chat_ref else { continue };
                    let (messages, rest) = self
                        .read_chat(inner, request, &chat_ref, last, room)
                        .map_err(source_error)?;
                    more |= rest;
                    messages
                } else {
                    inner
                        .buffer
                        .get(&chat)
                        .map(|m| {
                            m.values()
                                .filter(|m| last.is_none_or(|l| m.id > l))
                                .take(room)
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default()
                };
                if let Some(top) = messages.iter().map(|m| m.id).max() {
                    cursor.set(chat, top);
                }
                if found.len() + messages.len() >= budget
                    && inner
                        .buffer
                        .get(&chat)
                        .is_some_and(|m| m.keys().any(|&id| id > cursor.get(chat).unwrap_or(0)))
                {
                    more = true;
                }
                found.extend(messages);
            }
        }
        found.retain(|m| request.wants(Timestamp(m.date.saturating_mul(1000))));
        let next = cursor.write();
        if found.is_empty() {
            return Ok(Fetched::nothing(next).with_more(more));
        }
        let batch = self.batch(inner, found, &dialogs)?;
        Ok(Fetched::batch(batch, next).with_more(more))
    }

    /// Groups the messages by chat and downloads their media.
    fn batch(
        &self,
        inner: &mut Inner,
        messages: Vec<Message>,
        dialogs: &[Dialog],
    ) -> Result<TelegramBatch, SourceError> {
        let mut chats: BTreeMap<i64, ChatBatch> = BTreeMap::new();
        let mut media = MediaDir::default();
        for message in messages {
            let chat_id = message.chat.id;
            let entry = chats.entry(chat_id).or_insert_with(|| {
                let dialog = dialogs.iter().find(|d| d.chat.id == chat_id);
                ChatBatch {
                    id: chat_id,
                    name: dialog
                        .map(|d| d.name.clone())
                        .or_else(|| message.chat_name.clone())
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| format!("chat {chat_id}")),
                    kind: dialog.map(|d| d.kind).or(message.chat_kind).unwrap_or(
                        match message.chat.kind {
                            PeerKind::User => ChatKind::Personal,
                            PeerKind::Group => ChatKind::Group,
                            PeerKind::Channel => ChatKind::Channel,
                        },
                    ),
                    messages: Vec::new(),
                    files: BTreeMap::new(),
                }
            });
            if let Some(file) = self.download(inner, &message, &mut media)? {
                entry.files.insert(message.id, file);
            }
            entry.messages.push(message);
        }
        // What replies refer to, when it is not in the batch: the live buffer.
        let mut replied = BTreeMap::new();
        for chat in chats.values() {
            for message in &chat.messages {
                if let Some(reply) = message.reply_to
                    && !chat.messages.iter().any(|m| m.id == reply)
                    && let Some(found) = inner.buffer.get(&chat.id).and_then(|m| m.get(&reply))
                {
                    replied.insert((chat.id, reply), found.clone());
                }
            }
        }
        Ok(TelegramBatch {
            chats: chats.into_values().collect(),
            replied,
            media,
        })
    }

    /// Downloads a message's photo or file when it is small enough, into the batch's folder.
    fn download(
        &self,
        inner: &mut Inner,
        message: &Message,
        dir: &mut MediaDir,
    ) -> Result<Option<Downloaded>, SourceError> {
        let max = self.options.media_max_bytes;
        let (name, size) = match &message.media {
            Some(MessageMedia::Photo { size }) => (format!("photo_{}.jpg", message.id), *size),
            Some(MessageMedia::File { name, size, .. }) => (
                name.clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("file_{}", message.id)),
                *size,
            ),
            _ => return Ok(None),
        };
        if max == 0 || size.is_some_and(|s| s > max) {
            return Ok(Some(Downloaded::TooLarge(name)));
        }
        let bytes = match self.call(inner, |api| api.download(&message.chat, message.id, max)) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(Some(Downloaded::TooLarge(name))),
            Err(e @ (ApiError::FloodWait(_) | ApiError::SignedOut | ApiError::NoApp)) => {
                return Err(source_error(e));
            }
            Err(_) => return Ok(Some(Downloaded::Failed(name))),
        };
        let entry = format!("{}-{}", message.chat.id, message.id);
        dir.write(&entry, &bytes)?;
        Ok(Some(Downloaded::File { name, entry }))
    }

    fn signed_in(
        &self,
        inner: &mut Inner,
        secrets: &dyn Secrets,
    ) -> Result<SignInState, SourceError> {
        let session = self
            .call(inner, |api| api.session())
            .map_err(source_error)?;
        secrets.set(SESSION_SECRET, &session)?;
        let account = self.call(inner, |api| api.me()).map_err(source_error)?;
        inner.connected = true;
        inner.caught_up.clear();
        inner.pending = Pending::None;
        inner.account = Some(account.clone());
        Ok(SignInState::SignedIn {
            account: account.name,
        })
    }
}

fn source_error(error: ApiError) -> SourceError {
    match error {
        ApiError::NoApp => SourceError::Unavailable(NO_APP.into()),
        ApiError::SignedOut => SourceError::SignIn(
            "Telegram ended the session: run `knode sources connect telegram` to sign in again"
                .into(),
        ),
        ApiError::FloodWait(wait) => SourceError::Failed(format!(
            "Telegram asked Knode to wait {} s before asking again; it will",
            wait.as_secs().max(1)
        )),
        e @ (ApiError::PasswordNeeded(_)
        | ApiError::BadPhone
        | ApiError::BadCode
        | ApiError::BadPassword
        | ApiError::NoCodeRequested) => SourceError::SignIn(e.to_string()),
        ApiError::Network(e) => SourceError::Failed(format!("Telegram could not be reached: {e}")),
        ApiError::Other(e) => SourceError::Failed(e),
    }
}

impl<A: TelegramApi> Connector for TelegramConnector<A> {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            SOURCE_ID,
            "Telegram",
            "Your Telegram chats as they arrive, through Telegram's official API",
        )
        .needing([Permission::SignIn, Permission::Network])
        .excluding("chat")
    }

    fn trigger(&self) -> Trigger {
        Trigger::Interval(self.options.every)
    }

    fn containers(&self, request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let mut inner = self.lock();
        self.ensure_connected(&mut inner, request.secrets)?;
        let dialogs = self.dialogs(&mut inner).map_err(source_error)?;
        Ok(dialogs
            .into_iter()
            .map(|d| Container::new(d.chat.id.to_string(), d.name))
            .collect())
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let mut inner = self.lock();
        self.fetch_locked(&mut inner, request)
    }

    fn sign_in_state(&self, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let mut inner = self.lock();
        match inner.pending.clone() {
            Pending::Code => return Ok(SignInState::CodeSent { via: None }),
            Pending::Password(hint) => return Ok(SignInState::PasswordNeeded { hint }),
            Pending::None => {}
        }
        if let Some(account) = &inner.account {
            return Ok(SignInState::SignedIn {
                account: account.name.clone(),
            });
        }
        match self.ensure_connected(&mut inner, secrets) {
            Ok(()) => {
                let account = self
                    .call(&mut inner, |api| api.me())
                    .map_err(source_error)?;
                inner.account = Some(account.clone());
                Ok(SignInState::SignedIn {
                    account: account.name,
                })
            }
            Err(SourceError::SignIn(_)) => Ok(SignInState::SignedOut),
            Err(e) => Err(e),
        }
    }

    fn sign_in(&self, step: SignInStep, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        let mut inner = self.lock();
        match step {
            SignInStep::Start { account } => {
                let phone: String = account
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '+')
                    .collect();
                if phone.trim_start_matches('+').len() < 5 {
                    return Err(SourceError::SignIn(
                        "give the phone number of the account, in international format (+34...)"
                            .into(),
                    ));
                }
                inner.disconnected();
                inner.pending = Pending::None;
                self.call(&mut inner, |api| api.connect(None))
                    .map_err(source_error)?;
                let via = self
                    .call(&mut inner, |api| api.request_code(&phone))
                    .map_err(source_error)?;
                inner.pending = Pending::Code;
                Ok(SignInState::CodeSent { via })
            }
            SignInStep::Code { code } => {
                if inner.pending != Pending::Code {
                    return Err(SourceError::SignIn(
                        "give the phone number first: no code was asked for".into(),
                    ));
                }
                let code = code.trim().to_string();
                match self.call(&mut inner, |api| api.sign_in(&code)) {
                    Ok(_) => self.signed_in(&mut inner, secrets),
                    Err(ApiError::PasswordNeeded(hint)) => {
                        inner.pending = Pending::Password(hint.clone());
                        Ok(SignInState::PasswordNeeded { hint })
                    }
                    Err(e) => {
                        if e == ApiError::NoCodeRequested {
                            inner.pending = Pending::None;
                        }
                        Err(source_error(e))
                    }
                }
            }
            SignInStep::Password { password } => {
                if !matches!(inner.pending, Pending::Password(_)) {
                    return Err(SourceError::SignIn(
                        "no password was asked for: give the phone number first".into(),
                    ));
                }
                match self.call(&mut inner, |api| api.check_password(&password)) {
                    Ok(_) => self.signed_in(&mut inner, secrets),
                    Err(e) => Err(source_error(e)),
                }
            }
            SignInStep::Browser => Err(SourceError::SignIn(
                "Telegram signs in with the phone number and the code it sends".into(),
            )),
        }
    }

    fn sign_out(&self, secrets: &dyn Secrets) -> Result<bool, SourceError> {
        let mut inner = self.lock();
        let had = secrets.get(SESSION_SECRET)?.is_some() || inner.connected;
        if !inner.connected && had {
            // Connect with the session, so Telegram ends it too.
            if let Some(session) = secrets.get(SESSION_SECRET)? {
                let _ = self.call(&mut inner, |api| api.connect(Some(&session)));
            }
        }
        // The session is forgotten here even if Telegram cannot be reached now.
        let _ = self.call(&mut inner, |api| api.sign_out());
        secrets.delete(SESSION_SECRET)?;
        inner.disconnected();
        inner.pending = Pending::None;
        inner.buffer.clear();
        inner.buffered = 0;
        inner.pruned.clear();
        Ok(had)
    }
}

/// The last message id brought in of every chat: the connector's [`Cursor`], as JSON.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ChatCursors {
    #[serde(default)]
    chats: BTreeMap<String, i64>,
}

impl ChatCursors {
    fn read(cursor: Option<&Cursor>) -> Self {
        cursor
            .and_then(|c| serde_json::from_str(c.as_str()).ok())
            .unwrap_or_default()
    }

    fn get(&self, chat: i64) -> Option<i64> {
        self.chats.get(&chat.to_string()).copied()
    }

    fn set(&mut self, chat: i64, id: i64) {
        let entry = self.chats.entry(chat.to_string()).or_insert(id);
        *entry = (*entry).max(id);
    }

    fn write(&self) -> Cursor {
        Cursor::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// A downloaded file, or why there is none.
#[derive(Debug)]
enum Downloaded {
    File { name: String, entry: String },
    TooLarge(String),
    Failed(String),
}

struct ChatBatch {
    id: i64,
    name: String,
    kind: ChatKind,
    messages: Vec<Message>,
    files: BTreeMap<i64, Downloaded>,
}

/// A private folder for the media of one batch, removed with it.
#[derive(Default)]
struct MediaDir {
    path: Option<PathBuf>,
}

impl MediaDir {
    fn write(&mut self, entry: &str, bytes: &[u8]) -> std::io::Result<()> {
        let dir = self.ensure()?;
        fs::write(dir.join(entry), bytes)
    }

    /// The folder, made when it is first needed.
    fn ensure(&mut self) -> std::io::Result<&Path> {
        if self.path.is_none() {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "knode-telegram-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path)?;
            self.path = Some(path);
        }
        Ok(self.path.as_deref().expect("made above"))
    }
}

impl Drop for MediaDir {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// Messages read from Telegram, written by the Telegram export importer.
struct TelegramBatch {
    chats: Vec<ChatBatch>,
    replied: BTreeMap<(i64, i64), Message>,
    media: MediaDir,
}

impl Batch for TelegramBatch {
    fn len(&self) -> usize {
        self.chats.iter().map(|c| c.messages.len()).sum()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let TelegramBatch {
            chats,
            replied,
            mut media,
        } = *self;
        let mut files = Files::open(media.ensure()?)?;
        let mut writer = ChatWriter {
            space,
            by: by.clone().with_source("Telegram"),
            report: ChatImportReport::new(ChatFormat::Telegram),
        };
        for batch in &chats {
            let mut chat = to_chat(batch, &replied);
            // Who wrote before stays: only the new messages are handed over.
            if let Some(id) = writer.find(&chat).map_err(import_error)?
                && let Some(before) = writer.space.property(&id, keys::PARTICIPANTS)?
            {
                let mut everyone: Vec<String> = before
                    .split(", ")
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect();
                for p in chat.participants.drain(..) {
                    if !everyone.contains(&p) {
                        everyone.push(p);
                    }
                }
                chat.participants = everyone;
            }
            writer.write(&chat, &mut files).map_err(import_error)?;
        }
        let report = writer.report;
        let mut written = Written {
            items: report.messages,
            ..Default::default()
        };
        for chat in &report.chats {
            match chat.op {
                ChangeOp::Create => written.created += 1,
                _ => written.updated += 1,
            }
        }
        Ok(written)
    }
}

fn import_error(error: super::ChatImportError) -> SourceError {
    match error {
        super::ChatImportError::Space(e) => SourceError::Space(e),
        other => SourceError::Failed(other.to_string()),
    }
}

/// The local time of a date in seconds since 1970.
fn local_time(date: i64) -> chrono::NaiveDateTime {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(date, 0) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.naive_local(),
        chrono::LocalResult::None => chrono::DateTime::from_timestamp(date, 0)
            .map(|t| t.naive_utc())
            .unwrap_or_default(),
    }
}

/// The parts of a message's text, as an export cuts it: plain text between formatted parts.
/// Formatting inside other formatting is left out.
fn parts(text: &str, entities: &[Entity]) -> Vec<Part> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let slice = |from: usize, to: usize| String::from_utf16_lossy(&units[from..to]);
    let mut sorted: Vec<&Entity> = entities.iter().collect();
    sorted.sort_by_key(|e| (e.offset, std::cmp::Reverse(e.length)));
    let mut out = Vec::new();
    let mut at = 0;
    for e in sorted {
        let start = e.offset.min(units.len());
        let end = (e.offset + e.length).min(units.len());
        if start < at || end <= start {
            continue;
        }
        if start > at {
            out.push(plain(slice(at, start)));
        }
        out.push(Part {
            kind: e.kind.export_name().to_string(),
            text: slice(start, end),
            href: e.href.clone(),
            language: e.language.clone(),
        });
        at = end;
    }
    if at < units.len() {
        out.push(plain(slice(at, units.len())));
    }
    out
}

fn plain(text: String) -> Part {
    Part {
        kind: "plain".into(),
        text,
        href: None,
        language: None,
    }
}

/// The text of a message as Markdown, as the export importer writes it.
fn markdown(message: &Message) -> String {
    let mut text: String = parts(&message.text, &message.entities)
        .iter()
        .map(entity_markdown)
        .collect();
    if let Some(from) = &message.forwarded_from {
        text = format!("*Forwarded from {}:* {text}", escape_name(from));
    }
    match &message.media {
        Some(MessageMedia::Sticker { emoji }) if text.trim().is_empty() => {
            if let Some(emoji) = emoji {
                text = format!("{emoji} (sticker)");
            } else {
                text = "(sticker)".into();
            }
        }
        Some(MessageMedia::Location {
            latitude,
            longitude,
        }) => text.push_str(&format!("\nLocation: {latitude}, {longitude}")),
        Some(MessageMedia::Poll { question, answers }) => {
            text.push_str(&format!("\nPoll: {}", escape_text(question)));
            for (answer, voters) in answers {
                text.push_str(&format!("\n- {} ({voters})", escape_text(answer)));
            }
        }
        _ => {}
    }
    text
}

fn to_chat(batch: &ChatBatch, replied: &BTreeMap<(i64, i64), Message>) -> Chat {
    let name = clean_name(&batch.name);
    let mut chat = Chat::new(
        ChatFormat::Telegram,
        batch.id.to_string(),
        format!("Telegram - {name}"),
    );
    chat.kind = batch.kind.id().to_string();
    let mut messages: Vec<&Message> = batch.messages.iter().collect();
    messages.sort_by_key(|m| m.id);
    let find = |id: i64| {
        batch
            .messages
            .iter()
            .find(|m| m.id == id)
            .or_else(|| replied.get(&(batch.id, id)))
    };
    for m in messages {
        let time = local_time(m.date);
        let mark = format!("{:020}", m.id);
        let mut message = match &m.service {
            Some(service) => {
                ChatMessage::new(m.id.to_string(), mark, time, None, escape_text(service))
            }
            None => {
                let sender = m
                    .sender
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| name.clone());
                let mut message =
                    ChatMessage::new(m.id.to_string(), mark, time, Some(sender), markdown(m));
                match batch.files.get(&m.id) {
                    Some(Downloaded::File { name, entry }) => message.media.push(Media::File {
                        name: name.clone(),
                        entry: entry.clone(),
                    }),
                    Some(Downloaded::TooLarge(name)) => message.media.push(Media::Omitted {
                        what: format!("{name} (too large to download)"),
                    }),
                    Some(Downloaded::Failed(name)) => {
                        message.media.push(Media::Missing { name: name.clone() })
                    }
                    None => {
                        if let Some(MessageMedia::Other { what }) = &m.media {
                            message.media.push(Media::Omitted { what: what.clone() });
                        }
                    }
                }
                message
            }
        };
        if let Some(reply) = m.reply_to {
            message.reply_to = Some(match find(reply) {
                Some(r) => {
                    let at = local_time(r.date);
                    let when = if at.date() == time.date() {
                        at.format("%H:%M").to_string()
                    } else {
                        at.format("%Y-%m-%d %H:%M").to_string()
                    };
                    let who = r
                        .sender
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| name.clone());
                    let plain = crate::text::one_line(r.service.as_deref().unwrap_or(&r.text), 40);
                    if plain.is_empty() {
                        format!("{} at {when}", escape_name(&who))
                    } else {
                        format!(
                            "{} at {when}: \"{}\"",
                            escape_name(&who),
                            escape_text(&plain)
                        )
                    }
                }
                None => format!("message {reply}"),
            });
        }
        chat.messages.push(message);
    }
    chat.collect_participants();
    chat
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(kind: EntityKind, offset: usize, length: usize) -> Entity {
        Entity {
            kind,
            offset,
            length,
            href: None,
            language: None,
        }
    }

    #[test]
    fn cuts_text_by_utf16_offsets() {
        // "🙂" is two UTF-16 units.
        let text = "🙂 big and code";
        let parts = parts(
            text,
            &[
                entity(EntityKind::Bold, 3, 3),
                entity(EntityKind::Code, 11, 4),
                entity(EntityKind::Italic, 4, 1),
            ],
        );
        let markdown: String = parts.iter().map(entity_markdown).collect();
        assert_eq!(markdown, "🙂 **big** and `code`");
    }

    #[test]
    fn entities_past_the_end_are_cut() {
        let parts = parts("abc", &[entity(EntityKind::Bold, 1, 10)]);
        let markdown: String = parts.iter().map(entity_markdown).collect();
        assert_eq!(markdown, "a**bc**");
    }

    #[test]
    fn cursors_round_trip() {
        let mut c = ChatCursors::default();
        c.set(5, 10);
        c.set(5, 7);
        let cursor = c.write();
        assert_eq!(ChatCursors::read(Some(&cursor)).get(5), Some(10));
        assert_eq!(
            ChatCursors::read(Some(&Cursor::new("not json"))).get(5),
            None
        );
    }
}

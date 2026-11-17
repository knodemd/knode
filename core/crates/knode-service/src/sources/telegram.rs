//! The Telegram connector's network side: [`MtprotoApi`], Knode's own Telegram client over
//! MTProto (the official API, through the grammers library), behind
//! [`knode_ingest::telegram_live::TelegramApi`].
//!
//! It is the client a person signs in to, with Knode's own app id: it reads the person's chats
//! and the messages Telegram pushes as they arrive, and never sends a message, reads a secret
//! chat or does anything an official client would not.
//!
//! The app id is read when Knode is built (`KNODE_TELEGRAM_API_ID`, `KNODE_TELEGRAM_API_HASH`)
//! and can be given to Knode Service at run time with the same variables. A build without one
//! lists the connector, which says it has no app id.
//!
//! The session is the authorization key Telegram gave this client and the data centre it
//! lives in. [`MtprotoApi::session`] hands it to the connector, which keeps it only in the
//! system keychain; nothing of it is written anywhere else.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::{SocketAddrV4, SocketAddrV6};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::media::Media;
use grammers_client::message::Message as TgMessage;
use grammers_client::peer::Peer;
use grammers_client::sender::{
    ConnectionParams, InvocationError, NoRetries, SenderPool, UpdatesConfiguration,
};
use grammers_client::session::storages::{MemorySession, MemorySessionError};
use grammers_client::session::types::{
    DcOption, PeerAuth, PeerId, PeerInfo, PeerKind as TgPeerKind, PeerRef, UpdateState,
    UpdatesState,
};
use grammers_client::session::{BoxFuture, Session, SessionData};
use grammers_client::update::Update;
use grammers_client::{Client, SignInError, tl};
use knode_ingest::telegram_live::{
    Account, ApiError, ChatKind, ChatRef, Dialog, Entity, EntityKind, HistoryFrom, Live, Message,
    MessageMedia, PeerKind, TelegramApi, TelegramConnector,
};
use serde::{Deserialize, Serialize};

/// The longest a call to Telegram waits for its answer.
const TIMEOUT: Duration = Duration::from_secs(30);
/// How many messages' media are remembered, to download them without asking for the message
/// again.
const MEDIA_CACHE: usize = 2000;
/// The most live messages held before the connector drains them.
const LIVE_LIMIT: usize = 10_000;

/// Knode's Telegram app: the `api_id` and `api_hash` Telegram gave it.
#[derive(Clone, PartialEq, Eq)]
pub struct App {
    /// The app id.
    pub id: i32,
    /// The app hash.
    pub hash: String,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("App { .. }")
    }
}

impl App {
    /// The app Knode Service uses: the one given at run time (`KNODE_TELEGRAM_API_ID` and
    /// `KNODE_TELEGRAM_API_HASH`), else the one this build was made with, else none.
    pub fn configured() -> Option<App> {
        let id = std::env::var("KNODE_TELEGRAM_API_ID").ok();
        let hash = std::env::var("KNODE_TELEGRAM_API_HASH").ok();
        App::from_values(
            (id.as_deref(), hash.as_deref()),
            (
                option_env!("KNODE_TELEGRAM_API_ID"),
                option_env!("KNODE_TELEGRAM_API_HASH"),
            ),
        )
    }

    /// The app from the values given at run time, else those of the build. Each pair counts
    /// only when both are there and the id is a number.
    pub fn from_values(
        runtime: (Option<&str>, Option<&str>),
        build: (Option<&str>, Option<&str>),
    ) -> Option<App> {
        let pair = |(id, hash): (Option<&str>, Option<&str>)| {
            let id = id?.trim().parse::<i32>().ok().filter(|id| *id > 0)?;
            let hash = hash?.trim();
            (!hash.is_empty()).then(|| App {
                id,
                hash: hash.to_string(),
            })
        };
        pair(runtime).or_else(|| pair(build))
    }
}

/// The Telegram connector Knode Service runs.
pub fn connector() -> TelegramConnector<MtprotoApi> {
    TelegramConnector::new(MtprotoApi::new(App::configured()))
}

/// A session that remembers what is worth keeping: the data centre and its authorization
/// keys. Peers and update state are asked for again after a restart.
struct KnodeSession {
    inner: MemorySession,
    home: AtomicI32,
    dcs: Mutex<HashMap<i32, DcOption>>,
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    v: u32,
    home_dc: i32,
    dcs: Vec<SavedDc>,
}

#[derive(Serialize, Deserialize)]
struct SavedDc {
    id: i32,
    ipv4: SocketAddrV4,
    ipv6: SocketAddrV6,
    key: String,
}

impl KnodeSession {
    fn new(saved: Option<&str>) -> Result<Self, ApiError> {
        let mut data = SessionData::default();
        if let Some(saved) = saved {
            let saved: SavedSession = serde_json::from_str(saved)
                .map_err(|_| ApiError::SignedOut)
                .and_then(|s: SavedSession| {
                    if s.v == 1 {
                        Ok(s)
                    } else {
                        Err(ApiError::SignedOut)
                    }
                })?;
            data.home_dc = saved.home_dc;
            for dc in saved.dcs {
                let key = base64::engine::general_purpose::STANDARD
                    .decode(&dc.key)
                    .ok()
                    .and_then(|k| <[u8; 256]>::try_from(k).ok())
                    .ok_or(ApiError::SignedOut)?;
                data.dc_options.insert(
                    dc.id,
                    DcOption {
                        id: dc.id,
                        ipv4: dc.ipv4,
                        ipv6: dc.ipv6,
                        auth_key: Some(key),
                    },
                );
            }
        }
        let home = AtomicI32::new(data.home_dc);
        let dcs = Mutex::new(data.dc_options.clone());
        Ok(KnodeSession {
            inner: MemorySession::from(data),
            home,
            dcs,
        })
    }

    fn save(&self) -> String {
        let dcs = lock(&self.dcs);
        let saved = SavedSession {
            v: 1,
            home_dc: self.home.load(Ordering::SeqCst),
            dcs: dcs
                .values()
                .filter_map(|dc| {
                    dc.auth_key.map(|key| SavedDc {
                        id: dc.id,
                        ipv4: dc.ipv4,
                        ipv6: dc.ipv6,
                        key: base64::engine::general_purpose::STANDARD.encode(key),
                    })
                })
                .collect(),
        };
        serde_json::to_string(&saved).unwrap_or_default()
    }
}

impl Session for KnodeSession {
    type Error = MemorySessionError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        self.inner.home_dc_id()
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.home.store(dc_id, Ordering::SeqCst);
        self.inner.set_home_dc_id(dc_id)
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        self.inner.dc_option(dc_id)
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        lock(&self.dcs).insert(dc_option.id, dc_option.clone());
        self.inner.set_dc_option(dc_option)
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        self.inner.peer(peer)
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.inner.cache_peer(peer)
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        self.inner.updates_state()
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        self.inner.set_update_state(update)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// One connection to Telegram.
struct Connection {
    client: Client,
    session: Arc<KnodeSession>,
    login: Option<LoginToken>,
    password: Option<PasswordToken>,
    /// The updates of the connection, until they are read: once signed in.
    pending_updates:
        Option<tokio::sync::mpsc::Receiver<grammers_client::session::updates::UpdatesLike>>,
    updates: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(updates) = self.updates.take() {
            updates.abort();
        }
        self.client.disconnect();
    }
}

#[derive(Default)]
struct LiveBuffer {
    messages: Vec<Message>,
    gap: bool,
    /// The update stream ended: the connection is made again on the next call.
    ended: bool,
}

/// Knode's Telegram client, over MTProto.
pub struct MtprotoApi {
    app: Option<App>,
    runtime: OnceLock<std::io::Result<tokio::runtime::Runtime>>,
    connection: Mutex<Option<Connection>>,
    reconnected: Mutex<Option<std::time::Instant>>,
    live: Arc<Mutex<LiveBuffer>>,
    media: Arc<Mutex<MediaCache>>,
}

#[derive(Default)]
struct MediaCache {
    order: VecDeque<(i64, i64)>,
    media: HashMap<(i64, i64), Media>,
}

impl MediaCache {
    fn insert(&mut self, key: (i64, i64), media: Media) {
        if self.media.insert(key, media).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MEDIA_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.media.remove(&old);
            }
        }
    }
}

impl MtprotoApi {
    /// A client with Knode's app, or with none.
    pub fn new(app: Option<App>) -> Self {
        MtprotoApi {
            app,
            runtime: OnceLock::new(),
            connection: Mutex::new(None),
            reconnected: Mutex::new(None),
            live: Arc::new(Mutex::new(LiveBuffer::default())),
            media: Arc::new(Mutex::new(MediaCache::default())),
        }
    }

    fn app(&self) -> Result<&App, ApiError> {
        self.app.as_ref().ok_or(ApiError::NoApp)
    }

    fn runtime(&self) -> Result<&tokio::runtime::Runtime, ApiError> {
        self.runtime
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .thread_name("knode-telegram")
                    .enable_all()
                    .build()
            })
            .as_ref()
            .map_err(|e| ApiError::Other(format!("the Telegram client cannot start: {e}")))
    }

    /// Runs `future` on the client's own runtime and waits for it, at most [`TIMEOUT`]. Safe
    /// from any thread, also one of another runtime.
    fn run<F, T>(&self, future: F) -> Result<T, ApiError>
    where
        F: Future<Output = Result<T, ApiError>> + Send + 'static,
        T: Send + 'static,
    {
        let runtime = self.runtime()?;
        let (tx, rx) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let result = match tokio::time::timeout(TIMEOUT, future).await {
                Ok(result) => result,
                Err(_) => Err(ApiError::Network("Telegram did not answer in time".into())),
            };
            let _ = tx.send(result);
        });
        rx.recv_timeout(TIMEOUT + Duration::from_secs(5))
            .unwrap_or_else(|_| Err(ApiError::Network("Telegram did not answer in time".into())))
    }

    fn client(&self) -> Result<Client, ApiError> {
        self.reconnect_if_ended()?;
        lock(&self.connection)
            .as_ref()
            .map(|c| c.client.clone())
            .ok_or(ApiError::SignedOut)
    }

    /// Opens a connection with this session (a new one when `None`).
    fn open(&self, saved: Option<&str>) -> Result<(), ApiError> {
        let app = self.app()?.clone();
        let runtime = self.runtime()?;
        let session = Arc::new(KnodeSession::new(saved)?);
        let params = ConnectionParams {
            device_model: "Knode".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            // Flood waits are the connector's to respect: it sleeps or stops asking.
            retry_policy: Box::new(NoRetries),
            ..Default::default()
        };
        let pool = SenderPool::with_configuration(Arc::clone(&session), app.id, params);
        let client = Client::new(pool.handle);
        let _guard = runtime.enter();
        runtime.spawn(pool.runner.run());
        let connection = Connection {
            client,
            session,
            login: None,
            password: None,
            pending_updates: Some(pool.updates),
            updates: None,
        };
        *lock(&self.live) = LiveBuffer::default();
        *lock(&self.connection) = Some(connection);
        Ok(())
    }

    /// Starts reading the updates of the connection, once it is signed in.
    fn start_updates(&self) -> Result<(), ApiError> {
        let runtime = self.runtime()?;
        let mut connection = lock(&self.connection);
        let Some(connection) = connection.as_mut() else {
            return Ok(());
        };
        let Some(updates) = connection.pending_updates.take() else {
            return Ok(());
        };
        let live = Arc::clone(&self.live);
        let media = Arc::clone(&self.media);
        let client = connection.client.clone();
        connection.updates = Some(runtime.spawn(stream(client, updates, live, media)));
        Ok(())
    }

    /// When the update stream ended (the connection was lost), connects again with the same
    /// session, at most once a minute, and says there is a gap.
    fn reconnect_if_ended(&self) -> Result<(), ApiError> {
        if !lock(&self.live).ended {
            return Ok(());
        }
        {
            let mut last = lock(&self.reconnected);
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                return Err(ApiError::Network(
                    "the connection to Telegram was lost".into(),
                ));
            }
            *last = Some(std::time::Instant::now());
        }
        let saved = lock(&self.connection).as_ref().map(|c| c.session.save());
        if let Some(saved) = saved {
            self.open(Some(&saved))?;
            let client = self.client_now()?;
            if self.run(async move { client.is_authorized().await.map_err(api_error) })? {
                self.start_updates()?;
            }
            lock(&self.live).gap = true;
        }
        Ok(())
    }

    fn client_now(&self) -> Result<Client, ApiError> {
        lock(&self.connection)
            .as_ref()
            .map(|c| c.client.clone())
            .ok_or(ApiError::SignedOut)
    }
}

/// Reads the updates of a connection, keeping the new messages for the connector.
async fn stream(
    client: Client,
    updates: tokio::sync::mpsc::Receiver<grammers_client::session::updates::UpdatesLike>,
    live: Arc<Mutex<LiveBuffer>>,
    media: Arc<Mutex<MediaCache>>,
) {
    let mut stream = match client
        .stream_updates(updates, UpdatesConfiguration { catch_up: false })
        .await
    {
        Ok(stream) => stream,
        Err(_) => {
            lock(&live).ended = true;
            return;
        }
    };
    loop {
        match stream.next().await {
            Ok(Update::NewMessage(message)) => {
                let message = message.into_inner();
                if let Some(mapped) = to_message(&message, &media) {
                    let mut live = lock(&live);
                    if live.messages.len() >= LIVE_LIMIT {
                        live.messages.remove(0);
                        live.gap = true;
                    }
                    live.messages.push(mapped);
                }
            }
            Ok(_) => {}
            Err(_) => {
                let mut live = lock(&live);
                live.gap = true;
                live.ended = true;
                return;
            }
        }
    }
}

fn api_error(error: InvocationError) -> ApiError {
    match error {
        InvocationError::Rpc(rpc) if rpc.code == 420 || rpc.name.starts_with("FLOOD_WAIT") => {
            ApiError::FloodWait(Duration::from_secs(u64::from(rpc.value.unwrap_or(1))))
        }
        InvocationError::Rpc(rpc) if rpc.code == 401 => ApiError::SignedOut,
        InvocationError::Rpc(rpc) if rpc.name.starts_with("PHONE_NUMBER") => ApiError::BadPhone,
        InvocationError::Rpc(rpc) if rpc.name.starts_with("PHONE_CODE") => ApiError::BadCode,
        InvocationError::Rpc(rpc) => ApiError::Other(format!("Telegram refused: {}", rpc.name)),
        InvocationError::Io(e) => ApiError::Network(e.to_string()),
        other => ApiError::Network(other.to_string()),
    }
}

/// The chat of a peer.
fn chat_ref(id: PeerId, auth: Option<PeerAuth>) -> Option<ChatRef> {
    let bare = id.bare_id()?;
    let kind = match id.kind() {
        TgPeerKind::User => PeerKind::User,
        TgPeerKind::Chat => PeerKind::Group,
        TgPeerKind::Channel => PeerKind::Channel,
    };
    Some(ChatRef {
        id: bare,
        kind,
        access_hash: auth.map(PeerAuth::hash),
    })
}

fn peer_ref(chat: &ChatRef) -> PeerRef {
    let id = match chat.kind {
        PeerKind::User => PeerId::user_unchecked(chat.id),
        PeerKind::Group => PeerId::chat_unchecked(chat.id),
        PeerKind::Channel => PeerId::channel_unchecked(chat.id),
    };
    PeerRef {
        id,
        auth: chat
            .access_hash
            .map(PeerAuth::from_hash)
            .unwrap_or_default(),
    }
}

fn peer_name(peer: &Peer) -> Option<String> {
    match peer {
        Peer::User(user) if user.is_self() => Some("Saved Messages".to_string()),
        Peer::User(user) => Some(user.full_name()).filter(|n| !n.trim().is_empty()),
        other => other.name().map(str::to_string),
    }
}

fn sender_name(peer: &Peer) -> Option<String> {
    match peer {
        Peer::User(user) => Some(user.full_name()).filter(|n| !n.trim().is_empty()),
        other => other.name().map(str::to_string),
    }
}

fn peer_kind(peer: &Peer) -> ChatKind {
    match peer {
        Peer::User(_) => ChatKind::Personal,
        Peer::Channel(_) => ChatKind::Channel,
        _ => ChatKind::Group,
    }
}

/// A message as the connector reads it, remembering its media to download it later.
fn to_message(m: &TgMessage, cache: &Mutex<MediaCache>) -> Option<Message> {
    let chat = chat_ref(m.peer_id(), None)?;
    let chat_name = m.peer().and_then(peer_name);
    let chat_kind = m.peer().map(peer_kind);
    let id = i64::from(m.id());
    let service = m.action().map(describe_action);
    let entities = m
        .fmt_entities()
        .map(|list| list.iter().map(entity).collect())
        .unwrap_or_default();
    let forwarded_from = m.forward_header().map(|h| {
        let tl::enums::MessageFwdHeader::Header(h) = h;
        h.from_name
            .or(h.post_author)
            .unwrap_or_else(|| "someone".to_string())
    });
    let media = m.media().map(|media| {
        let mapped = describe_media(&media);
        if matches!(
            mapped,
            MessageMedia::Photo { .. } | MessageMedia::File { .. }
        ) {
            lock(cache).insert((chat.id, id), media);
        }
        mapped
    });
    Some(Message {
        chat,
        chat_name,
        chat_kind,
        id,
        date: m.date().as_second(),
        sender: m.sender().and_then(sender_name),
        service,
        text: m.text().to_string(),
        entities,
        reply_to: m.reply_to_message_id().map(i64::from),
        forwarded_from,
        media,
    })
}

fn entity(e: &tl::enums::MessageEntity) -> Entity {
    use tl::enums::MessageEntity as E;
    let (kind, href, language) = match e {
        E::Bold(_) => (EntityKind::Bold, None, None),
        E::Italic(_) => (EntityKind::Italic, None, None),
        E::Strike(_) => (EntityKind::Strike, None, None),
        E::Code(_) => (EntityKind::Code, None, None),
        E::Pre(p) => (
            EntityKind::Pre,
            None,
            Some(p.language.clone()).filter(|l| !l.is_empty()),
        ),
        E::TextUrl(t) => (EntityKind::TextLink, Some(t.url.clone()), None),
        E::Url(_) => (EntityKind::Url, None, None),
        E::Blockquote(_) => (EntityKind::Blockquote, None, None),
        _ => (EntityKind::Other, None, None),
    };
    Entity {
        kind,
        offset: e.offset().max(0) as usize,
        length: e.length().max(0) as usize,
        href,
        language,
    }
}

fn describe_media(media: &Media) -> MessageMedia {
    match media {
        Media::Photo(photo) => MessageMedia::Photo {
            size: photo.size().map(|s| s as u64),
        },
        Media::Sticker(sticker) => MessageMedia::Sticker {
            emoji: Some(sticker.emoji().to_string()).filter(|e| !e.is_empty()),
        },
        Media::Document(doc) => {
            let mime = doc.mime_type().unwrap_or("");
            let what = if mime.starts_with("audio/ogg") {
                "voice message"
            } else if mime.starts_with("video/") {
                "video"
            } else if mime.starts_with("audio/") {
                "audio"
            } else {
                "file"
            };
            MessageMedia::File {
                name: doc.name().map(str::to_string).filter(|n| !n.is_empty()),
                what: what.to_string(),
                size: doc.size().map(|s| s as u64),
            }
        }
        Media::Geo(geo) => MessageMedia::Location {
            latitude: geo.latitue(),
            longitude: geo.longitude(),
        },
        Media::Venue(venue) => MessageMedia::Other {
            what: format!("place \"{}\"", venue.title()),
        },
        Media::Contact(contact) => MessageMedia::Other {
            what: format!("contact {} {}", contact.first_name(), contact.last_name())
                .trim_end()
                .to_string(),
        },
        Media::Poll(_) => MessageMedia::Other {
            what: "poll".into(),
        },
        Media::GeoLive(_) => MessageMedia::Other {
            what: "live location".into(),
        },
        Media::Dice(dice) => MessageMedia::Other {
            what: format!("{} {}", dice.emoji(), dice.value()),
        },
        Media::WebPage(_) => MessageMedia::Other {
            what: "link preview".into(),
        },
        _ => MessageMedia::Other {
            what: "media".into(),
        },
    }
}

/// What Telegram itself wrote, as the export importer words it.
fn describe_action(action: &tl::enums::MessageAction) -> String {
    use tl::enums::MessageAction as A;
    match action {
        A::ChatCreate(a) => format!("created \"{}\"", a.title),
        A::ChannelCreate(a) => format!("created \"{}\"", a.title),
        A::ChatEditTitle(a) => format!("changed the title to \"{}\"", a.title),
        A::ChatEditPhoto(_) => "changed the photo".into(),
        A::ChatDeletePhoto => "removed the photo".into(),
        A::ChatAddUser(_) => "added members".into(),
        A::ChatDeleteUser(_) => "removed a member".into(),
        A::ChatJoinedByLink(_) | A::ChatJoinedByRequest => "joined by link".into(),
        A::PinMessage => "pinned a message".into(),
        A::PhoneCall(_) => "called".into(),
        A::ChatMigrateTo(_) | A::ChannelMigrateFrom(_) => "upgraded the group".into(),
        other => {
            // The action's name: `ScreenshotTaken` is "screenshot taken".
            let name = format!("{other:?}");
            let name = name.split(['(', ' ', '{']).next().unwrap_or("");
            let mut out = String::new();
            for c in name.chars() {
                if c.is_uppercase() && !out.is_empty() {
                    out.push(' ');
                }
                out.extend(c.to_lowercase());
            }
            out
        }
    }
}

impl TelegramApi for MtprotoApi {
    fn connect(&self, session: Option<&str>) -> Result<bool, ApiError> {
        self.app()?;
        self.open(session)?;
        if session.is_none() {
            return Ok(false);
        }
        let client = self.client_now()?;
        let signed_in = self.run(async move { client.is_authorized().await.map_err(api_error) })?;
        if signed_in {
            self.start_updates()?;
        }
        Ok(signed_in)
    }

    fn session(&self) -> Result<String, ApiError> {
        lock(&self.connection)
            .as_ref()
            .map(|c| c.session.save())
            .ok_or(ApiError::SignedOut)
    }

    fn request_code(&self, phone: &str) -> Result<Option<String>, ApiError> {
        let hash = self.app()?.hash.clone();
        let client = self.client()?;
        let phone = phone.to_string();
        let token = self.run(async move {
            client
                .request_login_code(&phone, &hash)
                .await
                .map_err(api_error)
        })?;
        if let Some(c) = lock(&self.connection).as_mut() {
            c.login = Some(token);
            c.password = None;
        }
        Ok(None)
    }

    fn sign_in(&self, code: &str) -> Result<Account, ApiError> {
        let client = self.client()?;
        let token = lock(&self.connection)
            .as_mut()
            .and_then(|c| c.login.take())
            .ok_or(ApiError::NoCodeRequested)?;
        let code = code.to_string();
        let (result, token) = self.run(async move {
            let result = client.sign_in(&token, &code).await;
            Ok((result, token))
        })?;
        match result {
            Ok(user) => {
                self.start_updates()?;
                Ok(Account {
                    name: user.full_name(),
                })
            }
            Err(SignInError::PasswordRequired(password)) => {
                let hint = password.hint().map(str::to_string);
                if let Some(c) = lock(&self.connection).as_mut() {
                    c.password = Some(password);
                }
                Err(ApiError::PasswordNeeded(hint))
            }
            Err(SignInError::InvalidCode) => {
                // The same code request can be answered again.
                if let Some(c) = lock(&self.connection).as_mut() {
                    c.login = Some(token);
                }
                Err(ApiError::BadCode)
            }
            Err(SignInError::SignUpRequired) => Err(ApiError::Other(
                "this phone number has no Telegram account: sign up in a Telegram app first".into(),
            )),
            Err(SignInError::InvalidPassword(_)) => Err(ApiError::BadPassword),
            Err(SignInError::Other(e)) => Err(api_error(e)),
        }
    }

    fn check_password(&self, password: &str) -> Result<Account, ApiError> {
        let client = self.client()?;
        let token = lock(&self.connection)
            .as_mut()
            .and_then(|c| c.password.take())
            .ok_or(ApiError::NoCodeRequested)?;
        let password = password.to_string();
        let result = self.run(async move { Ok(client.check_password(token, password).await) })?;
        match result {
            Ok(user) => {
                self.start_updates()?;
                Ok(Account {
                    name: user.full_name(),
                })
            }
            Err(SignInError::InvalidPassword(token)) => {
                if let Some(c) = lock(&self.connection).as_mut() {
                    c.password = Some(token);
                }
                Err(ApiError::BadPassword)
            }
            Err(SignInError::Other(e)) => Err(api_error(e)),
            Err(e) => Err(ApiError::Other(e.to_string())),
        }
    }

    fn me(&self) -> Result<Account, ApiError> {
        let client = self.client()?;
        self.run(async move {
            let me = client.get_me().await.map_err(api_error)?;
            Ok(Account {
                name: me.full_name(),
            })
        })
    }

    fn sign_out(&self) -> Result<(), ApiError> {
        let client = match self.client() {
            Ok(client) => client,
            Err(_) => return Ok(()),
        };
        let result = self.run(async move { client.sign_out().await.map_err(api_error) });
        *lock(&self.connection) = None;
        result.map(|_| ())
    }

    fn dialogs(&self) -> Result<Vec<Dialog>, ApiError> {
        let client = self.client()?;
        let media = Arc::clone(&self.media);
        self.run(async move {
            let mut out = Vec::new();
            let mut dialogs = client.iter_dialogs();
            while let Some(dialog) = dialogs.next().await.map_err(api_error)? {
                let peer = dialog.peer();
                let Some(chat) = chat_ref(peer.id(), Some(dialog.peer_ref().auth)) else {
                    continue;
                };
                let (top_message, top_date) = match &dialog.last_message {
                    Some(m) => {
                        let _ = to_message(m, &media);
                        (i64::from(m.id()), m.date().as_second())
                    }
                    None => (0, 0),
                };
                out.push(Dialog {
                    chat,
                    name: peer_name(peer).unwrap_or_else(|| format!("chat {}", chat.id)),
                    kind: peer_kind(peer),
                    top_message,
                    top_date,
                });
            }
            Ok(out)
        })
    }

    fn history(
        &self,
        chat: &ChatRef,
        from: HistoryFrom,
        limit: usize,
    ) -> Result<Vec<Message>, ApiError> {
        let client = self.client()?;
        let media = Arc::clone(&self.media);
        let peer = peer_ref(chat);
        self.run(async move {
            let iter = client.iter_messages(peer).reverse(true).limit(limit);
            let mut iter = match from {
                HistoryFrom::After(id) => iter.offset_id(i32::try_from(id).unwrap_or(i32::MAX)),
                HistoryFrom::Since(date) => {
                    iter.offset_date(i32::try_from(date).unwrap_or(i32::MAX))
                }
            };
            let mut out = Vec::new();
            while let Some(m) = iter.next().await.map_err(api_error)? {
                if let Some(mapped) = to_message(&m, &media) {
                    out.push(mapped);
                }
            }
            out.sort_by_key(|m| m.id);
            Ok(out)
        })
    }

    fn take_live(&self) -> Live {
        let _ = self.reconnect_if_ended();
        let mut live = lock(&self.live);
        Live {
            messages: std::mem::take(&mut live.messages),
            gap: std::mem::take(&mut live.gap),
        }
    }

    fn download(
        &self,
        chat: &ChatRef,
        message: i64,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        let client = self.client()?;
        let cached = lock(&self.media).media.get(&(chat.id, message)).cloned();
        let peer = peer_ref(chat);
        let id = i32::try_from(message).map_err(|_| ApiError::Other("no such message".into()))?;
        self.run(async move {
            let media = match cached {
                Some(media) => media,
                None => {
                    let found = client
                        .get_messages_by_id(peer, &[id])
                        .await
                        .map_err(api_error)?;
                    match found.into_iter().flatten().next().and_then(|m| m.media()) {
                        Some(media) => media,
                        None => return Err(ApiError::Other("the message has no file".into())),
                    }
                }
            };
            let mut bytes = Vec::new();
            let mut chunks = client.iter_download(&media);
            while let Some(chunk) = chunks.next().await.map_err(api_error)? {
                bytes.extend_from_slice(&chunk);
                if bytes.len() as u64 > max_bytes {
                    return Ok(None);
                }
            }
            Ok(Some(bytes))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_comes_from_run_time_then_build_time() {
        let run = (Some("123"), Some("abc"));
        let build = (Some("456"), Some("def"));
        assert_eq!(App::from_values(run, build).unwrap().id, 123);
        assert_eq!(App::from_values((None, None), build).unwrap().id, 456);
        assert_eq!(
            App::from_values((Some("123"), None), build).unwrap().id,
            456,
            "half a pair does not count"
        );
        assert!(App::from_values((Some("x"), Some("abc")), (None, None)).is_none());
        assert!(App::from_values((Some("1"), Some(" ")), (None, None)).is_none());
        assert!(App::from_values((None, None), (None, None)).is_none());
    }

    #[test]
    fn without_an_app_id_the_connector_says_so() {
        use knode_ingest::sources::{Connector, SignInStep};
        let connector = TelegramConnector::new(MtprotoApi::new(None));
        let secrets = knode_ingest::sources::NoSecrets;
        let error = connector
            .sign_in(
                SignInStep::Start {
                    account: "+34600000000".into(),
                },
                &secrets,
            )
            .unwrap_err();
        assert_eq!(error.kind(), "unavailable");
        assert!(error.to_string().contains("no Telegram app id"));
        assert!(
            connector.api().runtime.get().is_none(),
            "no client was started"
        );
    }

    #[test]
    fn a_session_round_trips_without_peers() {
        let session = KnodeSession::new(None).unwrap();
        // A new session has no keys, so nothing is worth keeping yet.
        let saved: SavedSession = serde_json::from_str(&session.save()).unwrap();
        assert!(saved.dcs.is_empty());
        let mut with_key = SavedSession {
            v: 1,
            home_dc: 4,
            dcs: Vec::new(),
        };
        with_key.dcs.push(SavedDc {
            id: 4,
            ipv4: "149.154.167.91:443".parse().unwrap(),
            ipv6: "[2001:67c:4e8:f004::a]:443".parse().unwrap(),
            key: base64::engine::general_purpose::STANDARD.encode([7u8; 256]),
        });
        let text = serde_json::to_string(&with_key).unwrap();
        let restored = KnodeSession::new(Some(&text)).unwrap();
        assert_eq!(restored.home_dc_id().unwrap(), 4);
        assert_eq!(
            restored.dc_option(4).unwrap().unwrap().auth_key,
            Some([7u8; 256])
        );
        let again: SavedSession = serde_json::from_str(&restored.save()).unwrap();
        assert_eq!(again.home_dc, 4);
        assert_eq!(again.dcs.len(), 1);
        assert!(KnodeSession::new(Some("not a session")).is_err());
    }

    #[test]
    fn actions_are_described() {
        use tl::enums::MessageAction as A;
        assert_eq!(describe_action(&A::PinMessage), "pinned a message");
        assert_eq!(describe_action(&A::ScreenshotTaken), "screenshot taken");
    }
}

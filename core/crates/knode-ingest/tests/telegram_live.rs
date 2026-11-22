//! The Telegram connector against a mocked API: signing in, catching up, live messages,
//! restarts, exclusions, history, flood waits and a build with no app id. No network.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use knode_format::{Node, Space, Timestamp};
use knode_ingest::chat_keys;
use knode_ingest::sources::{
    Connector, Container, Cursor, FetchRequest, History, Permission, Secrets, SignInState,
    SignInStep, SourceError, provenance,
};
use knode_ingest::telegram_live::{
    Account, ApiError, ChatKind, ChatRef, Dialog, Entity, EntityKind, HistoryFrom, Live, Message,
    MessageMedia, PeerKind, SESSION_SECRET, TelegramApi, TelegramConnector, TelegramOptions,
};
use tempfile::TempDir;

const SESSION: &str = "mock-session";
const CODE: &str = "12345";
const PHONE: &str = "+34 600 000 000";

/// 2026-03-10 12:00:00 UTC.
const T0: i64 = 1_773_144_000;

#[derive(Default)]
struct State {
    app: bool,
    password: Option<String>,
    revoked: bool,
    code_requested: bool,
    signed_in: bool,
    signed_out: bool,
    dialogs: Vec<Dialog>,
    history: BTreeMap<i64, Vec<Message>>,
    live: Vec<Message>,
    gap: bool,
    files: HashMap<(i64, i64), Vec<u8>>,
    floods: VecDeque<Duration>,
    calls: Vec<String>,
    /// While set, `history` waits until a message comes through it.
    hold: Option<std::sync::mpsc::Receiver<()>>,
}

#[derive(Clone, Default)]
struct MockApi(Arc<Mutex<State>>);

impl MockApi {
    fn new() -> Self {
        let api = MockApi::default();
        api.state().app = true;
        api
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn calls(&self, name: &str) -> usize {
        self.state().calls.iter().filter(|c| *c == name).count()
    }

    fn call(&self, name: &str) -> Result<(), ApiError> {
        let mut s = self.state();
        s.calls.push(name.to_string());
        if !s.app {
            return Err(ApiError::NoApp);
        }
        if let Some(wait) = s.floods.pop_front() {
            return Err(ApiError::FloodWait(wait));
        }
        Ok(())
    }

    fn chat(&self, id: i64, name: &str, kind: ChatKind) {
        let mut s = self.state();
        s.dialogs.push(Dialog {
            chat: chat_ref(id),
            name: name.into(),
            kind,
            top_message: 0,
            top_date: 0,
        });
    }

    /// A message in the history of the chat, which also becomes its last one.
    fn post(&self, chat: i64, id: i64, date: i64, sender: &str, text: &str) -> Message {
        let m = message(chat, id, date, sender, text);
        let mut s = self.state();
        s.history.entry(chat).or_default().push(m.clone());
        if let Some(d) = s.dialogs.iter_mut().find(|d| d.chat.id == chat) {
            d.top_message = d.top_message.max(id);
            d.top_date = d.top_date.max(date);
        }
        m
    }

    /// A message that also arrives live.
    fn arrive(&self, chat: i64, id: i64, date: i64, sender: &str, text: &str) {
        let m = self.post(chat, id, date, sender, text);
        self.state().live.push(m);
    }
}

fn chat_ref(id: i64) -> ChatRef {
    ChatRef {
        id,
        kind: PeerKind::Channel,
        access_hash: Some(7),
    }
}

fn message(chat: i64, id: i64, date: i64, sender: &str, text: &str) -> Message {
    Message {
        chat: chat_ref(chat),
        chat_name: None,
        chat_kind: None,
        id,
        date,
        sender: Some(sender.into()),
        service: None,
        text: text.into(),
        entities: Vec::new(),
        reply_to: None,
        forwarded_from: None,
        media: None,
    }
}

impl TelegramApi for MockApi {
    fn connect(&self, session: Option<&str>) -> Result<bool, ApiError> {
        self.call("connect")?;
        let mut s = self.state();
        s.signed_in = session == Some(SESSION) && !s.revoked;
        Ok(s.signed_in)
    }

    fn session(&self) -> Result<String, ApiError> {
        let s = self.state();
        if s.signed_in {
            Ok(SESSION.into())
        } else {
            Err(ApiError::SignedOut)
        }
    }

    fn request_code(&self, phone: &str) -> Result<Option<String>, ApiError> {
        self.call("request_code")?;
        assert_eq!(phone, "+34600000000");
        self.state().code_requested = true;
        Ok(Some("the Telegram app".into()))
    }

    fn sign_in(&self, code: &str) -> Result<Account, ApiError> {
        self.call("sign_in")?;
        let mut s = self.state();
        if !s.code_requested {
            return Err(ApiError::NoCodeRequested);
        }
        if code != CODE {
            return Err(ApiError::BadCode);
        }
        if s.password.is_some() {
            return Err(ApiError::PasswordNeeded(Some("the cat".into())));
        }
        s.signed_in = true;
        Ok(account())
    }

    fn check_password(&self, password: &str) -> Result<Account, ApiError> {
        self.call("check_password")?;
        let mut s = self.state();
        if s.password.as_deref() != Some(password) {
            return Err(ApiError::BadPassword);
        }
        s.signed_in = true;
        Ok(account())
    }

    fn me(&self) -> Result<Account, ApiError> {
        Ok(account())
    }

    fn sign_out(&self) -> Result<(), ApiError> {
        self.call("sign_out")?;
        let mut s = self.state();
        s.signed_in = false;
        s.signed_out = true;
        Ok(())
    }

    fn dialogs(&self) -> Result<Vec<Dialog>, ApiError> {
        self.call("dialogs")?;
        Ok(self.state().dialogs.clone())
    }

    fn history(
        &self,
        chat: &ChatRef,
        from: HistoryFrom,
        limit: usize,
    ) -> Result<Vec<Message>, ApiError> {
        self.call("history")?;
        let hold = self.state().hold.take();
        if let Some(hold) = hold {
            let _ = hold.recv();
        }
        let s = self.state();
        let mut all = s.history.get(&chat.id).cloned().unwrap_or_default();
        all.sort_by_key(|m| m.id);
        Ok(all
            .into_iter()
            .filter(|m| match from {
                HistoryFrom::After(id) => m.id > id,
                HistoryFrom::Since(t) => m.date >= t,
            })
            .take(limit)
            .collect())
    }

    fn take_live(&self) -> Live {
        let mut s = self.state();
        Live {
            messages: std::mem::take(&mut s.live),
            gap: std::mem::take(&mut s.gap),
        }
    }

    fn download(
        &self,
        chat: &ChatRef,
        message: i64,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        self.call("download")?;
        let s = self.state();
        let bytes = s
            .files
            .get(&(chat.id, message))
            .cloned()
            .unwrap_or_default();
        Ok((bytes.len() as u64 <= max_bytes).then_some(bytes))
    }
}

fn account() -> Account {
    Account {
        name: "Ada Lovelace".into(),
    }
}

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().remove(name).is_some())
    }
}

fn signed_in_secrets() -> MemorySecrets {
    let secrets = MemorySecrets::default();
    secrets.set(SESSION_SECRET, SESSION).unwrap();
    secrets
}

struct Slept(Arc<Mutex<Vec<Duration>>>);

fn connector(api: &MockApi) -> (TelegramConnector<MockApi>, Slept) {
    let slept = Arc::new(Mutex::new(Vec::new()));
    let record = slept.clone();
    let options = TelegramOptions {
        sleep: Arc::new(move |d| record.lock().unwrap().push(d)),
        ..TelegramOptions::default()
    };
    (
        TelegramConnector::with_options(api.clone(), options),
        Slept(slept),
    )
}

/// One space and what the service keeps for it.
struct Run {
    _dir: TempDir,
    space: Space,
    cursor: Option<Cursor>,
    history: History,
    since: Timestamp,
    excluded: Vec<Container>,
    max_items: usize,
    media_max_bytes: Option<u64>,
}

impl Run {
    fn new(history: History) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
        Run {
            _dir: dir,
            space,
            cursor: None,
            history,
            since: Timestamp(T0 * 1000),
            excluded: Vec::new(),
            max_items: 500,
            media_max_bytes: None,
        }
    }

    /// Runs the connector like the service does: batches until there is no more. Returns
    /// the messages written.
    fn run(
        &mut self,
        c: &TelegramConnector<MockApi>,
        secrets: &dyn Secrets,
    ) -> Result<usize, SourceError> {
        let mut items = 0;
        let space_id = self.space.manifest().id.clone();
        for _ in 0..50 {
            let request = FetchRequest {
                space_id: &space_id,
                cursor: self.cursor.as_ref(),
                history: self.history,
                since: self.since,
                excluded: &self.excluded,
                max_items: self.max_items,
                secrets,
                media_max_bytes: self.media_max_bytes,
            };
            let fetched = c.fetch(&request)?;
            if let Some(batch) = fetched.batch {
                items += batch.write(&mut self.space, &provenance("telegram"))?.items;
            }
            self.cursor = Some(fetched.cursor);
            if !fetched.more {
                return Ok(items);
            }
        }
        panic!("the connector never ran out of batches");
    }

    fn node(&self, chat: i64) -> Option<Node> {
        let found: Vec<_> = self
            .space
            .find_by_property(chat_keys::ID, &chat.to_string())
            .unwrap()
            .into_iter()
            .filter(|s| {
                self.space
                    .property(&s.id, chat_keys::APP)
                    .unwrap()
                    .as_deref()
                    == Some("telegram")
            })
            .collect();
        assert!(found.len() <= 1, "one node per chat");
        found
            .first()
            .map(|s| self.space.get_node(&s.id).unwrap().unwrap())
    }

    fn prop(&self, node: &Node, key: &str) -> Option<String> {
        self.space.property(&node.id, key).unwrap()
    }
}

#[test]
fn it_is_a_connector_that_signs_in() {
    let (c, _) = connector(&MockApi::new());
    let info = c.info();
    assert_eq!(info.id, "telegram");
    assert!(info.permissions.contains(&Permission::SignIn));
    assert_eq!(info.container.as_deref(), Some("chat"));
}

#[test]
fn signs_in_with_phone_code_and_password_and_out_again() {
    let api = MockApi::new();
    api.state().password = Some("correct horse".into());
    let (c, _) = connector(&api);
    let secrets = MemorySecrets::default();

    assert_eq!(c.sign_in_state(&secrets).unwrap(), SignInState::SignedOut);
    // A code before the phone number is refused.
    let early = c.sign_in(SignInStep::Code { code: CODE.into() }, &secrets);
    assert!(matches!(early, Err(SourceError::SignIn(_))));

    let sent = c
        .sign_in(
            SignInStep::Start {
                account: PHONE.into(),
            },
            &secrets,
        )
        .unwrap();
    assert_eq!(
        sent,
        SignInState::CodeSent {
            via: Some("the Telegram app".into())
        }
    );
    assert_eq!(
        c.sign_in_state(&secrets).unwrap(),
        SignInState::CodeSent { via: None }
    );
    let wrong = c.sign_in(
        SignInStep::Code {
            code: "00000".into(),
        },
        &secrets,
    );
    assert!(matches!(wrong, Err(SourceError::SignIn(_))));
    let needs = c
        .sign_in(
            SignInStep::Code {
                code: format!(" {CODE} "),
            },
            &secrets,
        )
        .unwrap();
    assert_eq!(
        needs,
        SignInState::PasswordNeeded {
            hint: Some("the cat".into())
        }
    );
    assert!(secrets.get(SESSION_SECRET).unwrap().is_none());
    let bad = c.sign_in(
        SignInStep::Password {
            password: "wrong".into(),
        },
        &secrets,
    );
    assert!(matches!(bad, Err(SourceError::SignIn(_))));
    let done = c
        .sign_in(
            SignInStep::Password {
                password: "correct horse".into(),
            },
            &secrets,
        )
        .unwrap();
    assert_eq!(
        done,
        SignInState::SignedIn {
            account: "Ada Lovelace".into()
        }
    );
    assert_eq!(
        secrets.get(SESSION_SECRET).unwrap().as_deref(),
        Some(SESSION),
        "the session is kept in the keychain"
    );

    // Another connector (the service started again) is signed in from the keychain.
    let (again, _) = connector(&api);
    assert_eq!(
        again.sign_in_state(&secrets).unwrap(),
        SignInState::SignedIn {
            account: "Ada Lovelace".into()
        }
    );

    assert!(again.sign_out(&secrets).unwrap());
    assert!(api.state().signed_out, "Telegram ended the session");
    assert!(secrets.get(SESSION_SECRET).unwrap().is_none());
    assert_eq!(
        again.sign_in_state(&secrets).unwrap(),
        SignInState::SignedOut
    );
    assert!(!again.sign_out(&secrets).unwrap());
}

#[test]
fn signs_in_without_a_password() {
    let api = MockApi::new();
    let (c, _) = connector(&api);
    let secrets = MemorySecrets::default();
    c.sign_in(
        SignInStep::Start {
            account: PHONE.into(),
        },
        &secrets,
    )
    .unwrap();
    let done = c
        .sign_in(SignInStep::Code { code: CODE.into() }, &secrets)
        .unwrap();
    assert!(matches!(done, SignInState::SignedIn { .. }));
    assert!(secrets.get(SESSION_SECRET).unwrap().is_some());
    let bad_phone = c.sign_in(
        SignInStep::Start {
            account: "12".into(),
        },
        &secrets,
    );
    assert!(matches!(bad_phone, Err(SourceError::SignIn(_))));
}

#[test]
fn fetching_without_a_session_asks_to_sign_in() {
    let api = MockApi::new();
    let (c, _) = connector(&api);
    let mut run = Run::new(History::Everything);
    let error = run.run(&c, &MemorySecrets::default()).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
    assert!(error.needs_person());
    assert_eq!(api.calls("connect"), 0);

    // A session Telegram ended is a sign-in error too.
    api.state().revoked = true;
    let error = run.run(&c, &signed_in_secrets()).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
}

#[test]
fn a_build_without_an_app_id_says_so() {
    let api = MockApi::new();
    api.state().app = false;
    let (c, _) = connector(&api);
    let error = Run::new(History::Everything)
        .run(&c, &signed_in_secrets())
        .unwrap_err();
    assert!(matches!(error, SourceError::Unavailable(_)), "{error}");
    assert!(error.to_string().contains("no Telegram app id"), "{error}");
    let error = c
        .sign_in(
            SignInStep::Start {
                account: PHONE.into(),
            },
            &MemorySecrets::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("no Telegram app id"), "{error}");
}

#[test]
fn catches_up_then_reads_live_messages_without_requests() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.chat(20, "Grace", ChatKind::Personal);
    api.post(10, 1, T0 + 60, "Ada", "Shall we read the lighthouse novel?");
    api.post(10, 2, T0 + 120, "Grace", "Yes!");
    api.post(20, 1, T0 + 180, "Grace", "Lunch?");
    let (c, _) = connector(&api);
    let secrets = signed_in_secrets();
    let mut run = Run::new(History::Everything);

    assert_eq!(run.run(&c, &secrets).unwrap(), 3);
    let club = run.node(10).unwrap();
    assert_eq!(club.title, "Telegram - Book Club");
    assert_eq!(run.prop(&club, chat_keys::TYPE).as_deref(), Some("group"));
    assert_eq!(run.prop(&club, chat_keys::MESSAGES).as_deref(), Some("2"));
    assert!(club.body.contains("**Ada**"));
    assert!(club.body.contains("Shall we read the lighthouse novel?"));
    assert_eq!(
        run.node(20).unwrap().title,
        "Telegram - Grace",
        "a personal chat"
    );
    assert_eq!(api.calls("dialogs"), 1);
    assert_eq!(api.calls("history"), 2);

    // Nothing new: no request at all.
    assert_eq!(run.run(&c, &secrets).unwrap(), 0);
    assert_eq!(api.calls("history"), 2);

    // A live message is drained from the buffer, without a request.
    api.arrive(10, 3, T0 + 240, "Linus", "Count me in");
    assert_eq!(run.run(&c, &secrets).unwrap(), 1);
    assert_eq!(api.calls("history"), 2);
    let club = run.node(10).unwrap();
    assert_eq!(run.prop(&club, chat_keys::MESSAGES).as_deref(), Some("3"));
    assert_eq!(
        run.prop(&club, chat_keys::PARTICIPANTS).as_deref(),
        Some("Ada, Grace, Linus"),
        "who wrote before stays"
    );
    assert_eq!(club.body.matches("Count me in").count(), 1);
}

#[test]
fn a_restart_catches_up_from_the_cursor_without_duplicates() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.post(10, 1, T0 + 60, "Ada", "one");
    api.post(10, 2, T0 + 120, "Grace", "two");
    let secrets = signed_in_secrets();
    let mut run = Run::new(History::Everything);
    {
        let (c, _) = connector(&api);
        assert_eq!(run.run(&c, &secrets).unwrap(), 2);
    }
    // While the service was not running.
    api.post(10, 3, T0 + 180, "Ada", "three");
    // A message that also arrives live once the service is back.
    api.arrive(10, 4, T0 + 240, "Grace", "four");
    let (c, _) = connector(&api);
    assert_eq!(run.run(&c, &secrets).unwrap(), 2);
    let node = run.node(10).unwrap();
    assert_eq!(run.prop(&node, chat_keys::MESSAGES).as_deref(), Some("4"));
    for text in ["one", "two", "three", "four"] {
        assert_eq!(
            node.body.matches(&format!(": {text}")).count(),
            1,
            "{text} once"
        );
    }
    let history_calls = api.calls("history");
    assert_eq!(history_calls, 2, "one read before, one after the restart");

    // The same messages again (a lost cursor, a second device): nothing added.
    run.cursor = None;
    assert_eq!(run.run(&c, &secrets).unwrap(), 0);
    let node = run.node(10).unwrap();
    assert_eq!(run.prop(&node, chat_keys::MESSAGES).as_deref(), Some("4"));

    // A gap in the live updates catches up again.
    api.post(10, 5, T0 + 300, "Ada", "five");
    api.state().gap = true;
    assert_eq!(run.run(&c, &secrets).unwrap(), 1);
}

#[test]
fn excluded_chats_are_never_read() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.chat(20, "Family", ChatKind::Group);
    api.post(10, 1, T0 + 60, "Ada", "books");
    api.post(20, 1, T0 + 60, "Mum", "dinner");
    let (c, _) = connector(&api);
    let secrets = signed_in_secrets();
    let mut run = Run::new(History::Everything);
    run.excluded = vec![Container::new("20", "Family")];

    assert_eq!(run.run(&c, &secrets).unwrap(), 1);
    assert!(run.node(20).is_none());
    assert_eq!(api.calls("history"), 1, "the excluded chat is not read");
    api.arrive(20, 2, T0 + 120, "Mum", "still dinner");
    assert_eq!(run.run(&c, &secrets).unwrap(), 0);
    assert!(run.node(20).is_none());

    // The chats to exclude, by the id the export uses.
    let space_id = run.space.manifest().id.clone();
    let request = FetchRequest {
        space_id: &space_id,
        cursor: None,
        history: History::Everything,
        since: run.since,
        excluded: &[],
        max_items: 10,
        secrets: &secrets,
        media_max_bytes: None,
    };
    let containers = c.containers(&request).unwrap();
    assert_eq!(
        containers,
        vec![
            Container::new("10", "Book Club"),
            Container::new("20", "Family")
        ]
    );
}

#[test]
fn from_today_leaves_out_older_history_and_everything_reads_it_in_batches() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.chat(20, "Old friends", ChatKind::Group);
    for id in 1..=250 {
        api.post(10, id, T0 - 86_400 + id, "Ada", &format!("old {id}"));
    }
    api.post(10, 251, T0 + 60, "Grace", "today");
    api.post(20, 1, T0 - 86_400, "Linus", "long ago");
    let secrets = signed_in_secrets();

    let (c, _) = connector(&api);
    let mut today = Run::new(History::FromToday);
    assert_eq!(today.run(&c, &secrets).unwrap(), 1);
    let node = today.node(10).unwrap();
    assert!(node.body.contains("today"));
    assert!(!node.body.contains("old 1"));
    assert!(today.node(20).is_none());
    assert_eq!(
        api.calls("history"),
        1,
        "a chat with nothing since today is not read"
    );

    let (c, _) = connector(&api);
    let mut all = Run::new(History::Everything);
    all.max_items = 100;
    assert_eq!(all.run(&c, &secrets).unwrap(), 252);
    let node = all.node(10).unwrap();
    assert_eq!(all.prop(&node, chat_keys::MESSAGES).as_deref(), Some("251"));
    assert_eq!(node.body.matches(": old 1\n").count(), 1);
    assert!(all.node(20).is_some());
}

#[test]
fn short_flood_waits_are_slept_and_long_ones_stop_asking() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.post(10, 1, T0 + 60, "Ada", "one");
    let secrets = signed_in_secrets();
    let (c, slept) = connector(&api);
    let mut run = Run::new(History::Everything);

    api.state().floods.push_back(Duration::from_secs(3));
    assert_eq!(run.run(&c, &secrets).unwrap(), 1);
    assert_eq!(*slept.0.lock().unwrap(), vec![Duration::from_secs(3)]);

    let (c, slept) = connector(&api);
    api.state().floods.push_back(Duration::from_secs(3600));
    let calls = api.state().calls.len();
    let error = run.run(&c, &secrets).unwrap_err();
    assert!(matches!(error, SourceError::Failed(_)), "{error}");
    assert!(error.to_string().contains("3600 s"), "{error}");
    assert!(slept.0.lock().unwrap().is_empty(), "not slept inside a run");
    let after = api.state().calls.len();
    assert_eq!(after, calls + 1);
    // Until the wait is over Telegram is not asked anything.
    assert!(run.run(&c, &secrets).is_err());
    assert_eq!(api.state().calls.len(), after);
}

#[test]
fn formatting_replies_and_media_come_in_like_an_export() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    let mut first = message(10, 1, T0 + 60, "Ada", "Read the lighthouse novel");
    first.entities = vec![Entity {
        kind: EntityKind::Bold,
        offset: 9,
        length: 10,
        href: None,
        language: None,
    }];
    let mut reply = message(10, 2, T0 + 120, "Grace", "Yes");
    reply.reply_to = Some(1);
    let mut photo = message(10, 3, T0 + 180, "Ada", "The cover");
    photo.media = Some(MessageMedia::Photo { size: Some(4) });
    let mut big = message(10, 4, T0 + 240, "Ada", "");
    big.media = Some(MessageMedia::File {
        name: Some("film.mp4".into()),
        what: "video".into(),
        size: Some(1 << 40),
    });
    let mut service = message(10, 5, T0 + 300, "", "");
    service.sender = None;
    service.service = Some("Ada added Linus".into());
    {
        let mut s = api.state();
        s.history
            .insert(10, vec![first, reply, photo, big, service]);
        s.dialogs[0].top_message = 5;
        s.dialogs[0].top_date = T0 + 300;
        s.files.insert((10, 3), b"jpeg".to_vec());
    }
    let (c, _) = connector(&api);
    let mut run = Run::new(History::Everything);
    assert_eq!(run.run(&c, &signed_in_secrets()).unwrap(), 5);
    let node = run.node(10).unwrap();
    assert!(
        node.body.contains("Read the **lighthouse** novel"),
        "{}",
        node.body
    );
    assert!(node.body.contains("replying to Ada at"), "{}", node.body);
    assert!(node.body.contains("![[photo_3.jpg]]"), "{}", node.body);
    assert!(
        node.body
            .contains("*[not downloaded: film.mp4, larger than 20 MB]*"),
        "{}",
        node.body
    );
    assert!(node.body.contains("Ada added Linus"), "{}", node.body);
    let attachments = run.space.attachments(&node.id).unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(api.calls("download"), 1, "the large file is not downloaded");
}

#[test]
fn the_media_limit_of_the_space_is_read_and_named_without_an_export() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    let mut photo = message(10, 1, T0 + 60, "Ada", "The cover");
    photo.media = Some(MessageMedia::Photo { size: Some(4) });
    let mut card = message(10, 2, T0 + 120, "Ada", "");
    card.media = Some(MessageMedia::Other {
        what: "contact".into(),
    });
    {
        let mut s = api.state();
        s.history.insert(10, vec![photo, card]);
        s.dialogs[0].top_message = 2;
        s.dialogs[0].top_date = T0 + 120;
        s.files.insert((10, 1), b"jpeg".to_vec());
    }
    let (c, _) = connector(&api);
    let mut run = Run::new(History::Everything);
    run.media_max_bytes = Some(0);
    assert_eq!(run.run(&c, &signed_in_secrets()).unwrap(), 2);
    assert_eq!(api.calls("download"), 0, "media downloads are off here");
    let node = run.node(10).unwrap();
    assert!(
        node.body
            .contains(r"*[not downloaded: photo\_1.jpg, media downloads are off]*"),
        "{}",
        node.body
    );
    assert!(
        node.body.contains("*[not downloaded: contact]*"),
        "{}",
        node.body
    );
    assert!(!node.body.contains("export"), "{}", node.body);
    assert!(c.info().media, "Telegram says it downloads media");
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/chats")
        .join(name)
}

#[test]
fn shares_the_node_of_a_telegram_desktop_export() {
    let mut run = Run::new(History::Everything);
    let report = knode_ingest::import_chat_export(
        &mut run.space,
        fixture("telegram-chat"),
        None,
        &knode_format::Provenance::person("example"),
    )
    .unwrap();
    assert_eq!(report.messages, 7);

    // The export's chat, as the API has it: the same ids, and two messages more.
    let api = MockApi::new();
    api.chat(1_000_000_001, "Book Club", ChatKind::Group);
    for id in 1..=7 {
        api.post(1_000_000_001, id, T0, "Ada", "already in the export");
    }
    api.post(1_000_000_001, 8, T0 + 60, "Grace", "after the export");
    api.post(1_000_000_001, 9, T0 + 120, "Linus", "and another");
    let (c, _) = connector(&api);
    assert_eq!(run.run(&c, &signed_in_secrets()).unwrap(), 2);
    let node = run.node(1_000_000_001).unwrap();
    assert_eq!(node.title, "Telegram - Book Club");
    assert_eq!(run.prop(&node, chat_keys::MESSAGES).as_deref(), Some("9"));
    assert!(!node.body.contains("already in the export"));
    assert_eq!(node.body.matches("after the export").count(), 1);
    assert_eq!(
        run.prop(&node, chat_keys::PARTICIPANTS).as_deref(),
        Some("Ada, Grace, Linus"),
        "the export's participants stay"
    );
}

#[test]
fn signing_out_does_not_wait_for_a_run() {
    let api = MockApi::new();
    api.chat(10, "Book Club", ChatKind::Group);
    api.post(10, 1, T0 + 60, "Ada", "one");
    let (release, hold) = std::sync::mpsc::channel();
    api.state().hold = Some(hold);
    let (c, _) = connector(&api);
    let c = Arc::new(c);
    let secrets = Arc::new(signed_in_secrets());
    let run = {
        let (c, secrets) = (c.clone(), secrets.clone());
        std::thread::spawn(move || {
            let mut run = Run::new(History::Everything);
            run.run(&c, secrets.as_ref())
        })
    };
    // Wait until the run is inside Telegram's answer.
    while api.state().hold.is_some() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (done, signed_out) = std::sync::mpsc::channel();
    {
        let (c, secrets) = (c.clone(), secrets.clone());
        std::thread::spawn(move || {
            let _ = done.send(c.sign_out(secrets.as_ref()).unwrap());
        });
    }
    let was = signed_out
        .recv_timeout(Duration::from_secs(5))
        .expect("sign out finished while the run waited for Telegram");
    assert!(was);
    assert!(secrets.get(SESSION_SECRET).unwrap().is_none());
    release.send(()).unwrap();
    let _ = run.join().unwrap();
    assert_eq!(
        c.sign_in_state(secrets.as_ref()).unwrap(),
        SignInState::SignedOut,
        "the run that started before does not sign it in again"
    );
}

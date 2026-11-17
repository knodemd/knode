//! The Slack connector against a fake Slack: a synthetic workspace held in memory, answering
//! the Web API methods the connector calls as Slack documents them. No network is used; the
//! sign-in tests reach only a loopback port of this machine.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use knode_format::{Node, Space, Timestamp, link_kinds};
use knode_ingest::slack::{Clock, HttpResponse, Limits, SlackConfig, SlackConnector, SlackHttp};
use knode_ingest::sources::{
    Connector, Container, Cursor, FetchRequest, History, Secrets, SourceError, provenance,
};
use knode_ingest::{ChatFormat, PERSON_KIND, chat_keys, import_chat_export, person_keys};
use serde_json::{Value, json};
use tempfile::TempDir;

// ---------------------------------------------------------------------------------------------
// A fake Slack.

/// A call the connector made.
#[derive(Debug, Clone)]
struct Call {
    method: String,
    params: BTreeMap<String, String>,
    token: Option<String>,
    at: Duration,
}

#[derive(Default)]
struct Workspace {
    users: Vec<Value>,
    conversations: Vec<Value>,
    /// Top-level messages by channel.
    history: BTreeMap<String, Vec<Value>>,
    /// Replies by `<channel>/<thread ts>`.
    replies: BTreeMap<String, Vec<Value>>,
    /// Answers given before any other, by method: `429`s, errors.
    queued: BTreeMap<String, Vec<HttpResponse>>,
    calls: Vec<Call>,
    /// The token the fake accepts.
    token: String,
    /// The page size of the lists.
    list_page: usize,
}

struct FakeSlack {
    workspace: Mutex<Workspace>,
    clock: Arc<FakeClock>,
}

fn ok(mut value: Value) -> HttpResponse {
    value["ok"] = json!(true);
    HttpResponse::ok(value.to_string())
}

fn error(code: &str) -> HttpResponse {
    HttpResponse::ok(json!({"ok": false, "error": code}).to_string())
}

fn ts_key(ts: &str) -> String {
    format!("{ts:0>20}")
}

impl FakeSlack {
    fn new(clock: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(FakeSlack {
            workspace: Mutex::new(Workspace {
                token: "xoxp-synthetic".into(),
                list_page: 200,
                ..Workspace::default()
            }),
            clock,
        })
    }

    fn with<R>(&self, f: impl FnOnce(&mut Workspace) -> R) -> R {
        f(&mut self.workspace.lock().unwrap())
    }

    fn calls(&self, method: &str) -> Vec<Call> {
        self.with(|w| {
            w.calls
                .iter()
                .filter(|c| c.method == method)
                .cloned()
                .collect()
        })
    }

    fn queue(&self, method: &str, response: HttpResponse) {
        self.with(|w| {
            w.queued
                .entry(method.to_string())
                .or_default()
                .push(response)
        });
    }

    fn post_message(&self, channel: &str, message: Value) {
        self.with(|w| match message["thread_ts"].as_str() {
            Some(thread) if thread != message["ts"].as_str().unwrap() => {
                w.replies
                    .entry(format!("{channel}/{thread}"))
                    .or_default()
                    .push(message);
            }
            _ => w
                .history
                .entry(channel.to_string())
                .or_default()
                .push(message),
        });
    }

    fn page<T: Clone>(
        items: &[T],
        params: &BTreeMap<String, String>,
        size: usize,
    ) -> (Vec<T>, String) {
        let start: usize = params
            .get("cursor")
            .and_then(|c| c.strip_prefix("page-"))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let end = (start + size).min(items.len());
        let next = if end < items.len() {
            format!("page-{end}")
        } else {
            String::new()
        };
        (items[start..end].to_vec(), next)
    }

    fn answer(
        &self,
        w: &mut Workspace,
        method: &str,
        token: Option<&str>,
        p: &BTreeMap<String, String>,
    ) -> HttpResponse {
        if let Some(queued) = w.queued.get_mut(method)
            && !queued.is_empty()
        {
            return queued.remove(0);
        }
        if method == "oauth.v2.access" {
            return match p.get("grant_type").map(String::as_str) {
                Some("authorization_code")
                    if p.get("code").map(String::as_str) == Some("good-code") =>
                {
                    ok(json!({
                        "app_id": "A01",
                        "authed_user": {"id": "U01", "scope": "channels:history", "access_token": w.token,
                            "token_type": "user", "refresh_token": "xoxe-1-refresh", "expires_in": 43200},
                        "team": {"id": "T01", "name": "Example Team"},
                        "is_enterprise_install": false
                    }))
                }
                Some("refresh_token")
                    if p.get("refresh_token").map(String::as_str) == Some("xoxe-1-refresh") =>
                {
                    w.token = "xoxp-refreshed".into();
                    ok(json!({
                        "app_id": "A01",
                        "authed_user": {"id": "U01", "access_token": "xoxp-refreshed", "token_type": "user",
                            "refresh_token": "xoxe-1-refresh-2", "expires_in": 43200},
                        "team": {"id": "T01", "name": "Example Team"}
                    }))
                }
                Some("refresh_token") => error("invalid_refresh_token"),
                _ => error("invalid_code"),
            };
        }
        if token != Some(w.token.as_str()) {
            return error("invalid_auth");
        }
        match method {
            "auth.revoke" => ok(json!({"revoked": true})),
            "users.list" => {
                let (members, next) = Self::page(&w.users, p, w.list_page);
                ok(json!({"members": members, "response_metadata": {"next_cursor": next}}))
            }
            "users.info" => {
                let id = p.get("user").cloned().unwrap_or_default();
                match w.users.iter().find(|u| u["id"] == json!(id)) {
                    Some(u) => ok(json!({"user": u})),
                    None => error("user_not_found"),
                }
            }
            "conversations.list" => {
                assert_eq!(
                    p.get("types").map(String::as_str),
                    Some("public_channel,private_channel,mpim,im")
                );
                let (channels, next) = Self::page(&w.conversations, p, w.list_page);
                ok(json!({"channels": channels, "response_metadata": {"next_cursor": next}}))
            }
            "conversations.history" => {
                let channel = p.get("channel").cloned().unwrap_or_default();
                if !w.conversations.iter().any(|c| c["id"] == json!(channel)) {
                    return error("channel_not_found");
                }
                let oldest = p.get("oldest").map(|o| ts_key(o)).unwrap_or_default();
                let latest = p.get("latest").map(|l| ts_key(l));
                let limit: usize = p.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
                let mut messages: Vec<Value> = w
                    .history
                    .get(&channel)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|m| {
                        let ts = ts_key(m["ts"].as_str().unwrap());
                        ts > oldest && latest.as_ref().is_none_or(|l| &ts < l)
                    })
                    .collect();
                messages.sort_by_key(|m| std::cmp::Reverse(ts_key(m["ts"].as_str().unwrap())));
                let has_more = messages.len() > limit;
                messages.truncate(limit);
                for m in &mut messages {
                    let key = format!("{channel}/{}", m["ts"].as_str().unwrap());
                    if let Some(replies) = w.replies.get(&key) {
                        m["thread_ts"] = m["ts"].clone();
                        m["reply_count"] = json!(replies.len());
                        m["latest_reply"] = replies.last().unwrap()["ts"].clone();
                    }
                }
                ok(json!({"messages": messages, "has_more": has_more}))
            }
            "conversations.replies" => {
                let channel = p.get("channel").cloned().unwrap_or_default();
                let thread = p.get("ts").cloned().unwrap_or_default();
                let Some(parent) = w
                    .history
                    .get(&channel)
                    .and_then(|h| h.iter().find(|m| m["ts"] == json!(thread)))
                    .cloned()
                else {
                    return error("thread_not_found");
                };
                let oldest = p.get("oldest").map(|o| ts_key(o)).unwrap_or_default();
                let limit: usize = p.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
                let replies: Vec<Value> = w
                    .replies
                    .get(&format!("{channel}/{thread}"))
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|m| ts_key(m["ts"].as_str().unwrap()) > oldest)
                    .collect();
                let mut parent = parent;
                parent["thread_ts"] = json!(thread);
                // Slack gives the message that starts the thread first, then the replies.
                let mut messages = vec![parent];
                messages.extend(replies);
                let has_more = messages.len() > limit;
                messages.truncate(limit);
                ok(json!({"messages": messages, "has_more": has_more}))
            }
            other => panic!("the connector called {other}, which it never should"),
        }
    }
}

impl SlackHttp for FakeSlack {
    fn post(
        &self,
        method: &str,
        token: Option<&str>,
        params: &[(&str, String)],
    ) -> io::Result<HttpResponse> {
        let p: BTreeMap<String, String> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let at = self.clock.elapsed();
        let mut w = self.workspace.lock().unwrap();
        w.calls.push(Call {
            method: method.to_string(),
            params: p.clone(),
            token: token.map(str::to_string),
            at,
        });
        Ok(self.answer(&mut w, method, token, &p))
    }
}

/// A clock that moves only when the connector waits or the test says so.
struct FakeClock {
    elapsed: Mutex<Duration>,
    unix: Mutex<Duration>,
}

impl FakeClock {
    fn new(unix_secs: u64) -> Arc<Self> {
        Arc::new(FakeClock {
            elapsed: Mutex::new(Duration::from_secs(1000)),
            unix: Mutex::new(Duration::from_secs(unix_secs)),
        })
    }

    fn advance(&self, by: Duration) {
        *self.elapsed.lock().unwrap() += by;
        *self.unix.lock().unwrap() += by;
    }
}

impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        *self.elapsed.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }

    fn unix(&self) -> Duration {
        *self.unix.lock().unwrap()
    }
}

/// Secrets in memory, standing in for the keychain.
#[derive(Default)]
struct MemorySecrets(Mutex<BTreeMap<String, String>>);

impl Secrets for MemorySecrets {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }

    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, name: &str) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().remove(name).is_some())
    }
}

// ---------------------------------------------------------------------------------------------
// The synthetic workspace: invented people and channels.

/// 2026-03-04 10:00 UTC.
const DAY: u64 = 1_772_618_400;
/// When the tests start: two days later.
const NOW: u64 = DAY + 2 * 24 * 3600;

fn user(id: &str, name: &str, real: &str) -> Value {
    json!({"id": id, "team_id": "T01", "name": name, "real_name": real,
        "profile": {"real_name": real, "display_name": name}})
}

fn msg(user: &str, text: &str, ts: &str) -> Value {
    json!({"type": "message", "user": user, "text": text, "ts": ts})
}

fn reply(user: &str, text: &str, ts: &str, thread: &str) -> Value {
    json!({"type": "message", "user": user, "text": text, "ts": ts, "thread_ts": thread,
        "parent_user_id": "U01"})
}

/// `ts` of a message `secs` seconds after [`DAY`].
fn ts(secs: u64, n: u32) -> String {
    format!("{}.{n:06}", DAY + secs)
}

fn workspace(slack: &FakeSlack) {
    slack.with(|w| {
        w.users = vec![
            user("U01", "ada", "Ada"),
            user("U02", "grace", "Grace"),
            user("U03", "linus", "Linus"),
            user("U04", "mira", "Mira"),
        ];
        w.conversations = vec![
            json!({"id": "C01", "name": "general", "is_channel": true, "is_member": true}),
            json!({"id": "C02", "name": "plans", "is_group": true, "is_private": true, "is_member": true}),
            json!({"id": "C08", "name": "elsewhere", "is_channel": true, "is_member": false}),
            json!({"id": "C09", "name": "excluded", "is_channel": true, "is_member": true}),
            json!({"id": "D01", "is_im": true, "user": "U02"}),
            json!({"id": "G01", "name": "mpdm-ada--grace--linus-1", "is_mpim": true, "is_private": true}),
        ];
    });
    slack.post_message("C01", msg("U01", "Plan for *Friday*, <@U03>?", &ts(0, 100)));
    slack.post_message(
        "C01",
        reply("U02", "Works for me", &ts(60, 200), &ts(0, 100)),
    );
    slack.post_message("C01", msg("U02", "Here is the map", &ts(120, 300)));
    slack.post_message(
        "C01",
        reply("U03", "Late reply, sorry", &ts(3600, 100), &ts(0, 100)),
    );
    slack.post_message("C02", msg("U01", "Budget draft", &ts(30, 1)));
    slack.post_message("C08", msg("U04", "Not for Ada", &ts(30, 2)));
    slack.post_message("C09", msg("U04", "Never read", &ts(30, 3)));
    slack.post_message("D01", msg("U02", "Lunch?", &ts(40, 4)));
    slack.post_message("G01", msg("U03", "Hi both", &ts(50, 5)));
}

struct Setup {
    _dir: TempDir,
    space: Space,
    slack: Arc<FakeSlack>,
    clock: Arc<FakeClock>,
    secrets: MemorySecrets,
    cursor: Option<Cursor>,
    excluded: Vec<Container>,
    history: History,
    since: Timestamp,
}

fn token_json(token: &str) -> String {
    json!({"access_token": token, "user_id": "U01", "team_id": "T01", "team_name": "Example Team"})
        .to_string()
}

fn setup(limits: Limits) -> (Setup, SlackConnector) {
    let dir = tempfile::tempdir().unwrap();
    let space = Space::create(dir.path().join("Test.knode"), "Test").unwrap();
    let clock = FakeClock::new(NOW);
    let slack = FakeSlack::new(clock.clone());
    workspace(&slack);
    let secrets = MemorySecrets::default();
    secrets.set("token", &token_json("xoxp-synthetic")).unwrap();
    let connector = connector(&slack, &clock, limits);
    (
        Setup {
            _dir: dir,
            space,
            slack,
            clock,
            secrets,
            cursor: None,
            excluded: vec![Container::new("C09", "#excluded")],
            history: History::Everything,
            since: Timestamp(0),
        },
        connector,
    )
}

fn connector(slack: &Arc<FakeSlack>, clock: &Arc<FakeClock>, limits: Limits) -> SlackConnector {
    SlackConnector::new(
        SlackConfig {
            client_id: Some("1234.5678".into()),
            limits,
        },
        slack.clone(),
        clock.clone(),
    )
}

impl Setup {
    fn request(&self) -> FetchRequest<'_> {
        FetchRequest {
            space_id: "s",
            cursor: self.cursor.as_ref(),
            history: self.history,
            since: self.since,
            excluded: &self.excluded,
            max_items: 500,
            secrets: &self.secrets,
        }
    }

    /// One run as Knode Service makes it: fetches and writes until the connector has nothing
    /// more now. Returns the messages written.
    fn run(&mut self, connector: &SlackConnector) -> Result<usize, SourceError> {
        let mut items = 0;
        for _ in 0..20 {
            let fetched = connector.fetch(&self.request())?;
            if let Some(batch) = fetched.batch {
                let written = batch.write(&mut self.space, &provenance("slack"))?;
                items += written.items;
            }
            self.cursor = Some(fetched.cursor);
            if !fetched.more {
                break;
            }
        }
        Ok(items)
    }

    /// Runs once a minute until a run brings nothing and nothing is left to read.
    fn sync(&mut self, connector: &SlackConnector, runs: usize) -> usize {
        let mut items = 0;
        for _ in 0..runs {
            items += self.run(connector).unwrap();
            self.clock.advance(Duration::from_secs(61));
        }
        items
    }

    fn chat(&self, id: &str) -> Option<Node> {
        for summary in self.space.find_by_property(chat_keys::ID, id).unwrap() {
            if self
                .space
                .property(&summary.id, chat_keys::APP)
                .unwrap()
                .as_deref()
                == Some("slack")
            {
                return self.space.get_node(&summary.id).unwrap();
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Signing in.

#[test]
fn a_build_without_a_slack_app_says_so() {
    let clock = FakeClock::new(DAY);
    let slack = FakeSlack::new(clock.clone());
    let connector = SlackConnector::new(SlackConfig::default(), slack.clone(), clock);
    let error = connector.check().unwrap_err();
    assert!(matches!(error, SourceError::Unavailable(_)), "{error}");
    assert!(error.to_string().contains("no Slack app"), "{error}");
    assert!(
        error.to_string().contains("KNODE_SLACK_CLIENT_ID"),
        "{error}"
    );
    assert!(connector.begin_sign_in_on(&[0]).is_err());
    let secrets = MemorySecrets::default();
    let request = FetchRequest {
        space_id: "s",
        cursor: None,
        history: History::FromToday,
        since: Timestamp(0),
        excluded: &[],
        max_items: 10,
        secrets: &secrets,
    };
    assert!(matches!(
        connector.fetch(&request),
        Err(SourceError::Unavailable(_))
    ));
    assert!(slack.with(|w| w.calls.is_empty()));
}

#[test]
fn reading_without_signing_in_asks_to_sign_in() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.secrets.delete("token").unwrap();
    let error = setup.run(&connector).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
    // A token Slack refuses asks the same.
    setup
        .secrets
        .set("token", &token_json("xoxp-revoked"))
        .unwrap();
    let error = setup.run(&connector).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
}

/// The browser coming back from Slack to `redirect`, with `query`. Returns the status line.
fn browser(redirect: &str, query: &str) -> String {
    let rest = redirect.strip_prefix("http://localhost:").unwrap();
    let (port, path) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    write!(
        stream,
        "GET /{path}?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer.lines().next().unwrap_or_default().to_string()
}

fn query_param(url: &str, name: &str) -> String {
    let query = url.split_once('?').unwrap().1;
    query
        .split('&')
        .find_map(|p| p.strip_prefix(&format!("{name}=")))
        .unwrap()
        .to_string()
}

#[test]
fn signs_in_with_pkce_and_no_client_secret() {
    let clock = FakeClock::new(DAY);
    let slack = FakeSlack::new(clock.clone());
    let connector = connector(&slack, &clock, Limits::NotListed);
    let secrets = MemorySecrets::default();

    let pending = connector.begin_sign_in_on(&[0]).unwrap();
    let url = pending.url().to_string();
    assert!(
        url.starts_with("https://slack.com/oauth/v2/authorize?"),
        "{url}"
    );
    assert_eq!(query_param(&url, "client_id"), "1234.5678");
    assert_eq!(query_param(&url, "code_challenge_method"), "S256");
    assert_eq!(query_param(&url, "scope"), "", "no bot scopes");
    let scopes = query_param(&url, "user_scope")
        .replace("%3A", ":")
        .replace("%2C", ",");
    assert_eq!(
        scopes,
        "channels:history,groups:history,im:history,mpim:history,channels:read,groups:read,\
         im:read,mpim:read,users:read"
    );
    assert!(!url.contains("secret"));
    let state = query_param(&url, "state");
    let challenge = query_param(&url, "code_challenge");
    let redirect = pending.redirect_uri().to_string();
    assert!(redirect.starts_with("http://localhost:"), "{redirect}");
    assert!(redirect.ends_with("/slack/callback"));

    let browser_thread = std::thread::spawn({
        let redirect = redirect.clone();
        move || {
            // Something else on the port, and a callback of another sign-in: ignored.
            let other = browser(&redirect.replace("/slack/callback", "/favicon.ico"), "");
            let forged = browser(&redirect, "code=evil&state=not-ours");
            let real = browser(&redirect, &format!("code=good-code&state={state}"));
            (other, forged, real)
        }
    });
    let account = connector
        .finish_sign_in(pending, &secrets, Duration::from_secs(20))
        .unwrap();
    let (other, forged, real) = browser_thread.join().unwrap();
    assert!(other.contains("404"), "{other}");
    assert!(forged.contains("400"), "{forged}");
    assert!(real.contains("200"), "{real}");
    assert_eq!(account.team_id, "T01");
    assert_eq!(account.team_name.as_deref(), Some("Example Team"));
    assert_eq!(account.user_id, "U01");

    // One exchange, of the real code, with the verifier of the challenge and no secret.
    let exchanges = slack.calls("oauth.v2.access");
    assert_eq!(exchanges.len(), 1);
    let p = &exchanges[0].params;
    assert_eq!(p["code"], "good-code");
    assert_eq!(p["client_id"], "1234.5678");
    assert_eq!(p["redirect_uri"], redirect);
    assert!(!p.contains_key("client_secret"));
    assert!(exchanges[0].token.is_none());
    use base64::Engine;
    use sha2::Digest;
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(p["code_verifier"].as_bytes()));
    assert_eq!(expected, challenge);

    // The token is kept with the secrets, and only there.
    let kept = secrets.get("token").unwrap().unwrap();
    assert!(kept.contains("xoxp-synthetic"));
    assert!(!format!("{account:?}").contains("xoxp"));
    assert_eq!(connector.account(&secrets).unwrap(), Some(account));

    // Signing out revokes the token and forgets it.
    assert!(connector.sign_out(&secrets, true).unwrap());
    assert_eq!(slack.calls("auth.revoke").len(), 1);
    assert_eq!(secrets.get("token").unwrap(), None);
    assert!(!connector.sign_out(&secrets, true).unwrap());
}

#[test]
fn a_refused_sign_in_keeps_nothing() {
    let clock = FakeClock::new(DAY);
    let slack = FakeSlack::new(clock.clone());
    let connector = connector(&slack, &clock, Limits::NotListed);
    let secrets = MemorySecrets::default();

    let pending = connector.begin_sign_in_on(&[0]).unwrap();
    let state = query_param(pending.url(), "state");
    let redirect = pending.redirect_uri().to_string();
    let t = std::thread::spawn(move || {
        browser(&redirect, &format!("error=access_denied&state={state}"))
    });
    let error = connector
        .finish_sign_in(pending, &secrets, Duration::from_secs(20))
        .unwrap_err();
    t.join().unwrap();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
    assert!(error.to_string().contains("access_denied"), "{error}");

    // A code Slack refuses.
    let pending = connector.begin_sign_in_on(&[0]).unwrap();
    let state = query_param(pending.url(), "state");
    let redirect = pending.redirect_uri().to_string();
    let t = std::thread::spawn(move || browser(&redirect, &format!("code=bad&state={state}")));
    let error = connector
        .finish_sign_in(pending, &secrets, Duration::from_secs(20))
        .unwrap_err();
    t.join().unwrap();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");

    // Nobody comes back.
    let pending = connector.begin_sign_in_on(&[0]).unwrap();
    let error = connector
        .finish_sign_in(pending, &secrets, Duration::from_millis(200))
        .unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
    assert_eq!(secrets.get("token").unwrap(), None);
}

#[test]
fn refreshes_a_rotating_token_before_it_expires() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    let unix = setup.clock.unix().as_secs();
    setup
        .secrets
        .set(
            "token",
            &json!({"access_token": "xoxp-synthetic", "refresh_token": "xoxe-1-refresh",
                "expires_at": unix + 60, "user_id": "U01", "team_id": "T01"})
            .to_string(),
        )
        .unwrap();
    setup.run(&connector).unwrap();
    let refreshes = setup.slack.calls("oauth.v2.access");
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0].params["grant_type"], "refresh_token");
    assert!(!refreshes[0].params.contains_key("client_secret"));
    let kept = setup.secrets.get("token").unwrap().unwrap();
    assert!(kept.contains("xoxp-refreshed") && kept.contains("xoxe-1-refresh-2"));
    // Every call after it used the new token.
    let history = setup.slack.calls("conversations.history");
    assert!(!history.is_empty());
    assert!(
        history
            .iter()
            .all(|c| c.token.as_deref() == Some("xoxp-refreshed"))
    );

    // A refresh token Slack no longer takes asks to sign in again.
    setup
        .secrets
        .set(
            "token",
            &json!({"access_token": "xoxp-old", "refresh_token": "xoxe-expired",
                "expires_at": unix, "user_id": "U01", "team_id": "T01"})
            .to_string(),
        )
        .unwrap();
    let error = setup.run(&connector).unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
}

// ---------------------------------------------------------------------------------------------
// Reading.

#[test]
fn brings_in_channels_threads_and_people_like_the_importer() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    let items = setup.sync(&connector, 3);
    assert_eq!(
        items, 8,
        "every message but the excluded and foreign channels'"
    );

    let general = setup.chat("C01").expect("#general");
    assert_eq!(general.title, "Slack - general");
    assert_eq!(
        setup
            .space
            .property(&general.id, chat_keys::TYPE)
            .unwrap()
            .as_deref(),
        Some("channel")
    );
    assert!(
        general.body.contains("Plan for **Friday**, [[Linus]]?"),
        "{}",
        general.body
    );
    assert!(general.body.contains("Here is the map"));
    assert!(
        !general.body.contains("Works for me"),
        "replies stay in the thread"
    );

    let thread = setup
        .chat(&format!("C01/{}", ts(0, 100)))
        .expect("the thread");
    assert_eq!(
        setup
            .space
            .property(&thread.id, chat_keys::TYPE)
            .unwrap()
            .as_deref(),
        Some("thread")
    );
    assert!(
        thread
            .title
            .starts_with("Slack - general - Plan for Friday, Linus?"),
        "{}",
        thread.title
    );
    assert!(
        general
            .body
            .contains(&format!("Thread: [[{}]]", thread.title)),
        "{}",
        general.body
    );
    assert!(thread.body.contains("Works for me") && thread.body.contains("Late reply, sorry"));
    let links = setup.space.links_from(&thread.id).unwrap();
    assert!(
        links
            .iter()
            .any(|l| l.kind == link_kinds::PART_OF && l.to == general.id),
        "{links:?}"
    );

    let private = setup.chat("C02").expect("the private channel");
    assert_eq!(
        setup
            .space
            .property(&private.id, chat_keys::TYPE)
            .unwrap()
            .as_deref(),
        Some("private-channel")
    );
    let dm = setup.chat("D01").expect("the direct message");
    assert_eq!(dm.title, "Slack - Grace, Ada");
    assert_eq!(
        setup
            .space
            .property(&dm.id, chat_keys::TYPE)
            .unwrap()
            .as_deref(),
        Some("personal")
    );
    let group = setup.chat("G01").expect("the group message");
    assert_eq!(group.title, "Slack - Ada, Grace, Linus");
    assert_eq!(
        setup
            .space
            .property(&group.id, chat_keys::TYPE)
            .unwrap()
            .as_deref(),
        Some("group")
    );

    // Not in it, or excluded: never read.
    assert!(setup.chat("C08").is_none());
    assert!(setup.chat("C09").is_none());
    for call in setup.slack.with(|w| w.calls.clone()) {
        assert_ne!(
            call.params.get("channel").map(String::as_str),
            Some("C09"),
            "{call:?}"
        );
        assert_ne!(
            call.params.get("channel").map(String::as_str),
            Some("C08"),
            "{call:?}"
        );
    }

    // People: those who wrote or are mentioned, by team and member id; Mira only wrote where
    // Ada is not.
    let ada = setup
        .space
        .find_by_property(person_keys::USER, "T01/U01")
        .unwrap();
    assert_eq!(ada.len(), 1);
    assert_eq!(
        setup.space.get_node(&ada[0].id).unwrap().unwrap().kind,
        PERSON_KIND
    );
    assert!(
        setup
            .space
            .find_by_property(person_keys::USER, "T01/U04")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_restart_adds_nothing_twice() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.sync(&connector, 3);
    let nodes = setup.space.node_count().unwrap();
    let general = setup.chat("C01").unwrap().body;

    // The same cursor, a new process: nothing new.
    let restarted = self::connector(&setup.slack, &setup.clock, Limits::Marketplace);
    assert_eq!(setup.sync(&restarted, 2), 0);
    // The cursor lost (a run cut short before it was saved, another device): everything is
    // read again and nothing is written twice.
    setup.cursor = None;
    let again = self::connector(&setup.slack, &setup.clock, Limits::Marketplace);
    assert_eq!(setup.sync(&again, 3), 0);
    assert_eq!(setup.space.node_count().unwrap(), nodes);
    assert_eq!(setup.chat("C01").unwrap().body, general);

    // New messages and a new reply come in, once.
    setup
        .slack
        .post_message("C01", msg("U03", "Morning _all_", &ts(2 * 24 * 3600, 1)));
    setup.slack.post_message(
        "C01",
        reply("U01", "Done", &ts(2 * 24 * 3600 + 5, 1), &ts(0, 100)),
    );
    setup.clock.advance(Duration::from_secs(3600));
    assert_eq!(setup.sync(&again, 3), 2);
    let thread = setup.chat(&format!("C01/{}", ts(0, 100))).unwrap();
    assert!(thread.body.trim_end().ends_with("Done"), "{}", thread.body);
    assert_eq!(
        setup
            .space
            .property(&thread.id, chat_keys::MESSAGES)
            .unwrap()
            .as_deref(),
        Some("4")
    );
    assert_eq!(setup.sync(&again, 2), 0);
}

#[test]
fn shares_nodes_with_a_slack_export() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    // The export of the same workspace, as an owner would download it, imported first.
    let export = setup._dir.path().join("export");
    let files: [(&str, String); 5] = [
        ("users.json", json!([user("U01", "ada", "Ada"), user("U02", "grace", "Grace"), user("U03", "linus", "Linus")]).to_string()),
        ("channels.json", json!([{"id": "C01", "name": "general"}]).to_string()),
        ("dms.json", json!([{"id": "D01", "members": ["U02", "U01"]}]).to_string()),
        ("general/2026-03-04.json", json!([
            {"type": "message", "user": "U01", "text": "Plan for *Friday*, <@U03>?", "ts": ts(0, 100), "thread_ts": ts(0, 100), "reply_count": 2},
            reply("U02", "Works for me", &ts(60, 200), &ts(0, 100)),
            msg("U02", "Here is the map", &ts(120, 300)),
            reply("U03", "Late reply, sorry", &ts(3600, 100), &ts(0, 100)),
        ]).to_string()),
        ("D01/2026-03-04.json", json!([msg("U02", "Lunch?", &ts(40, 4))]).to_string()),
    ];
    for (name, text) in &files {
        let path = export.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let report = import_chat_export(
        &mut setup.space,
        &export,
        Some(ChatFormat::Slack),
        &provenance("slack"),
    )
    .unwrap();
    assert_eq!(report.chats.len(), 3);
    let general = setup.chat("C01").unwrap();
    let thread = setup.chat(&format!("C01/{}", ts(0, 100))).unwrap();

    // The connector finds the same nodes and people and adds only what the export lacks.
    let items = setup.sync(&connector, 3);
    assert_eq!(items, 2, "the private channel and the group message");
    assert_eq!(setup.chat("C01").unwrap().body, general.body);
    assert_eq!(
        setup.chat(&format!("C01/{}", ts(0, 100))).unwrap().body,
        thread.body
    );
    assert_eq!(
        setup
            .space
            .find_by_property(person_keys::USER, "T01/U01")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        setup
            .space
            .find_by_property(person_keys::USER, "T01/U03")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reads_long_histories_oldest_first_within_the_lower_limits() {
    let (mut setup, connector) = setup(Limits::NotListed);
    setup.excluded = ["C02", "C09", "D01", "G01"]
        .iter()
        .map(|id| Container::new(*id, *id))
        .collect();
    // 40 messages in #general, more than two pages of 15.
    setup.slack.with(|w| w.history.clear());
    setup.slack.with(|w| w.replies.clear());
    for i in 0..40 {
        setup.slack.post_message(
            "C01",
            msg("U01", &format!("Message {i:02}"), &ts(i * 60, 1)),
        );
    }
    // Lists in pages of two.
    setup.slack.with(|w| w.list_page = 2);

    let items = setup.sync(&connector, 12);
    assert_eq!(items, 40);
    let body = setup.chat("C01").unwrap().body;
    let positions: Vec<usize> = (0..40)
        .map(|i| {
            body.find(&format!("Message {i:02}"))
                .expect("every message")
        })
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "oldest first: {body}"
    );

    // Never more than one call a minute, of at most 15 messages.
    let calls = setup.slack.calls("conversations.history");
    assert!(calls.iter().all(|c| c.params["limit"] == "15"));
    for pair in calls.windows(2) {
        assert!(
            pair[1].at - pair[0].at >= Duration::from_secs(60),
            "{pair:?}"
        );
    }
    // 40 messages: walk back over two pages, then three pages forward, then one call that
    // finds nothing new; lists read page by page.
    assert!(calls.len() >= 5, "{}", calls.len());
    assert!(setup.slack.calls("users.list").len() >= 2);
    assert!(setup.slack.calls("conversations.list").len() >= 3);

    // The cursor holds only ids and times.
    let cursor = setup.cursor.clone().unwrap();
    assert!(!cursor.as_str().contains("Message"), "{cursor:?}");
    assert!(!cursor.as_str().contains("xoxp"), "{cursor:?}");
}

#[test]
fn waits_as_long_as_slack_says() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup
        .slack
        .queue("conversations.history", HttpResponse::rate_limited(120));
    // The run stops without an error and without writing.
    assert_eq!(setup.run(&connector).unwrap(), 0);
    let first = setup.slack.calls("conversations.history").len();
    assert_eq!(first, 1);

    // A minute later it still waits: no call.
    setup.clock.advance(Duration::from_secs(60));
    assert_eq!(setup.run(&connector).unwrap(), 0);
    assert_eq!(setup.slack.calls("conversations.history").len(), first);

    // After the two minutes, it reads.
    setup.clock.advance(Duration::from_secs(61));
    let items = setup.sync(&connector, 2);
    assert_eq!(items, 8);

    // A `ratelimited` error in the body counts the same.
    setup
        .slack
        .queue("conversations.history", error("ratelimited"));
    setup.clock.advance(Duration::from_secs(3600));
    assert_eq!(setup.run(&connector).unwrap(), 0);
}

#[test]
fn errors_stop_a_run_but_keep_what_was_read() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.slack.queue(
        "conversations.history",
        HttpResponse {
            status: 503,
            retry_after: None,
            body: b"down".to_vec(),
        },
    );
    let error = setup.run(&connector).unwrap_err();
    assert!(matches!(error, SourceError::Failed(_)), "{error}");
    assert!(!error.needs_person());
    assert_eq!(setup.sync(&connector, 3), 8);
}

#[test]
fn from_today_leaves_older_messages_out() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.history = History::FromToday;
    // Connected an hour after the first messages: the first thread and the map are older.
    setup.since = Timestamp(((DAY + 3000) * 1000) as i64);
    setup
        .slack
        .post_message("C01", msg("U02", "After connecting", &ts(4000, 1)));
    let items = setup.sync(&connector, 3);
    assert_eq!(items, 1);
    let general = setup.chat("C01").unwrap();
    assert!(general.body.contains("After connecting"));
    assert!(!general.body.contains("Here is the map"));
    assert!(setup.chat(&format!("C01/{}", ts(0, 100))).is_none());
    let calls = setup.slack.calls("conversations.history");
    assert!(
        calls
            .iter()
            .all(|c| c.params["oldest"] >= format!("{}.000000", DAY + 3000))
    );
}

#[test]
fn excluded_channels_are_never_read_and_new_ones_come_in() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.sync(&connector, 3);
    // Excluded later: what came in stays, nothing more is read.
    setup.excluded.push(Container::new("C02", "#plans"));
    setup
        .slack
        .post_message("C02", msg("U01", "Secret plan", &ts(2 * 24 * 3600, 1)));
    // A channel created after connecting comes in.
    setup.slack.with(|w| {
        w.conversations
            .push(json!({"id": "C10", "name": "new", "is_channel": true, "is_member": true}))
    });
    setup
        .slack
        .post_message("C10", msg("U02", "Fresh", &ts(2 * 24 * 3600, 2)));
    setup.clock.advance(Duration::from_secs(700));
    let before = setup.slack.with(|w| w.calls.len());
    setup.sync(&connector, 3);
    let after: Vec<Call> = setup.slack.with(|w| w.calls[before..].to_vec());
    assert!(
        after
            .iter()
            .all(|c| c.params.get("channel").map(String::as_str) != Some("C02"))
    );
    assert!(!setup.chat("C02").unwrap().body.contains("Secret plan"));
    assert!(setup.chat("C10").unwrap().body.contains("Fresh"));
}

#[test]
fn lists_conversations_for_the_connect_sheet() {
    let (setup, connector) = setup(Limits::NotListed);
    let containers = connector.containers(&setup.request()).unwrap();
    let names: Vec<(&str, &str)> = containers
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("C09", "#excluded"),
            ("C01", "#general"),
            ("C02", "#plans"),
            ("G01", "Ada, Grace, Linus"),
            ("D01", "Grace, Ada"),
        ]
    );
}

#[test]
fn a_conversation_gone_is_dropped_from_the_cursor() {
    let (mut setup, connector) = setup(Limits::Marketplace);
    setup.sync(&connector, 3);
    setup
        .slack
        .with(|w| w.conversations.retain(|c| c["id"] != json!("C02")));
    setup.clock.advance(Duration::from_secs(700));
    setup.sync(&connector, 2);
    assert!(!setup.cursor.clone().unwrap().as_str().contains("C02"));
    // What came in from it stays.
    assert!(setup.chat("C02").is_some());
}

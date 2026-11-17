//! The Gmail connector against Google in memory: no network, no real account. The loopback
//! redirect of the sign-in is a real socket on 127.0.0.1, which the test plays the browser on.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use base64::Engine;
use knode_format::{Provenance, Timestamp};
use knode_ingest::sources::sign_in::SignInValues;
use knode_ingest::sources::{Connector, Container, Cursor, History, Secrets, SourceError};
use knode_vault::mail::{MailOptions, MailSource, RawMail, import_mail};
use serde_json::{Value, json};

use super::http::{Http, HttpResponse, query_pairs};
use super::oauth::{GoogleClient, SCOPE, challenge};
use super::*;
use crate::sources::mail_live::MailBatch;
use crate::sources::mail_test_support::{
    MemSecrets, day, message, message_ids, request, space, threads, write,
};

const API: &str = "https://gmail.test/users/me";
const TOKEN: &str = "https://oauth.test/token";

#[derive(Clone)]
struct Msg {
    id: String,
    labels: Vec<String>,
    at: Timestamp,
    raw: String,
}

/// Google, in memory.
#[derive(Default)]
struct Google {
    email: String,
    messages: Vec<Msg>,
    /// `(historyId, message id)` of each message added.
    history: Vec<(u64, String)>,
    history_id: u64,
    /// History older than this is gone: asking for it is a 404.
    oldest_history: u64,
    refresh_tokens: Vec<String>,
    access_tokens: Vec<String>,
    issued: usize,
    challenge: Option<String>,
    revoked: Vec<String>,
    /// The paths of the API asked for.
    asked: Vec<String>,
    refreshed: usize,
}

impl Google {
    fn add(&mut self, id: &str, labels: &[&str], at: Timestamp, raw: String) {
        self.history_id += 1;
        self.messages.push(Msg {
            id: id.into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            at,
            raw,
        });
        self.history.push((self.history_id, id.into()));
    }

    fn access(&mut self) -> Value {
        self.issued += 1;
        let token = format!("at-{}", self.issued);
        self.access_tokens.push(token.clone());
        json!({"access_token": token, "expires_in": 3600, "token_type": "Bearer", "scope": SCOPE})
    }
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Google>>);

fn page<T: Clone>(
    items: &[T],
    params: &[(String, String)],
    max: usize,
) -> (Vec<T>, Option<String>) {
    let get = |k: &str| {
        params
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    let start: usize = get("pageToken").and_then(|p| p.parse().ok()).unwrap_or(0);
    let max: usize = get("maxResults")
        .and_then(|m| m.parse().ok())
        .unwrap_or(max);
    let end = (start + max).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    (items[start.min(end)..end].to_vec(), next)
}

impl Http for Fake {
    fn get(&self, url: &str, bearer: &str) -> io::Result<HttpResponse> {
        let mut g = self.0.lock().unwrap();
        let path = url.strip_prefix(API).expect("only the Gmail API is asked");
        g.asked.push(path.to_string());
        if !g.access_tokens.iter().any(|t| t == bearer) {
            return Ok(HttpResponse::json(
                401,
                &json!({"error": {"message": "Invalid Credentials"}}),
            ));
        }
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let params = query_pairs(query);
        let get = |k: &str| {
            params
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        let ok = |v: Value| Ok(HttpResponse::json(200, &v));
        match route {
            "/profile" => {
                ok(json!({"emailAddress": g.email, "historyId": g.history_id.to_string()}))
            }
            "/labels" => ok(json!({"labels": [
                {"id": "INBOX", "name": "INBOX", "type": "system"},
                {"id": "SENT", "name": "SENT", "type": "system"},
                {"id": "SPAM", "name": "SPAM", "type": "system"},
                {"id": "TRASH", "name": "TRASH", "type": "system"},
                {"id": "UNREAD", "name": "UNREAD", "type": "system"},
                {"id": "CATEGORY_UPDATES", "name": "CATEGORY_UPDATES", "type": "system"},
                {"id": "Label_1", "name": "Work/Clients", "type": "user"},
                {"id": "Label_2", "name": "Private", "type": "user"},
            ]})),
            "/messages" => {
                let after: Option<i64> =
                    get("q").and_then(|q| q.strip_prefix("after:").and_then(|s| s.parse().ok()));
                let mut listed: Vec<Msg> = g
                    .messages
                    .iter()
                    .filter(|m| !m.labels.iter().any(|l| l == "SPAM" || l == "TRASH"))
                    .filter(|m| after.is_none_or(|s| m.at.0 >= s * 1000))
                    .cloned()
                    .collect();
                listed.reverse(); // newest first, like Gmail
                let (items, next) = page(&listed, &params, 100);
                let ids: Vec<Value> = items.iter().map(|m| json!({"id": m.id})).collect();
                let mut out = json!({"messages": ids});
                if let Some(next) = next {
                    out["nextPageToken"] = json!(next);
                }
                ok(out)
            }
            "/history" => {
                let start: u64 = get("startHistoryId").unwrap().parse().unwrap();
                if start < g.oldest_history {
                    return Ok(HttpResponse::json(
                        404,
                        &json!({"error": {"message": "Not Found"}}),
                    ));
                }
                let records: Vec<(u64, String)> = g
                    .history
                    .iter()
                    .filter(|(h, _)| *h > start)
                    .cloned()
                    .collect();
                let (items, next) = page(&records, &params, 100);
                let history: Vec<Value> = items
                    .iter()
                    .map(|(h, id)| json!({"id": h.to_string(), "messagesAdded": [{"message": {"id": id}}]}))
                    .collect();
                let mut out = json!({"history": history, "historyId": g.history_id.to_string()});
                if let Some(next) = next {
                    out["nextPageToken"] = json!(next);
                }
                ok(out)
            }
            other => {
                let id = other.strip_prefix("/messages/").unwrap();
                assert_eq!(get("format").as_deref(), Some("raw"));
                match g.messages.iter().find(|m| m.id == id) {
                    Some(m) => ok(json!({
                        "id": m.id,
                        "labelIds": m.labels,
                        "internalDate": m.at.0.to_string(),
                        "raw": base64::engine::general_purpose::URL_SAFE.encode(&m.raw),
                    })),
                    None => Ok(HttpResponse::json(
                        404,
                        &json!({"error": {"message": "Not Found"}}),
                    )),
                }
            }
        }
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> io::Result<HttpResponse> {
        let mut g = self.0.lock().unwrap();
        let get = |k: &str| {
            form.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        };
        if url == "https://oauth.test/revoke" {
            g.revoked.push(get("token").unwrap());
            return Ok(HttpResponse::json(200, &json!({})));
        }
        assert_eq!(url, TOKEN);
        assert_eq!(get("client_id").as_deref(), Some("test-client"));
        match get("grant_type").as_deref() {
            Some("refresh_token") => {
                let token = get("refresh_token").unwrap();
                if !g.refresh_tokens.contains(&token) {
                    return Ok(HttpResponse::json(
                        400,
                        &json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
                    ));
                }
                g.refreshed += 1;
                let access = g.access();
                Ok(HttpResponse::json(200, &access))
            }
            Some("authorization_code") => {
                let verifier = get("code_verifier").unwrap();
                if get("code").as_deref() != Some("good-code")
                    || g.challenge.as_deref() != Some(challenge(&verifier).as_str())
                    || !get("redirect_uri")
                        .unwrap()
                        .starts_with("http://127.0.0.1:")
                {
                    return Ok(HttpResponse::json(400, &json!({"error": "invalid_grant"})));
                }
                g.refresh_tokens.push("rt-1".into());
                let mut access = g.access();
                access["refresh_token"] = json!("rt-1");
                Ok(HttpResponse::json(200, &access))
            }
            other => panic!("unexpected grant {other:?}"),
        }
    }
}

fn endpoints() -> Endpoints {
    Endpoints {
        auth: "https://accounts.test/auth".into(),
        token: TOKEN.into(),
        revoke: "https://oauth.test/revoke".into(),
        api: API.into(),
    }
}

fn client() -> Option<GoogleClient> {
    Some(GoogleClient {
        id: "test-client".into(),
        secret: None,
    })
}

fn connector(fake: &Fake) -> GmailConnector {
    GmailConnector::with(Arc::new(fake.clone()), client(), endpoints())
}

/// A connector signed in already, as if a sign-in had run.
fn signed_in(fake: &Fake) -> (GmailConnector, MemSecrets) {
    {
        let mut g = fake.0.lock().unwrap();
        g.email = "ana@example.com".into();
        g.refresh_tokens.push("rt-0".into());
    }
    let secrets = MemSecrets::default();
    secrets.set(REFRESH_TOKEN_SECRET, "rt-0").unwrap();
    secrets.set(ACCOUNT_SECRET, "ana@example.com").unwrap();
    (connector(fake), secrets)
}

fn run(
    connector: &GmailConnector,
    space: &mut knode_format::Space,
    cursor: Option<Cursor>,
    history: History,
    excluded: &[Container],
    secrets: &dyn Secrets,
) -> (Cursor, usize) {
    let mut cursor = cursor;
    let mut added = 0;
    for _ in 0..100 {
        let fetched = connector
            .fetch(&request(cursor.as_ref(), history, excluded, secrets))
            .unwrap();
        let more = fetched.more;
        cursor = Some(fetched.cursor.clone());
        added += write(space, fetched, ID);
        if !more {
            break;
        }
    }
    (cursor.unwrap(), added)
}

/// Plays the browser coming back to the loopback address with this query.
fn browser(redirect: &str, path_and_query: &str) -> String {
    let port = redirect.rsplit(':').next().unwrap();
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    write!(
        stream,
        "GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut page = String::new();
    stream.read_to_string(&mut page).unwrap();
    page
}

#[test]
fn without_a_client_id_it_says_sign_in_with_google_is_not_set_up() {
    let fake = Fake::default();
    let connector = GmailConnector::with(Arc::new(fake), None, endpoints());
    let error = connector.check().unwrap_err();
    assert_eq!(error.kind(), "unavailable");
    assert!(error.to_string().contains("KNODE_GOOGLE_CLIENT_ID"));
    let secrets = MemSecrets::default();
    assert_eq!(connector.start(&secrets).unwrap_err().kind(), "unavailable");
    assert_eq!(
        connector
            .fetch(&request(None, History::Everything, &[], &secrets))
            .unwrap_err()
            .kind(),
        "unavailable"
    );
}

#[test]
fn signs_in_with_google_with_pkce_and_a_loopback_redirect() {
    let fake = Fake::default();
    fake.0.lock().unwrap().email = "ana@example.com".into();
    let connector = connector(&fake);
    let secrets = MemSecrets::default();
    let step = connector.start(&secrets).unwrap().unwrap();
    assert_eq!(step.id, "browser");
    let url = step.browser_url.unwrap();
    let (base, query) = url.split_once('?').unwrap();
    assert_eq!(base, "https://accounts.test/auth");
    let params = query_pairs(query);
    let get = |k: &str| {
        params
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(get("scope"), SCOPE);
    assert_eq!(get("code_challenge_method"), "S256");
    assert_eq!(get("client_id"), "test-client");
    assert_eq!(get("access_type"), "offline");
    let redirect = get("redirect_uri");
    assert!(redirect.starts_with("http://127.0.0.1:"));
    fake.0.lock().unwrap().challenge = Some(get("code_challenge"));
    let state = get("state");

    // Requests that are not the redirect are ignored.
    assert!(browser(&redirect, "/favicon.ico").starts_with("HTTP/1.1 404"));
    assert!(browser(&redirect, "/?state=forged&code=bad").starts_with("HTTP/1.1 400"));
    let page = browser(
        &redirect,
        &format!("/?state={state}&code=good-code&scope={SCOPE}"),
    );
    assert!(page.contains("Signed in"));

    assert_eq!(
        connector
            .answer("browser", &SignInValues::default(), &secrets)
            .unwrap(),
        None
    );
    assert_eq!(
        secrets.get(REFRESH_TOKEN_SECRET).unwrap().as_deref(),
        Some("rt-1")
    );
    assert_eq!(
        SignIn::account(&connector, &secrets).unwrap().as_deref(),
        Some("ana@example.com")
    );
    assert_eq!(connector.start(&secrets).unwrap(), None);

    // Signing out forgets the secrets and revokes the token.
    assert!(connector.sign_out(&secrets).unwrap());
    assert!(secrets.names().is_empty());
    assert_eq!(fake.0.lock().unwrap().revoked, vec!["rt-1"]);
}

#[test]
fn a_refused_sign_in_says_so_and_can_start_again() {
    let fake = Fake::default();
    let connector = connector(&fake);
    let secrets = MemSecrets::default();
    let step = connector.start(&secrets).unwrap().unwrap();
    let url = step.browser_url.unwrap();
    let params = query_pairs(url.split_once('?').unwrap().1);
    let get = |k: &str| {
        params
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    let page = browser(
        &get("redirect_uri"),
        &format!("/?state={}&error=access_denied", get("state")),
    );
    assert!(page.contains("Not signed in"));
    let error = connector
        .answer("browser", &SignInValues::default(), &secrets)
        .unwrap_err();
    assert_eq!(error.kind(), "sign-in");
    assert!(secrets.names().is_empty());
    // Going on again opens a new sign-in page.
    let again = connector
        .answer("browser", &SignInValues::default(), &secrets)
        .unwrap()
        .unwrap();
    assert!(again.browser_url.is_some());
}

#[test]
fn without_signing_in_it_asks_to_sign_in() {
    let connector = connector(&Fake::default());
    let secrets = MemSecrets::default();
    let error = connector
        .fetch(&request(None, History::Everything, &[], &secrets))
        .unwrap_err();
    assert_eq!(error.kind(), "sign-in");
}

fn mailbox(fake: &Fake) {
    let mut g = fake.0.lock().unwrap();
    g.add(
        "a1",
        &["INBOX", "UNREAD"],
        day(2),
        message("old@example.com", "Old news", 2, "old"),
    );
    g.add(
        "a2",
        &["INBOX", "Label_1", "CATEGORY_UPDATES"],
        day(11),
        message("new@example.com", "Client plans", 11, "new"),
    );
    g.add(
        "a3",
        &["SPAM"],
        day(11),
        message("spam@example.com", "Win", 11, "spam"),
    );
    g.add(
        "a4",
        &["TRASH"],
        day(11),
        message("bin@example.com", "Gone", 11, "bin"),
    );
    g.add(
        "a5",
        &["Label_2"],
        day(11),
        message("private@example.com", "Private", 11, "p"),
    );
    g.add(
        "a6",
        &["SENT"],
        day(12),
        message("sent@example.com", "Sent one", 12, "s"),
    );
}

#[test]
fn lists_labels_without_spam_trash_or_states() {
    let fake = Fake::default();
    let (connector, secrets) = signed_in(&fake);
    let containers = connector
        .containers(&request(None, History::Everything, &[], &secrets))
        .unwrap();
    let ids: Vec<&str> = containers.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["INBOX", "CATEGORY_UPDATES", "Label_2", "SENT", "Label_1"]
    );
    assert_eq!(containers[1].name, "Category Updates");
    assert_eq!(containers[4].name, "Work/Clients");
}

#[test]
fn everything_comes_in_once_except_spam_trash_and_excluded_labels() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let fake = Fake::default();
    mailbox(&fake);
    let (connector, secrets) = signed_in(&fake);
    let excluded = [Container::new("Label_2", "Private")];
    let (_, added) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &excluded,
        &secrets,
    );
    assert_eq!(added, 3);
    assert_eq!(
        message_ids(&space),
        vec!["new@example.com", "old@example.com", "sent@example.com"]
    );
    let client = threads(&space)
        .into_iter()
        .find(|t| t.title == "Client plans")
        .unwrap();
    let props = space.properties(&client.id).unwrap();
    let folders = &props["mail.folders"];
    assert!(folders.contains("Inbox") && folders.contains("Work/Clients"));
    assert!(folders.contains("Category Updates") && !folders.contains("UNREAD"));
    assert_eq!(props["mail.source"], "gmail");
}

#[test]
fn from_today_lists_only_what_came_since_and_then_follows_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let fake = Fake::default();
    mailbox(&fake);
    let (connector, secrets) = signed_in(&fake);
    let (cursor, added) = run(
        &connector,
        &mut space,
        None,
        History::FromToday,
        &[],
        &secrets,
    );
    assert_eq!(added, 3); // a2, a5, a6: not the old one, not spam or trash
    assert!(!message_ids(&space).contains(&"old@example.com".to_string()));
    let listing = fake
        .0
        .lock()
        .unwrap()
        .asked
        .iter()
        .find(|p| p.starts_with("/messages?"))
        .cloned()
        .unwrap();
    assert!(listing.contains("q=after%3A"));

    // New mail: read through the history, and only it.
    fake.0.lock().unwrap().add(
        "b1",
        &["INBOX"],
        day(13),
        message("b1@example.com", "Fresh", 13, "fresh"),
    );
    fake.0.lock().unwrap().asked.clear();
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::FromToday,
        &[],
        &secrets,
    );
    assert_eq!(added, 1);
    let asked = fake.0.lock().unwrap().asked.clone();
    assert!(
        asked
            .iter()
            .any(|p| p.starts_with("/history?startHistoryId=6&"))
    );
    assert_eq!(
        asked.iter().filter(|p| p.starts_with("/messages/")).count(),
        1
    );

    // Nothing new: no message is read.
    fake.0.lock().unwrap().asked.clear();
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::FromToday,
        &[],
        &secrets,
    );
    assert_eq!(added, 0);
    assert!(
        !fake
            .0
            .lock()
            .unwrap()
            .asked
            .iter()
            .any(|p| p.starts_with("/messages"))
    );

    // A restart from an older cursor (the run stopped before it was saved) adds nothing twice.
    let saved: Value = serde_json::from_str(cursor.as_str()).unwrap();
    let mut older = saved.clone();
    older["history"] = json!("1");
    let (_, added) = run(
        &connector,
        &mut space,
        Some(Cursor::new(older.to_string())),
        History::FromToday,
        &[],
        &secrets,
    );
    assert_eq!(added, 0);
    assert_eq!(message_ids(&space).len(), 4);
}

#[test]
fn long_listings_and_histories_come_in_pages() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let fake = Fake::default();
    for i in 0..25 {
        fake.0.lock().unwrap().add(
            &format!("p{i}"),
            &["INBOX"],
            day(11),
            message(
                &format!("p{i:02}@example.com"),
                &format!("Page {i}"),
                11,
                "x",
            ),
        );
    }
    let (connector, secrets) = signed_in(&fake);
    let mut req = request(None, History::Everything, &[], &secrets);
    req.max_items = 10;
    let first = connector.fetch(&req).unwrap();
    assert!(first.more);
    assert_eq!(first.batch.as_ref().unwrap().len(), 10);
    let mut cursor = first.cursor.clone();
    write(&mut space, first, ID);
    loop {
        let mut req = request(Some(&cursor), History::Everything, &[], &secrets);
        req.max_items = 10;
        let fetched = connector.fetch(&req).unwrap();
        let more = fetched.more;
        cursor = fetched.cursor.clone();
        write(&mut space, fetched, ID);
        if !more {
            break;
        }
    }
    assert_eq!(message_ids(&space).len(), 25);
}

#[test]
fn a_history_google_no_longer_has_is_resynced_from_the_last_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let fake = Fake::default();
    mailbox(&fake);
    let (connector, secrets) = signed_in(&fake);
    let (cursor, _) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );
    let before = message_ids(&space).len();

    // A week passes: Google drops the old history, and new mail arrives.
    {
        let mut g = fake.0.lock().unwrap();
        g.add(
            "c1",
            &["INBOX"],
            Timestamp::now(),
            message("c1@example.com", "Later", 14, "later"),
        );
        g.oldest_history = g.history_id;
    }
    fake.0.lock().unwrap().asked.clear();
    let (cursor, added) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 1);
    assert_eq!(message_ids(&space).len(), before + 1);
    let asked = fake.0.lock().unwrap().asked.clone();
    let listing = asked.iter().find(|p| p.starts_with("/messages?")).unwrap();
    // Bounded: only from a day before the last run, at most RESYNC_MAX.
    assert!(listing.contains("q=after%3A"), "{listing}");
    let saved: Value = serde_json::from_str(cursor.as_str()).unwrap();
    assert_eq!(
        saved["history"],
        json!(fake.0.lock().unwrap().history_id.to_string())
    );
    assert!(saved.get("listing").is_none());
}

#[test]
fn an_expired_access_token_is_refreshed_and_a_revoked_one_asks_to_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let fake = Fake::default();
    mailbox(&fake);
    let (connector, secrets) = signed_in(&fake);
    let (cursor, _) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(fake.0.lock().unwrap().refreshed, 1);

    // Google expires the access token: it is refreshed once and the run goes on.
    fake.0.lock().unwrap().access_tokens.clear();
    let (cursor, _) = run(
        &connector,
        &mut space,
        Some(cursor),
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(fake.0.lock().unwrap().refreshed, 2);

    // The person removes Knode's access: sign in again.
    {
        let mut g = fake.0.lock().unwrap();
        g.access_tokens.clear();
        g.refresh_tokens.clear();
    }
    let error = connector
        .fetch(&request(Some(&cursor), History::Everything, &[], &secrets))
        .unwrap_err();
    assert!(matches!(error, SourceError::SignIn(_)), "{error}");
}

#[test]
fn another_account_starts_again() {
    let fake = Fake::default();
    mailbox(&fake);
    let (connector, secrets) = signed_in(&fake);
    let cursor = Cursor::new(r#"{"account":"bo@example.org","history":"99"}"#);
    let fetched = connector
        .fetch(&request(Some(&cursor), History::Everything, &[], &secrets))
        .unwrap();
    assert!(fetched.batch.is_some());
    let saved: Value = serde_json::from_str(fetched.cursor.as_str()).unwrap();
    assert_eq!(saved["account"], "ana@example.com");
}

/// An `.emlx` file of Apple Mail: the length, the message, a property list.
fn emlx(message: &str) -> String {
    format!(
        "{}\n{message}<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict></dict></plist>\n",
        message.len()
    )
}

#[test]
fn the_same_mail_from_takeout_apple_mail_imap_and_the_api_is_written_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut space = space(dir.path());
    let by = Provenance::person("Ana");
    let with_id = message("same@example.com", "Shared", 11, "the same message");
    // A message without a Message-ID is found by its sender, date, subject and text.
    let without_id = message("", "No id", 11, "a message without an id");

    // 1. A Google Takeout export.
    let mbox = dir.path().join("All mail Including Spam and Trash.mbox");
    let mut text = String::new();
    for m in [&with_id, &without_id] {
        text.push_str("From 1278455344230334865@xxx Wed Nov 11 10:00:00 +0000 2026\n");
        text.push_str("X-GM-THRID: 1278455344230334865\nX-Gmail-Labels: Inbox,Opened\n");
        text.push_str(&m.replace("\r\n", "\n"));
        text.push('\n');
    }
    std::fs::write(&mbox, text).unwrap();
    let report = import_mail(
        &mut space,
        MailSource::Mbox,
        &mbox,
        &MailOptions::default(),
        &by,
    )
    .unwrap();
    assert_eq!(report.added, 2);

    // 2. Apple Mail, with the same account in it.
    let messages = dir.path().join("V10/ACCOUNT/INBOX.mbox/Messages");
    std::fs::create_dir_all(&messages).unwrap();
    std::fs::write(messages.join("1.emlx"), emlx(&with_id)).unwrap();
    std::fs::write(messages.join("2.emlx"), emlx(&without_id)).unwrap();
    let report = import_mail(
        &mut space,
        MailSource::AppleMail,
        dir.path().join("V10"),
        &MailOptions::default(),
        &by,
    )
    .unwrap();
    assert_eq!(report.added, 0);

    // 3. The IMAP connector.
    let imap = Box::new(MailBatch {
        source: MailSource::Imap,
        mails: vec![
            RawMail::new(with_id.clone(), ["Inbox".to_string()]),
            RawMail::new(without_id.clone(), ["Inbox".to_string()]),
        ],
    });
    assert_eq!(
        knode_ingest::sources::Batch::write(imap, &mut space, &provenance_of("imap"))
            .unwrap()
            .items,
        0
    );

    // 4. The Gmail API connector.
    let dir2 = tempfile::tempdir().unwrap();
    let _ = dir2;
    let fake = Fake::default();
    fake.0
        .lock()
        .unwrap()
        .add("x1", &["INBOX"], day(11), with_id.clone());
    fake.0
        .lock()
        .unwrap()
        .add("x2", &["INBOX"], day(11), without_id.clone());
    let (connector, secrets) = signed_in(&fake);
    let (_, added) = run(
        &connector,
        &mut space,
        None,
        History::Everything,
        &[],
        &secrets,
    );
    assert_eq!(added, 0);

    let ids = message_ids(&space);
    assert_eq!(ids.len(), 2, "{ids:?}");
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 2);
    assert!(ids.contains(&"same@example.com".to_string()));
}

fn provenance_of(id: &str) -> Provenance {
    crate::sources::provenance(id)
}

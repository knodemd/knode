//! Apple Mail, read live from the Mail store of this Mac: [`AppleMailConnector`].
//!
//! Apple Mail keeps the mail of every account added to it (Gmail, iCloud, Outlook, Exchange,
//! any IMAP or POP account, and the mailboxes "On My Mac") in `~/Library/Mail/V<n>`: one
//! `.emlx` file per message, under a folder per account and mailbox, and an index of every
//! message in `MailData/Envelope Index`, a SQLite database. Reading them needs Full Disk Access.
//!
//! - The index is never opened where it is: it is copied, with its write-ahead log, into a
//!   private temporary folder and only the copy is read
//!   ([`sqlite::open_copy`](knode_ingest::sources::sqlite::open_copy)). The copy is checked
//!   first for the tables and columns Knode reads ([`SCHEMA`]); when one is missing, nothing
//!   is written and the error says the connector needs an update. The `.emlx` files are only
//!   read.
//! - The cursor is the number (`ROWID`) of the last message of the index read, with the store
//!   it belongs to. Messages the index lists but whose file Mail has not written yet (a mail
//!   still downloading when the copy was taken, or the first sync of an account) are kept in
//!   the cursor as ranges of numbers, as many as there are, and read on a later run once their
//!   file is there. None is dropped: those still waiting after a day are counted in the
//!   connector's [notice](Connector::notice). When Mail rebuilds its index or moves to a new
//!   `V<n>`, the numbers start again and so does the cursor: what was read before is found
//!   again by its `Message-ID` and added nothing twice.
//! - A message's file is found where Mail puts it,
//!   `<mailbox>.mbox/<id>/Data/<digits>/Messages/<ROWID>.emlx`, through a listing of that
//!   `Messages` folder, kept between runs and read again only when the folder changed: a
//!   mailbox is never walked.
//! - The containers are the mailboxes, by their URL in the index (`imap://<account>/INBOX`):
//!   every mailbox of every account comes in, new ones too, except the ones excluded and spam
//!   and trash (`Junk`, `Spam`, `Trash`, `Bin`, `Deleted Messages`, `Deleted Items`), which are
//!   never read. Messages Mail marked deleted are left out.
//! - With [`History::FromToday`], nothing Mail received before the day the connector was
//!   connected is read.
//! - The messages are written by the mail importer
//!   ([`knode_vault::mail::import_apple_mail_files`]): one node per thread, the people of each
//!   thread, edits made in Knode kept. A message already in the space from an import of an
//!   Apple Mail store or export or of a Gmail Takeout `.mbox` is found by its `Message-ID` and
//!   not added again. Attachments are not attached: the files are named in the thread.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use knode_ingest::sources::FileStamp;

use knode_format::{Provenance, Space, Timestamp};
use knode_ingest::sources::sqlite::{DatabaseCopy, check_schema, open_copy};
use knode_ingest::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, Readiness,
    SourceError, SourceInfo, Trigger, Written,
};
use knode_vault::mail::{MailError, MailOptions, import_apple_mail_files, is_junk_folder};
use serde::{Deserialize, Serialize};

/// The id of the connector, the same as the Apple Mail importer's.
pub const ID: &str = "apple-mail";

/// The tables and columns of the Envelope Index the connector reads.
pub const SCHEMA: &[(&str, &[&str])] = &[
    (
        "messages",
        &["ROWID", "mailbox", "date_received", "deleted"],
    ),
    ("mailboxes", &["ROWID", "url"]),
];

/// The longest time between two looks at the index when no change of it was seen. macOS
/// reports the changes of a file that is kept open late, and Mail keeps its index open, so the
/// service also looks at it every minute, and runs the connector when it changed or a message
/// file is waited for: new mail is in within a minute. An index that did not change is not
/// copied.
const RUN_AT_LEAST_EVERY: Duration = Duration::from_secs(60);

/// After how long a message the index lists whose file is still not there is reported in the
/// connector's notice. It is still waited for.
const LATE_AFTER_MS: i64 = 24 * 3600 * 1000;

/// The most `Messages` folder listings kept between runs.
const MAX_LISTINGS: usize = 20_000;

/// What a person reads when macOS keeps Knode from reading the Mail store.
const NO_ACCESS: &str = "macOS keeps Knode from reading Apple Mail's mail in ~/Library/Mail. \
     Open System Settings > Privacy & Security > Full Disk Access, turn it on for Knode (or the \
     app that runs Knode Service), then quit and reopen that app. Mail comes in from then on.";

/// The Apple Mail connector: brings in the new mail of every account of Apple Mail on this
/// Mac, from its store. See the [module](self).
#[derive(Debug, Clone)]
pub struct AppleMailConnector {
    /// `~/Library/Mail`, with the `V<n>` folders in it.
    mail_dir: PathBuf,
    /// The index as it was when it was last read.
    seen: Arc<Mutex<Seen>>,
}

/// What the connector remembers between runs: by request, the index as it was when a fetch
/// found nothing new and waited for no file, so that a run while it has not changed copies
/// nothing; and the listings of the `Messages` folders it looked in.
#[derive(Debug, Default)]
struct Seen {
    idle: HashMap<String, FileStamp>,
    listings: HashMap<PathBuf, Listing>,
    /// How many listings were read from disk, for tests.
    listings_read: u64,
}

/// The message files of a `Messages` folder by number, and when the folder last changed.
#[derive(Debug)]
struct Listing {
    modified: Option<SystemTime>,
    files: HashMap<i64, PathBuf>,
}

impl Seen {
    /// The message files of `dir`, read again only when it changed since the last time.
    fn listing(&mut self, dir: &Path) -> &HashMap<i64, PathBuf> {
        let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let fresh = modified.is_some()
            && self
                .listings
                .get(dir)
                .is_some_and(|l| l.modified == modified);
        if !fresh {
            if self.listings.len() >= MAX_LISTINGS {
                self.listings.clear();
            }
            let files = if modified.is_some() {
                self.listings_read += 1;
                message_files(dir)
            } else {
                HashMap::new()
            };
            self.listings
                .insert(dir.to_path_buf(), Listing { modified, files });
        }
        &self.listings[dir].files
    }
}

/// The message files directly in a `Messages` folder, by number: `<n>.emlx`, or
/// `<n>.partial.emlx` when there is no whole one.
fn message_files(dir: &Path) -> HashMap<i64, PathBuf> {
    let mut found = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(n) = name.strip_suffix(".partial.emlx") {
            if let Ok(n) = n.parse::<i64>() {
                found.entry(n).or_insert_with(|| entry.path());
            }
        } else if let Some(n) = name.strip_suffix(".emlx")
            && let Ok(n) = n.parse::<i64>()
        {
            found.insert(n, entry.path());
        }
    }
    found
}

impl AppleMailConnector {
    /// The connector for the Mail store of the person running Knode; `None` off macOS.
    pub fn local() -> Option<AppleMailConnector> {
        knode_vault::mail::apple_mail_location().map(AppleMailConnector::new)
    }

    /// The connector for a Mail folder laid out like `~/Library/Mail`.
    pub fn new(mail_dir: impl Into<PathBuf>) -> AppleMailConnector {
        AppleMailConnector {
            mail_dir: mail_dir.into(),
            seen: Arc::default(),
        }
    }

    /// The store read: the newest `V<n>` folder of the Mail folder.
    pub fn store(&self) -> Result<PathBuf, SourceError> {
        let entries = std::fs::read_dir(&self.mail_dir).map_err(|e| self.read_error(e))?;
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let n: u32 = name.strip_prefix('V')?.parse().ok()?;
                e.path().is_dir().then(|| (n, e.path()))
            })
            .max_by_key(|(n, _)| *n)
            .map(|(_, path)| path)
            .ok_or_else(|| {
                SourceError::Unavailable(
                    "Apple Mail has no mail on this Mac: add an account to Mail first".into(),
                )
            })
    }

    /// The Envelope Index of a store.
    pub fn index_path(store: &Path) -> PathBuf {
        store.join("MailData").join("Envelope Index")
    }

    fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The index and its `-wal` as they are now, or `None` when it cannot be told.
    fn stamp(&self) -> Option<FileStamp> {
        let store = self.store().ok()?;
        FileStamp::of(&[Self::index_path(&store)])
    }

    /// How many listings of `Messages` folders the connector read from disk so far.
    #[doc(hidden)]
    pub fn listings_read(&self) -> u64 {
        self.seen().listings_read
    }

    fn read_error(&self, error: io::Error) -> SourceError {
        match error.kind() {
            io::ErrorKind::NotFound => SourceError::Unavailable(
                "Apple Mail has no mail on this Mac: add an account to Mail first".into(),
            ),
            io::ErrorKind::PermissionDenied => no_access(),
            _ => SourceError::Failed(format!(
                "{} could not be read: {error}",
                self.mail_dir.display()
            )),
        }
    }

    fn snapshot(&self) -> Result<Snapshot, SourceError> {
        let store = self.store()?;
        let index = Self::index_path(&store);
        let copy = open_copy(&index).map_err(|e| match e {
            SourceError::Permission(..) => no_access(),
            SourceError::Unavailable(_) => SourceError::Unavailable(format!(
                "Apple Mail has not indexed its mail yet ({} is missing): open Mail once",
                index.display()
            )),
            other => other,
        })?;
        check_schema(copy.conn(), SCHEMA)?;
        Ok(Snapshot { copy, store })
    }
}

fn wal_path(index: &Path) -> PathBuf {
    let mut wal = index.as_os_str().to_os_string();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// What a fetch depends on besides the index.
fn request_key(request: &FetchRequest<'_>, cursor: &str) -> String {
    let excluded: Vec<&str> = request.excluded.iter().map(|c| c.id.as_str()).collect();
    format!(
        "{}\n{cursor}\n{}\n{}\n{}",
        request.space_id,
        request.history.id(),
        request.since.0,
        excluded.join("\n")
    )
}

fn no_access() -> SourceError {
    SourceError::Permission(Permission::FullDiskAccess, NO_ACCESS.into())
}

fn failed(error: rusqlite::Error) -> SourceError {
    SourceError::Failed(format!("Apple Mail's index could not be read: {error}"))
}

impl Connector for AppleMailConnector {
    /// Whether the Mail folder can be listed (macOS refuses it without Full Disk Access) and
    /// holds a store, which it has once Mail has an account.
    fn readiness(&self) -> Readiness {
        match self.store() {
            Ok(_) => Readiness::Ready,
            Err(e) => Readiness::Blocked(e),
        }
    }

    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            ID,
            "Apple Mail",
            "The mail of every account in Apple Mail on this Mac: Gmail, iCloud, Outlook, \
             Exchange, IMAP",
        )
        .needing([Permission::FullDiskAccess])
        .excluding("mailbox")
    }

    fn trigger(&self) -> Trigger {
        let paths = match self.store() {
            Ok(store) => {
                let index = Self::index_path(&store);
                let wal = wal_path(&index);
                vec![index, wal]
            }
            // Watched until a store appears or access is given.
            Err(_) => vec![self.mail_dir.clone()],
        };
        Trigger::Watch {
            paths,
            every: RUN_AT_LEAST_EVERY,
        }
    }

    fn check(&self) -> Result<(), SourceError> {
        self.snapshot().map(|_| ())
    }

    fn waiting(&self, cursor: &Cursor) -> bool {
        // A message file Mail writes after its index is not seen as a change of the index.
        !State::read(Some(cursor)).waiting.is_empty()
    }

    fn notice(&self, cursor: &Cursor) -> Option<String> {
        let late_since = Timestamp::now().0 - LATE_AFTER_MS;
        let late: i64 = State::read(Some(cursor))
            .waiting
            .iter()
            .filter(|w| w.2 < late_since)
            .map(|w| w.1 - w.0 + 1)
            .sum();
        match late {
            0 => None,
            1 => Some(
                "1 message Mail lists has had no file on this Mac for more than a day, so it is \
                 not in the space yet: Mail keeps it only on the server. It comes in once Mail \
                 downloads it, for example when you open it in Mail."
                    .into(),
            ),
            n => Some(format!(
                "{n} messages Mail lists have had no file on this Mac for more than a day, so \
                 they are not in the space yet: Mail keeps them only on the server. They come in \
                 once Mail downloads them, for example when you open them in Mail."
            )),
        }
    }

    fn containers(&self, _request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let snapshot = self.snapshot()?;
        let mut containers: Vec<Container> = snapshot
            .mailboxes()?
            .into_values()
            .filter(|m| !m.is_junk())
            .map(|m| Container::new(m.url, m.name))
            .collect();
        containers.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        Ok(containers)
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let stamp = self.stamp();
        if let (Some(now), Some(cursor)) = (&stamp, request.cursor) {
            let key = request_key(request, cursor.as_str());
            if self.seen().idle.get(&key) == Some(now) {
                return Ok(Fetched::nothing(cursor.clone()));
            }
        }
        let snapshot = self.snapshot()?;
        let conn = snapshot.copy.conn();
        let store_name = snapshot
            .store
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let newest: i64 = conn
            .query_row("SELECT IFNULL(MAX(ROWID), 0) FROM messages", [], |r| {
                r.get(0)
            })
            .map_err(failed)?;
        let mut state = State::read(request.cursor);
        // A new store, or an index Mail rebuilt: its numbers start again.
        if state.store != store_name || state.rowid > newest {
            state = State {
                store: store_name,
                ..State::default()
            };
        }
        let mailboxes = snapshot.mailboxes()?;
        let wanted = |mailbox: i64| {
            mailboxes
                .get(&mailbox)
                .filter(|m| !m.is_junk() && !request.is_excluded(&m.url))
                .map(|m| (mailbox, m))
        };
        let from = match request.history {
            History::Everything => i64::MIN,
            History::FromToday => request.since.0.div_euclid(1000),
        };
        let now = Timestamp::now();
        let max = request.max_items.max(1);
        let mut seen = self.seen();
        let mut finder = Finder::new(&snapshot.store);
        let mut files: Vec<PathBuf> = Vec::new();
        let mut waiting: Vec<Waiting> = Vec::new();
        let mut more = false;

        // The messages waited for, first. One that left the index, was deleted or is in a
        // mailbox now excluded is not waited for any more.
        {
            let mut stmt = conn
                .prepare(
                    "SELECT ROWID, mailbox, deleted FROM messages \
                     WHERE ROWID BETWEEN ?1 AND ?2 ORDER BY ROWID",
                )
                .map_err(failed)?;
            for range in std::mem::take(&mut state.waiting) {
                if files.len() >= max {
                    add_waiting(&mut waiting, range);
                    more = true;
                    continue;
                }
                let mut rows = stmt.query([range.0, range.1]).map_err(failed)?;
                while let Some(row) = rows.next().map_err(failed)? {
                    let rowid: i64 = row.get(0).map_err(failed)?;
                    if files.len() >= max {
                        add_waiting(&mut waiting, Waiting(rowid, range.1, range.2));
                        more = true;
                        break;
                    }
                    let mailbox: i64 = row.get(1).map_err(failed)?;
                    let deleted: Option<i64> = row.get(2).map_err(failed)?;
                    if deleted.unwrap_or(0) != 0 {
                        continue;
                    }
                    let Some(mailbox) = wanted(mailbox) else {
                        continue;
                    };
                    match finder.find(&mut seen, mailbox, rowid) {
                        Some(file) => files.push(file),
                        None => add_waiting(&mut waiting, Waiting(rowid, rowid, range.2)),
                    }
                }
            }
        }

        // Then what is new after the cursor.
        let mut last = state.rowid;
        if !more {
            let mut stmt = conn
                .prepare(
                    "SELECT ROWID, mailbox FROM messages \
                     WHERE ROWID > ?1 AND IFNULL(deleted, 0) = 0 \
                     AND IFNULL(date_received, 0) >= ?2 ORDER BY ROWID",
                )
                .map_err(failed)?;
            let mut rows = stmt.query([state.rowid, from]).map_err(failed)?;
            while let Some(row) = rows.next().map_err(failed)? {
                if files.len() >= max {
                    more = true;
                    break;
                }
                let rowid: i64 = row.get(0).map_err(failed)?;
                let mailbox: i64 = row.get(1).map_err(failed)?;
                last = rowid;
                let Some(mailbox) = wanted(mailbox) else {
                    continue;
                };
                match finder.find(&mut seen, mailbox, rowid) {
                    Some(file) => files.push(file),
                    // Mail lists it, but has not written it yet: read it on a later run.
                    None => add_waiting(&mut waiting, Waiting(rowid, rowid, now.0)),
                }
            }
            if !more {
                last = last.max(newest);
            }
        }
        state.rowid = last;
        state.waiting = waiting;
        let idle = files.is_empty() && state.waiting.is_empty() && !more;
        let cursor = state.write();
        if idle && let Some(stamp) = stamp {
            if seen.idle.len() >= 64 {
                seen.idle.clear();
            }
            seen.idle
                .insert(request_key(request, cursor.as_str()), stamp);
        }
        drop(seen);
        if files.is_empty() {
            return Ok(Fetched::nothing(cursor).with_more(more));
        }
        let batch = AppleMailBatch {
            store: snapshot.store.clone(),
            files,
        };
        Ok(Fetched::batch(batch, cursor).with_more(more))
    }
}

/// Messages waited for: the numbers `.0` to `.1` of the index, first seen at `.2`
/// (milliseconds since 1970).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Waiting(i64, i64, i64);

/// Adds messages to wait for, after the ones already there, joining them to the last range
/// when they follow it and were first seen at the same time.
fn add_waiting(waiting: &mut Vec<Waiting>, range: Waiting) {
    if let Some(last) = waiting.last_mut()
        && last.1.checked_add(1) == Some(range.0)
        && last.2 == range.2
    {
        last.1 = range.1;
        return;
    }
    waiting.push(range);
}

/// The cursor: the store, the last message of its index read, and the messages waited for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    store: String,
    rowid: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waiting: Vec<Waiting>,
}

impl State {
    fn read(cursor: Option<&Cursor>) -> State {
        cursor
            .and_then(|c| serde_json::from_str(c.as_str()).ok())
            .unwrap_or_default()
    }

    fn write(&self) -> Cursor {
        Cursor::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// A copy of the Envelope Index of a store, checked.
struct Snapshot {
    copy: DatabaseCopy,
    store: PathBuf,
}

/// A mailbox of the index.
#[derive(Debug, Clone)]
struct Mailbox {
    /// Its URL in the index: `imap://<account>/INBOX`, `local:///Archive`.
    url: String,
    /// Its name for people.
    name: String,
    /// Its path of folders: `INBOX`, `[Gmail]/All Mail`.
    path: Vec<String>,
    /// The folder of its account in the store: the account's id, or `Mailboxes` for the
    /// mailboxes On My Mac.
    account: String,
}

impl Mailbox {
    fn is_junk(&self) -> bool {
        is_junk_folder(&self.path.join("/"))
    }
}

impl Snapshot {
    /// The mailboxes of the index, by number, named for people.
    fn mailboxes(&self) -> Result<BTreeMap<i64, Mailbox>, SourceError> {
        let conn = self.copy.conn();
        let mut stmt = conn
            .prepare("SELECT ROWID, url FROM mailboxes")
            .map_err(failed)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(failed)?;
        let mut mailboxes = BTreeMap::new();
        for row in rows {
            let (rowid, url) = row.map_err(failed)?;
            let Some(url) = url else { continue };
            let Some((account, path)) = parse_url(&url) else {
                continue;
            };
            let name = path.join("/");
            mailboxes.insert(
                rowid,
                Mailbox {
                    url,
                    name,
                    path,
                    account,
                },
            );
        }
        // A mailbox name more than one account has is told apart by the account's address.
        let mut accounts_of: HashMap<String, Vec<String>> = HashMap::new();
        for m in mailboxes.values() {
            let accounts = accounts_of.entry(m.name.to_lowercase()).or_default();
            if !accounts.contains(&m.account) {
                accounts.push(m.account.clone());
            }
        }
        let mut labels: HashMap<String, String> = HashMap::new();
        let shared: Vec<i64> = mailboxes
            .iter()
            .filter(|(_, m)| accounts_of[&m.name.to_lowercase()].len() > 1)
            .map(|(id, _)| *id)
            .collect();
        for id in shared {
            let account = mailboxes[&id].account.clone();
            if !labels.contains_key(&account) {
                let label = account_label(conn, &mailboxes, &account);
                labels.insert(account.clone(), label);
            }
            let m = mailboxes.get_mut(&id).expect("listed");
            m.name = format!("{} - {}", m.name, labels[&account]);
        }
        Ok(mailboxes)
    }
}

/// How an account is named for people: the address it sends from, read from its sent
/// mailbox when the index has one, or `On My Mac`, or the start of its id.
fn account_label(
    conn: &rusqlite::Connection,
    mailboxes: &BTreeMap<i64, Mailbox>,
    account: &str,
) -> String {
    if account == LOCAL {
        return "On My Mac".into();
    }
    let sent = mailboxes.iter().find(|(_, m)| {
        m.account == account
            && m.path.last().is_some_and(|p| {
                matches!(
                    p.to_lowercase().as_str(),
                    "sent" | "sent messages" | "sent items" | "sent mail"
                )
            })
    });
    // Best effort: `sender` and `addresses` are not needed to read mail, so a store without
    // them still works and names the account by its id.
    let address = sent.and_then(|(id, _)| {
        conn.query_row(
            "SELECT a.address FROM messages m JOIN addresses a ON a.ROWID = m.sender \
             WHERE m.mailbox = ?1 GROUP BY a.address ORDER BY COUNT(*) DESC LIMIT 1",
            [id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    });
    address.unwrap_or_else(|| format!("account {}", account.chars().take(8).collect::<String>()))
}

/// The account folder of the mailboxes On My Mac.
const LOCAL: &str = "Mailboxes";

/// The account folder and the path of a mailbox URL: `imap://<id>/%5BGmail%5D/All%20Mail` is
/// `<id>` and `[Gmail]`, `All Mail`; `local:///Archive` is `Mailboxes` and `Archive`.
fn parse_url(url: &str) -> Option<(String, Vec<String>)> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path: Vec<String> = path
        .split('/')
        .filter(|p| !p.is_empty())
        .map(percent_decode)
        .collect();
    if path.is_empty() {
        return None;
    }
    let host = percent_decode(host);
    let account = if scheme.eq_ignore_ascii_case("local") || host.is_empty() {
        LOCAL.to_string()
    } else {
        // The account's folder in the store is named by its id, which the URL carries.
        host
    };
    Some((account, path))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Finds the `.emlx` file of a message of the index in the store, for one run.
struct Finder<'a> {
    store: &'a Path,
    /// By mailbox number: its folder and its message folders (`<id>/Data`, `Data`).
    folders: HashMap<i64, Option<(PathBuf, Vec<PathBuf>)>>,
}

impl<'a> Finder<'a> {
    fn new(store: &'a Path) -> Self {
        Finder {
            store,
            folders: HashMap::new(),
        }
    }

    fn find(&mut self, seen: &mut Seen, mailbox: (i64, &Mailbox), rowid: i64) -> Option<PathBuf> {
        let store = self.store;
        let (root, data) = self
            .folders
            .entry(mailbox.0)
            .or_insert_with(|| {
                mailbox_folder(store, mailbox.1).map(|root| {
                    let data = data_folders(&root);
                    (root, data)
                })
            })
            .as_ref()?;
        // Mail files a message under `Data/<digits of rowid / 1000, last first>/Messages`.
        let mut digits = PathBuf::new();
        if rowid >= 1000 {
            for d in (rowid / 1000).to_string().chars().rev() {
                digits.push(d.to_string());
            }
        }
        let dirs = data
            .iter()
            .map(|base| base.join(&digits).join("Messages"))
            .chain(std::iter::once(root.join("Messages")));
        for dir in dirs {
            if let Some(file) = seen.listing(&dir).get(&rowid) {
                return Some(file.clone());
            }
        }
        None
    }
}

/// The folder of a mailbox in the store: `<account>/INBOX.mbox`, `<account>/[Gmail].mbox/All
/// Mail.mbox`.
fn mailbox_folder(store: &Path, mailbox: &Mailbox) -> Option<PathBuf> {
    let mut folder = store.join(&mailbox.account);
    for part in &mailbox.path {
        let mbox = folder.join(format!("{part}.mbox"));
        folder = if mbox.is_dir() {
            mbox
        } else {
            let imap = folder.join(format!("{part}.imapmbox"));
            if !imap.is_dir() {
                return None;
            }
            imap
        };
    }
    Some(folder)
}

/// The `Data` folders of a mailbox folder: `<id>/Data` for each folder in it, and `Data`.
fn data_folders(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            let data = path.join("Data");
            if !is_mailbox_dir(&path) && data.is_dir() {
                found.push(data);
            }
        }
    }
    found.sort();
    let data = folder.join("Data");
    if data.is_dir() {
        found.push(data);
    }
    found
}

fn is_mailbox_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.ends_with(".mbox") || name.ends_with(".imapmbox")
}

/// New messages of Apple Mail, written by the mail importer.
pub struct AppleMailBatch {
    store: PathBuf,
    files: Vec<PathBuf>,
}

impl fmt::Debug for AppleMailBatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleMailBatch")
            .field("messages", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl Batch for AppleMailBatch {
    fn len(&self) -> usize {
        self.files.len()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let report =
            import_apple_mail_files(space, &self.store, &self.files, &MailOptions::default(), by)
                .map_err(|e| match e {
                MailError::NoAccess { .. } => no_access(),
                MailError::Space(e) => SourceError::Space(e),
                e => SourceError::Failed(e.to_string()),
            })?;
        Ok(Written {
            items: report.added,
            created: report.created.len() + report.people,
            updated: report.updated.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_says_whether_mail_has_a_store() {
        let dir = tempfile::tempdir().unwrap();
        let missing = AppleMailConnector::new(dir.path().join("Mail"));
        assert!(matches!(
            missing.readiness(),
            Readiness::Blocked(SourceError::Unavailable(why)) if why.contains("add an account")
        ));
        std::fs::create_dir_all(dir.path().join("Mail")).unwrap();
        let empty = AppleMailConnector::new(dir.path().join("Mail"));
        assert!(matches!(
            empty.readiness(),
            Readiness::Blocked(SourceError::Unavailable(_))
        ));
        std::fs::create_dir_all(dir.path().join("Mail/V10")).unwrap();
        assert!(matches!(empty.readiness(), Readiness::Ready));
    }

    #[test]
    fn reads_mailbox_urls() {
        assert_eq!(
            parse_url("imap://ABC-123/%5BGmail%5D/All%20Mail"),
            Some(("ABC-123".into(), vec!["[Gmail]".into(), "All Mail".into()]))
        );
        assert_eq!(
            parse_url("local:///Archive/2026"),
            Some((LOCAL.into(), vec!["Archive".into(), "2026".into()]))
        );
        assert_eq!(parse_url("ews://ABC/"), None);
        assert_eq!(parse_url("not a url"), None);
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%C3%A1"), "á");
    }

    #[test]
    fn cursors_read_back() {
        let state = State {
            store: "V10".into(),
            rowid: 42,
            waiting: vec![Waiting(43, 50, 1000)],
        };
        assert_eq!(State::read(Some(&state.write())), state);
        assert_eq!(State::read(Some(&Cursor::new("junk"))), State::default());
        assert_eq!(State::read(None), State::default());
    }

    #[test]
    fn waiting_messages_join_into_ranges() {
        let mut waiting = Vec::new();
        for rowid in 1..=5000 {
            add_waiting(&mut waiting, Waiting(rowid, rowid, 7));
        }
        add_waiting(&mut waiting, Waiting(5001, 5001, 8));
        add_waiting(&mut waiting, Waiting(6000, 6010, 8));
        assert_eq!(
            waiting,
            [
                Waiting(1, 5000, 7),
                Waiting(5001, 5001, 8),
                Waiting(6000, 6010, 8)
            ]
        );
    }
}

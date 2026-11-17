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
//!   still downloading when the copy was taken) are kept in the cursor and read on a later
//!   run, for up to a day. When Mail rebuilds its index or moves to a new `V<n>`, the numbers
//!   start again and so does the cursor: what was read before is found again by its
//!   `Message-ID` and added nothing twice.
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
use std::time::Duration;

use knode_format::{Provenance, Space, Timestamp};
use knode_ingest::sources::sqlite::{DatabaseCopy, check_schema, open_copy};
use knode_ingest::sources::{
    Batch, Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, SourceError,
    SourceInfo, Trigger, Written,
};
use knode_vault::mail::{MailError, MailOptions, import_apple_mail_files, is_junk_folder};
use serde::{Deserialize, Serialize};

/// The id of the connector, the same as the Apple Mail importer's.
pub const ID: &str = "apple-mail";

/// The tables and columns of the Envelope Index the connector reads.
pub const SCHEMA: &[(&str, &[&str])] = &[
    ("messages", &["ROWID", "mailbox", "date_received", "deleted"]),
    ("mailboxes", &["ROWID", "url"]),
];

/// The longest time between two runs when no change of the index was seen.
const RUN_AT_LEAST_EVERY: Duration = Duration::from_secs(120);

/// How long a message the index lists is waited for when its file is not there yet.
const WAIT_FOR_FILE_MS: i64 = 24 * 3600 * 1000;

/// The most messages waited for at once.
const MAX_WAITING: usize = 1000;

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

fn no_access() -> SourceError {
    SourceError::Permission(Permission::FullDiskAccess, NO_ACCESS.into())
}

fn failed(error: rusqlite::Error) -> SourceError {
    SourceError::Failed(format!("Apple Mail's index could not be read: {error}"))
}

impl Connector for AppleMailConnector {
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
                let mut wal = index.clone().into_os_string();
                wal.push("-wal");
                vec![index, PathBuf::from(wal)]
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
        };
        let from = match request.history {
            History::Everything => i64::MIN,
            History::FromToday => request.since.0.div_euclid(1000),
        };
        let now = Timestamp::now();
        let max = request.max_items.max(1);
        let mut finder = Finder::default();
        let mut files: Vec<PathBuf> = Vec::new();
        let mut waiting: Vec<(i64, i64)> = Vec::new();

        // The messages waited for, first.
        {
            let mut stmt = conn
                .prepare("SELECT mailbox, deleted FROM messages WHERE ROWID = ?1")
                .map_err(failed)?;
            for (rowid, first_seen) in std::mem::take(&mut state.waiting) {
                let row: Option<(i64, Option<i64>)> = stmt
                    .query_row([rowid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        e => Err(e),
                    })
                    .map_err(failed)?;
                let Some((mailbox, deleted)) = row else {
                    continue;
                };
                let Some(mailbox) = wanted(mailbox).filter(|_| deleted.unwrap_or(0) == 0) else {
                    continue;
                };
                match finder.find(&snapshot.store, mailbox, rowid) {
                    Some(file) => files.push(file),
                    None if now.0 - first_seen < WAIT_FOR_FILE_MS => {
                        waiting.push((rowid, first_seen))
                    }
                    None => {}
                }
            }
        }

        // Then what is new after the cursor.
        let mut last = state.rowid;
        let mut more = false;
        {
            let mut stmt = conn
                .prepare(
                    "SELECT ROWID, mailbox, date_received FROM messages \
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
                let received: Option<i64> = row.get(2).map_err(failed)?;
                last = rowid;
                let Some(mailbox) = wanted(mailbox) else {
                    continue;
                };
                match finder.find(&snapshot.store, mailbox, rowid) {
                    Some(file) => files.push(file),
                    // Mail lists it, but is still writing it: read it on a later run.
                    None if waiting.len() < MAX_WAITING
                        && received.unwrap_or(0).saturating_mul(1000)
                            >= now.0 - WAIT_FOR_FILE_MS =>
                    {
                        waiting.push((rowid, now.0));
                    }
                    // An older message Mail keeps only on the server.
                    None => {}
                }
            }
        }
        if !more {
            last = last.max(newest);
        }
        state.rowid = last;
        state.waiting = waiting;
        let cursor = state.write();
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

/// The cursor: the store, the last message of its index read, and the messages waited for
/// (their number and when they were first seen, in milliseconds since 1970).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    store: String,
    rowid: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waiting: Vec<(i64, i64)>,
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
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
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

/// Finds the `.emlx` file of a message of the index in the store.
#[derive(Default)]
struct Finder {
    /// By mailbox folder: its message folders (`<id>/Data`, `Data`).
    data: HashMap<PathBuf, Vec<PathBuf>>,
    /// By mailbox folder: every message file in it by number, once it was walked.
    walked: HashMap<PathBuf, HashMap<i64, PathBuf>>,
}

impl Finder {
    fn find(&mut self, store: &Path, mailbox: &Mailbox, rowid: i64) -> Option<PathBuf> {
        let folder = mailbox_folder(store, mailbox)?;
        let names = [format!("{rowid}.emlx"), format!("{rowid}.partial.emlx")];
        // Mail files a message under `Data/<digits of rowid / 1000, last first>/Messages`.
        let mut digits = PathBuf::new();
        if rowid >= 1000 {
            for d in (rowid / 1000).to_string().chars().rev() {
                digits.push(d.to_string());
            }
        }
        let data = self
            .data
            .entry(folder.clone())
            .or_insert_with(|| data_folders(&folder));
        for base in data.iter() {
            for name in &names {
                let file = base.join(&digits).join("Messages").join(name);
                if file.is_file() {
                    return Some(file);
                }
            }
        }
        for name in &names {
            let file = folder.join("Messages").join(name);
            if file.is_file() {
                return Some(file);
            }
        }
        // Laid out some other way: look for it once in the whole mailbox.
        self.walked
            .entry(folder.clone())
            .or_insert_with(|| walk(&folder))
            .get(&rowid)
            .cloned()
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

/// Every message file of a mailbox folder, by number, leaving out the mailboxes in it.
fn walk(folder: &Path) -> HashMap<i64, PathBuf> {
    let mut found = HashMap::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if !is_mailbox_dir(&path) {
                    stack.push(path);
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name
                .strip_suffix(".partial.emlx")
                .or_else(|| name.strip_suffix(".emlx"))
                .and_then(|n| n.parse::<i64>().ok());
            if let Some(number) = number {
                found.entry(number).or_insert(path);
            }
        }
    }
    found
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
        let report = import_apple_mail_files(
            space,
            &self.store,
            &self.files,
            &MailOptions::default(),
            by,
        )
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
            waiting: vec![(43, 1000)],
        };
        assert_eq!(State::read(Some(&state.write())), state);
        assert_eq!(State::read(Some(&Cursor::new("junk"))), State::default());
        assert_eq!(State::read(None), State::default());
    }
}

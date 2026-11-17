//! Mail over IMAP, live, from any mail server: [`ImapConnector`].
//!
//! - **Signing in**: the server, the user name and an app password (for accounts with two-step
//!   verification, such as Gmail, iCloud, Yahoo or Outlook), checked by signing in once. The
//!   account and the password are kept in the system keychain (`_source-imap-account`,
//!   `_source-imap-password`), never in a space or a file; signing out or disconnecting forgets
//!   them. Only IMAP over TLS (port 993) is spoken: never plain text, never `STARTTLS`.
//! - **New mail**: every minute the connector asks each mailbox for its `UIDVALIDITY` and
//!   `UIDNEXT` (`STATUS`) and reads only the mailboxes that changed, from the last UID read on
//!   (`UID SEARCH`, `UID FETCH ... BODY.PEEK[]`, which leaves messages unread). It does not
//!   keep a connection open with `IDLE`: Knode Service runs every connector on one thread, and
//!   a run must end. The cursor keeps, per mailbox, the `UIDVALIDITY`, the last UID read and
//!   the `UIDNEXT` seen; when the server changes a mailbox's `UIDVALIDITY` the mailbox is read
//!   again, and what was read before is found by its `Message-ID` and not added twice.
//! - **Gmail**: when the server speaks Gmail's extension (`X-GM-EXT-1`), only `All Mail` is
//!   read, once, with the labels of each message (`X-GM-LABELS`) as its folders, instead of
//!   every label as a mailbox. Spam and trash are not in `All Mail`.
//! - **Containers** are the mailboxes (on Gmail, the labels), by their name on the server.
//!   Every one comes in, new ones too, except the ones excluded and spam and trash (mailboxes
//!   marked `\Junk` or `\Trash`, or named `Junk`, `Spam`, `Trash`, `Bin`, `Deleted Messages` or
//!   `Deleted Items`), which are never read. On Gmail a message with an excluded label is left
//!   out even when it has other labels.
//! - **History**: with [`History::FromToday`], nothing the server received before the day the
//!   connector was connected is read.
//! - The messages are written by the mail importer ([`knode_vault::mail::import_raw_mail`]),
//!   so a message already in the space from a Gmail Takeout `.mbox`, Apple Mail or the Gmail
//!   API connector is found by its `Message-ID` and not added again. Attachments are not
//!   attached: the files are named in the thread. Messages over 40 MB are left out.

pub mod client;
pub mod wire;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Days, Local, NaiveDate};
use knode_ingest::sources::{
    Connector, Container, Cursor, FetchRequest, Fetched, History, Permission, Secrets, SignInState,
    SignInStep, SourceError, SourceInfo, Trigger,
};
use knode_vault::mail::{MailSource, RawMail, is_junk_folder};
use serde::{Deserialize, Serialize};

use self::client::{
    Account, FetchedMessage, IMAPS_PORT, ImapConnect, ImapError, ImapSession, Mailbox,
};
use super::mail_live::{MailBatch, gmail_system_label, is_gmail_junk};

/// The id of the connector.
pub const ID: &str = "imap";

/// The secret holding the account (server and user name), as JSON.
pub const ACCOUNT_SECRET: &str = "account";
/// The secret holding the app password.
pub const PASSWORD_SECRET: &str = "password";

/// How often mailboxes are looked at for new mail.
pub const POLL_EVERY: Duration = Duration::from_secs(60);

/// The most messages one fetch reads, whatever the service asks: a fetch must end soon.
const MAX_PER_FETCH: usize = 200;
/// How many messages are read with one command.
const CHUNK: usize = 25;
/// How long one fetch reads before it stops and says there is more.
const TIME_BUDGET: Duration = Duration::from_secs(20);

/// What a person reads when the connector is not signed in.
const NOT_SIGNED_IN: &str = "sign in to your mail account first: in the Mac app, Settings > \
     Sources > Mail (IMAP) > Connect; or run knode sources sign-in imap";

/// Mail over IMAP. See the [module](self).
pub struct ImapConnector {
    net: Arc<dyn ImapConnect>,
    /// The account given in the first step of signing in, until its app password comes.
    pending: Mutex<Option<Account>>,
}

impl std::fmt::Debug for ImapConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConnector").finish_non_exhaustive()
    }
}

impl Default for ImapConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl ImapConnector {
    /// The connector, over the network with TLS.
    pub fn new() -> Self {
        Self::with_network(Arc::new(wire::TlsImap))
    }

    /// The connector over `net`: a server in memory, in tests.
    pub fn with_network(net: Arc<dyn ImapConnect>) -> Self {
        ImapConnector {
            net,
            pending: Mutex::new(None),
        }
    }

    fn account(secrets: &dyn Secrets) -> Result<Option<(Account, String)>, SourceError> {
        let account = secrets.get(ACCOUNT_SECRET)?;
        let password = secrets.get(PASSWORD_SECRET)?;
        match (account, password) {
            (Some(account), Some(password)) => {
                let account: Account = serde_json::from_str(&account).map_err(|_| {
                    SourceError::SignIn(format!(
                        "the saved account cannot be read: {NOT_SIGNED_IN}"
                    ))
                })?;
                Ok(Some((account, password)))
            }
            _ => Ok(None),
        }
    }

    fn session(
        &self,
        secrets: &dyn Secrets,
    ) -> Result<(Account, Box<dyn ImapSession>), SourceError> {
        let (account, password) =
            Self::account(secrets)?.ok_or_else(|| SourceError::SignIn(NOT_SIGNED_IN.into()))?;
        let session = self
            .net
            .connect(&account, &password)
            .map_err(|e| sign_in_error(&account, e))?;
        Ok((account, session))
    }
}

fn sign_in_error(account: &Account, error: ImapError) -> SourceError {
    match error {
        ImapError::Login(what) => SourceError::SignIn(format!(
            "{} refused the user name or app password of {} ({what}): sign in again with a \
             new app password",
            account.host,
            account.describe()
        )),
        other => failed(other),
    }
}

fn failed(error: ImapError) -> SourceError {
    SourceError::Failed(error.to_string())
}

/// What a fetch knows of the server's mailboxes.
struct Layout {
    /// Gmail: only `All Mail` is read, with labels.
    gmail: Option<Gmail>,
    mailboxes: Vec<Mailbox>,
}

/// Gmail's special mailboxes, by what they are.
struct Gmail {
    all: String,
    /// The mailbox of each system label: `\Sent` is `[Gmail]/Sent Mail`.
    system: BTreeMap<&'static str, String>,
}

impl Layout {
    fn read(session: &mut dyn ImapSession) -> Result<Layout, SourceError> {
        let capabilities = session.capabilities().map_err(failed)?;
        let mailboxes = session.list().map_err(failed)?;
        let gmail = if capabilities.iter().any(|c| c == "X-GM-EXT-1") {
            mailboxes.iter().find(|m| m.has("\\All")).map(|all| {
                let mut system = BTreeMap::new();
                for (label, attribute) in [
                    ("\\Sent", "\\Sent"),
                    ("\\Important", "\\Important"),
                    ("\\Starred", "\\Flagged"),
                    ("\\Draft", "\\Drafts"),
                ] {
                    if let Some(m) = mailboxes.iter().find(|m| m.has(attribute)) {
                        system.insert(label, m.name.clone());
                    }
                }
                system.insert("\\Inbox", "INBOX".to_string());
                Gmail {
                    all: all.name.clone(),
                    system,
                }
            })
        } else {
            None
        };
        Ok(Layout { gmail, mailboxes })
    }

    /// Whether a mailbox is spam or trash.
    fn is_junk(mailbox: &Mailbox) -> bool {
        mailbox.has("\\Junk") || mailbox.has("\\Trash") || is_junk_folder(&mailbox.display_name())
    }

    /// The mailboxes (on Gmail, the labels) a person can exclude.
    fn containers(&self) -> Vec<Container> {
        let mut out: Vec<Container> = self
            .mailboxes
            .iter()
            .filter(|m| m.selectable() && !m.has("\\All") && !Self::is_junk(m))
            .map(|m| Container::new(m.name.clone(), m.display_name()))
            .collect();
        out.sort_by(|a, b| (a.id != "INBOX", &a.name).cmp(&(b.id != "INBOX", &b.name)));
        out
    }

    /// The mailboxes to read.
    fn to_read(&self, request: &FetchRequest<'_>) -> Vec<&Mailbox> {
        match &self.gmail {
            Some(gmail) => self
                .mailboxes
                .iter()
                .filter(|m| m.name == gmail.all)
                .collect(),
            None => self
                .mailboxes
                .iter()
                .filter(|m| {
                    m.selectable()
                        && !m.has("\\All")
                        && !Self::is_junk(m)
                        && !request.is_excluded(&m.name)
                })
                .collect(),
        }
    }

    fn display(&self, name: &str) -> String {
        self.mailboxes
            .iter()
            .find(|m| m.name == name)
            .map(Mailbox::display_name)
            .unwrap_or_else(|| client::decode_utf7(name))
    }

    /// The folders of a message read in `mailbox`, or `None` when it is left out: spam, trash
    /// or an excluded label (on Gmail).
    fn folders(
        &self,
        mailbox: &Mailbox,
        message: &FetchedMessage,
        request: &FetchRequest<'_>,
    ) -> Option<Vec<String>> {
        let Some(gmail) = &self.gmail else {
            return Some(vec![mailbox.display_name()]);
        };
        let mut folders = Vec::new();
        for label in &message.gmail_labels {
            if is_gmail_junk(label) {
                return None;
            }
            let system = gmail
                .system
                .iter()
                .find(|(l, _)| l.eq_ignore_ascii_case(label));
            let id = match system {
                Some((_, mailbox)) => mailbox.clone(),
                None => label.clone(),
            };
            if request.is_excluded(&id) {
                return None;
            }
            match gmail_system_label(label) {
                Some(Some(name)) => folders.push(name.to_string()),
                Some(None) => {}
                None if label.starts_with('\\') => {}
                None => folders.push(self.display(label)),
            }
        }
        Some(folders)
    }
}

/// Where the connector stopped, for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ImapCursor {
    /// The account read ([`Account::key`]): another account starts again.
    account: String,
    /// By mailbox name.
    #[serde(default)]
    mailboxes: BTreeMap<String, MailboxCursor>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MailboxCursor {
    /// The `UIDVALIDITY` the UIDs belong to.
    validity: u32,
    /// The last UID read.
    last: u32,
    /// The `UIDNEXT` once everything before it was read: while the server says the same, the
    /// mailbox has nothing new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next: Option<u32>,
}

impl ImapCursor {
    fn load(cursor: Option<&Cursor>, account: &Account) -> ImapCursor {
        let key = account.key();
        cursor
            .and_then(|c| serde_json::from_str::<ImapCursor>(c.as_str()).ok())
            .filter(|c| c.account == key)
            .unwrap_or(ImapCursor {
                account: key,
                mailboxes: BTreeMap::new(),
            })
    }

    fn save(&self) -> Cursor {
        Cursor::new(serde_json::to_string(self).unwrap_or_default())
    }
}

/// The day IMAP's `SEARCH SINCE` starts from for the history chosen: the day before the
/// connector was connected, as servers compare days in their own time zone; the exact time is
/// checked on each message.
fn since_day(request: &FetchRequest<'_>) -> Option<NaiveDate> {
    match request.history {
        History::Everything => None,
        History::FromToday => {
            let day = DateTime::from_timestamp_millis(request.since.0)
                .map(|d| d.with_timezone(&Local).date_naive())?;
            day.checked_sub_days(Days::new(1))
        }
    }
}

impl Connector for ImapConnector {
    fn info(&self) -> SourceInfo {
        SourceInfo::connector(
            ID,
            "Mail (IMAP)",
            "The mail of any account over IMAP with an app password: Gmail, iCloud, Fastmail, \
             Yahoo, Outlook, your own server",
        )
        .needing([Permission::SignIn, Permission::Network])
        .excluding("mailbox")
    }

    fn trigger(&self) -> Trigger {
        Trigger::Interval(POLL_EVERY)
    }

    fn containers(&self, request: &FetchRequest<'_>) -> Result<Vec<Container>, SourceError> {
        let (_, mut session) = self.session(request.secrets)?;
        let layout = Layout::read(session.as_mut());
        session.logout();
        Ok(layout?.containers())
    }

    fn sign_in_state(&self, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        Ok(match Self::account(secrets)? {
            Some((account, _)) => SignInState::SignedIn {
                account: account.describe(),
            },
            None => SignInState::SignedOut,
        })
    }

    /// [`SignInStep::Start`] with the account, `you@example.com`, followed by the server when
    /// it is not a well-known provider (`you@example.com imap.example.com`), then
    /// [`SignInStep::Password`] with an app password, checked by signing in once.
    fn sign_in(&self, step: SignInStep, secrets: &dyn Secrets) -> Result<SignInState, SourceError> {
        match step {
            SignInStep::Start { account } => {
                let account = parse_account(&account)?;
                let hint = format!("an app password for {}", account.describe());
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(account);
                Ok(SignInState::PasswordNeeded { hint: Some(hint) })
            }
            SignInStep::Password { password } => {
                let account = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .ok_or_else(|| {
                        SourceError::SignIn("give the account first, then its app password".into())
                    })?;
                self.check_and_keep(&account, &password, secrets)?;
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Ok(SignInState::SignedIn {
                    account: account.describe(),
                })
            }
            _ => Err(SourceError::SignIn(
                "Mail (IMAP) signs in with the account and an app password".into(),
            )),
        }
    }

    fn sign_out(&self, secrets: &dyn Secrets) -> Result<bool, SourceError> {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let password = secrets.delete(PASSWORD_SECRET)?;
        let account = secrets.delete(ACCOUNT_SECRET)?;
        Ok(password || account)
    }

    fn fetch(&self, request: &FetchRequest<'_>) -> Result<Fetched, SourceError> {
        let (account, mut session) = self.session(request.secrets)?;
        let result = read(session.as_mut(), &account, request);
        session.logout();
        result
    }
}

fn read(
    session: &mut dyn ImapSession,
    account: &Account,
    request: &FetchRequest<'_>,
) -> Result<Fetched, SourceError> {
    let started = Instant::now();
    let layout = Layout::read(session)?;
    let mut cursor = ImapCursor::load(request.cursor, account);
    let since = since_day(request);
    let mut budget = request.max_items.clamp(1, MAX_PER_FETCH);
    let mut more = false;
    let mut mails = Vec::new();
    for mailbox in layout.to_read(request) {
        let status = match session.status(&mailbox.name) {
            Ok(status) => status,
            // A mailbox the server lists but will not open is left for next time.
            Err(ImapError::Protocol(_)) => continue,
            Err(e) => return Err(failed(e)),
        };
        let mut state = cursor
            .mailboxes
            .get(&mailbox.name)
            .copied()
            .filter(|s| s.validity == status.uid_validity)
            .unwrap_or(MailboxCursor {
                validity: status.uid_validity,
                last: 0,
                next: None,
            });
        if state.next.is_some() && state.next == status.uid_next {
            cursor.mailboxes.insert(mailbox.name.clone(), state);
            continue;
        }
        if budget == 0 || started.elapsed() > TIME_BUDGET {
            more = true;
            break;
        }
        let examined = session.examine(&mailbox.name).map_err(failed)?;
        if examined.uid_validity != state.validity {
            state = MailboxCursor {
                validity: examined.uid_validity,
                last: 0,
                next: None,
            };
        }
        let mut uids: Vec<u32> = session
            .search(state.last.saturating_add(1), since)
            .map_err(failed)?
            .into_iter()
            .filter(|uid| *uid > state.last)
            .collect();
        uids.sort_unstable();
        uids.dedup();
        let take = uids.len().min(budget);
        let mut read_all = take == uids.len();
        for chunk in uids[..take].chunks(CHUNK) {
            if started.elapsed() > TIME_BUDGET {
                read_all = false;
                break;
            }
            let mut fetched = session
                .fetch(chunk, layout.gmail.is_some())
                .map_err(failed)?;
            fetched.sort_by_key(|m| m.uid);
            for message in fetched {
                let wanted = message.internal_date.is_none_or(|at| request.wants(at));
                if let (true, Some(bytes)) = (wanted, &message.bytes)
                    && let Some(folders) = layout.folders(mailbox, &message, request)
                {
                    mails.push(RawMail::new(bytes.clone(), folders));
                }
            }
            // Every UID asked for was read, even those the server no longer has.
            state.last = state.last.max(chunk.iter().copied().max().unwrap_or(0));
            budget = budget.saturating_sub(chunk.len());
        }
        if read_all {
            state.next = status.uid_next.or(examined.uid_next);
        } else {
            state.next = None;
            more = true;
        }
        cursor.mailboxes.insert(mailbox.name.clone(), state);
        if more {
            break;
        }
    }
    let saved = cursor.save();
    if mails.is_empty() {
        return Ok(Fetched::nothing(saved).with_more(more));
    }
    Ok(Fetched::batch(
        MailBatch {
            source: MailSource::Imap,
            mails,
        },
        saved,
    )
    .with_more(more))
}

/// The server and port a person typed: `imap.example.com` or `imap.example.com:993`.
fn parse_server(text: &str) -> Result<(String, u16), SourceError> {
    let text = text
        .trim()
        .trim_start_matches("imaps://")
        .trim_start_matches("imap://")
        .trim_end_matches('/');
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().map_err(|_| {
                SourceError::SignIn(format!(
                    "{port:?} is not a port: write the server as imap.example.com"
                ))
            })?;
            (host, port)
        }
        None => (text, IMAPS_PORT),
    };
    if host.is_empty() || host.contains(char::is_whitespace) || host.contains('/') {
        return Err(SourceError::SignIn(
            "write the server as imap.example.com".into(),
        ));
    }
    if port == 143 {
        return Err(SourceError::SignIn(
            "Knode reads mail over TLS only: use the server's IMAP over TLS port, 993".into(),
        ));
    }
    Ok((host.to_ascii_lowercase(), port))
}

/// The server of well-known mail providers, from the address.
fn known_server(username: &str) -> Option<&'static str> {
    let domain = username.rsplit_once('@')?.1.to_ascii_lowercase();
    Some(match domain.as_str() {
        "gmail.com" | "googlemail.com" => "imap.gmail.com",
        "icloud.com" | "me.com" | "mac.com" => "imap.mail.me.com",
        "outlook.com" | "hotmail.com" | "live.com" => "outlook.office365.com",
        "yahoo.com" => "imap.mail.yahoo.com",
        "fastmail.com" => "imap.fastmail.com",
        _ => return None,
    })
}

/// The account a person typed: the address or user name, then the server when it is not a
/// well-known provider's: `you@gmail.com`, `you@example.com imap.example.com:993`.
fn parse_account(text: &str) -> Result<Account, SourceError> {
    let mut parts = text.split_whitespace();
    let username = parts
        .next()
        .ok_or_else(|| SourceError::SignIn("give the email or user name of the account".into()))?;
    let server = match parts.next() {
        Some(server) => server.to_string(),
        None => known_server(username)
            .ok_or_else(|| {
                SourceError::SignIn(format!(
                    "add the IMAP server after the account, such as `{username} imap.example.com`"
                ))
            })?
            .to_string(),
    };
    if parts.next().is_some() {
        return Err(SourceError::SignIn(
            "give the account and the server only, such as `you@example.com imap.example.com`"
                .into(),
        ));
    }
    let (host, port) = parse_server(&server)?;
    Ok(Account {
        host,
        port,
        username: username.to_string(),
    })
}

impl ImapConnector {
    /// Signs in once with the app password and, when the server lets it, keeps the account
    /// and the password in the keychain.
    fn check_and_keep(
        &self,
        account: &Account,
        password: &str,
        secrets: &dyn Secrets,
    ) -> Result<(), SourceError> {
        // App passwords are shown in groups with spaces; the servers take them without.
        let password: String = if known_server(&account.username).is_some() {
            password.split_whitespace().collect()
        } else {
            password.trim().to_string()
        };
        if password.is_empty() {
            return Err(SourceError::SignIn("give the app password".into()));
        }
        let mut session = self.net.connect(account, &password).map_err(|e| match e {
            ImapError::Login(what) => SourceError::SignIn(format!(
                "{} refused the user name or app password: {what}",
                account.host
            )),
            other => SourceError::SignIn(other.to_string()),
        })?;
        session.logout();
        let json =
            serde_json::to_string(account).map_err(|e| SourceError::Failed(e.to_string()))?;
        secrets.set(ACCOUNT_SECRET, &json)?;
        secrets.set(PASSWORD_SECRET, &password)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

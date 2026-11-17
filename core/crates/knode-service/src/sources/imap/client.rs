//! The IMAP commands the connector uses, behind a trait so tests run against a server in
//! memory: [`ImapConnect`] signs in and returns an [`ImapSession`]. The real one is
//! [`TlsImap`](super::wire::TlsImap).

use std::fmt;

use base64::Engine;
use chrono::NaiveDate;
use knode_format::Timestamp;
use serde::{Deserialize, Serialize};

/// The port of IMAP over TLS.
pub const IMAPS_PORT: u16 = 993;

/// A mail account read over IMAP: the server and the user name. The app password is kept apart,
/// in the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The server, such as `imap.gmail.com`.
    pub host: String,
    /// The port: 993, IMAP over TLS, unless the person said otherwise.
    #[serde(default = "default_port")]
    pub port: u16,
    /// The user name, often the address.
    pub username: String,
}

fn default_port() -> u16 {
    IMAPS_PORT
}

impl Account {
    /// What tells this account from another in a cursor: `<user>@<host>:<port>`, in lower
    /// case.
    pub fn key(&self) -> String {
        format!("{}@{}:{}", self.username, self.host, self.port).to_lowercase()
    }

    /// The account for people: the user name, and the server when the name is not an
    /// address there.
    pub fn describe(&self) -> String {
        if self.username.contains('@') {
            self.username.clone()
        } else {
            format!("{} on {}", self.username, self.host)
        }
    }
}

/// A mailbox the server lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// Its name as the server sends it (modified UTF-7): the id the connector keeps.
    pub name: String,
    /// The character between the levels of its path, such as `/` or `.`.
    pub delimiter: Option<char>,
    /// Its attributes, in lower case: `\noselect`, `\junk`, `\trash`, `\all`, `\sent`.
    pub attributes: Vec<String>,
}

impl Mailbox {
    /// A mailbox with this name and these attributes, with `/` as the delimiter.
    pub fn new(name: impl Into<String>, attributes: &[&str]) -> Self {
        Mailbox {
            name: name.into(),
            delimiter: Some('/'),
            attributes: attributes.iter().map(|a| a.to_ascii_lowercase()).collect(),
        }
    }

    /// Whether it has the attribute `attribute` (`\Junk`), in any case.
    pub fn has(&self, attribute: &str) -> bool {
        self.attributes
            .iter()
            .any(|a| a.eq_ignore_ascii_case(attribute))
    }

    /// Whether messages can be read in it.
    pub fn selectable(&self) -> bool {
        !self.has("\\Noselect") && !self.has("\\NonExistent")
    }

    /// Its name for people: decoded, with `/` between its levels.
    pub fn display_name(&self) -> String {
        let name = decode_utf7(&self.name);
        match self.delimiter {
            Some(d) if d != '/' => name.replace(d, "/"),
            _ => name,
        }
    }
}

/// What the server says of a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxStatus {
    /// `UIDVALIDITY`: when it changes, the UIDs of the mailbox mean something else.
    pub uid_validity: u32,
    /// `UIDNEXT`: the UID the next message will get, when the server says.
    pub uid_next: Option<u32>,
}

/// One message read from the selected mailbox.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchedMessage {
    /// Its UID in the mailbox.
    pub uid: u32,
    /// When the server received it.
    pub internal_date: Option<Timestamp>,
    /// Its bytes; `None` when it was larger than the connector reads.
    pub bytes: Option<Vec<u8>>,
    /// Gmail's id of the message (`X-GM-MSGID`), on Gmail.
    pub gmail_id: Option<u64>,
    /// Gmail's labels of the message (`X-GM-LABELS`), on Gmail: `\Inbox`, `\Sent`, user labels
    /// by their mailbox name.
    pub gmail_labels: Vec<String>,
}

impl fmt::Debug for FetchedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchedMessage")
            .field("uid", &self.uid)
            .field("internal_date", &self.internal_date)
            .field("bytes", &self.bytes.as_ref().map(Vec::len))
            .field("gmail_id", &self.gmail_id)
            .finish_non_exhaustive()
    }
}

/// Why an IMAP command failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImapError {
    /// The server refused the user name or the password.
    Login(String),
    /// The server could not be reached, or the connection broke or timed out.
    Network(String),
    /// The server answered something the connector does not understand, or refused a command.
    Protocol(String),
}

impl fmt::Display for ImapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImapError::Login(what) => write!(f, "the server refused to sign in: {what}"),
            ImapError::Network(what) => write!(f, "the mail server could not be reached: {what}"),
            ImapError::Protocol(what) => write!(f, "the mail server answered: {what}"),
        }
    }
}

impl std::error::Error for ImapError {}

/// A signed-in IMAP connection.
pub trait ImapSession: Send {
    /// What the server can do, in upper case: `IMAP4REV1`, `X-GM-EXT-1`, `CONDSTORE`.
    fn capabilities(&mut self) -> Result<Vec<String>, ImapError>;
    /// Every mailbox (`LIST "" "*"`).
    fn list(&mut self) -> Result<Vec<Mailbox>, ImapError>;
    /// What the server says of a mailbox without selecting it (`STATUS`).
    fn status(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError>;
    /// Selects a mailbox read-only (`EXAMINE`).
    fn examine(&mut self, mailbox: &str) -> Result<MailboxStatus, ImapError>;
    /// The UIDs of the selected mailbox from `from_uid` on, received on or after `since` when
    /// given (`UID SEARCH UID <from>:* SINCE <day>`), in any order.
    fn search(&mut self, from_uid: u32, since: Option<NaiveDate>) -> Result<Vec<u32>, ImapError>;
    /// Reads these messages of the selected mailbox, without marking them read
    /// (`UID FETCH ... BODY.PEEK[]`), with Gmail's id and labels when `gmail`.
    fn fetch(&mut self, uids: &[u32], gmail: bool) -> Result<Vec<FetchedMessage>, ImapError>;
    /// Says goodbye; errors are ignored.
    fn logout(&mut self);
}

/// Opens signed-in IMAP connections.
pub trait ImapConnect: Send + Sync {
    /// Connects to the account's server over TLS and signs in with the password.
    fn connect(&self, account: &Account, password: &str)
    -> Result<Box<dyn ImapSession>, ImapError>;
}

/// Decodes a mailbox name in IMAP's modified UTF-7 (`&AOk-t&AOk-` is `été`). A name that is
/// not valid modified UTF-7 is returned as it is.
pub fn decode_utf7(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            return name.to_string();
        };
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            let b64 = encoded.replace(',', "/");
            let Ok(bytes) = base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64) else {
                return name.to_string();
            };
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            match String::from_utf16(&units) {
                Ok(text) => out.push_str(&text),
                Err(_) => return name.to_string(),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_modified_utf7() {
        assert_eq!(decode_utf7("INBOX"), "INBOX");
        assert_eq!(decode_utf7("&AOk-t&AOk-"), "été");
        assert_eq!(decode_utf7("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(decode_utf7("&ZeVnLIqe-"), "日本語");
        assert_eq!(decode_utf7("broken&"), "broken&");
    }

    #[test]
    fn mailboxes_say_what_they_are() {
        let mut inbox = Mailbox::new("Work.Clients", &["\\HasNoChildren"]);
        inbox.delimiter = Some('.');
        assert_eq!(inbox.display_name(), "Work/Clients");
        assert!(inbox.selectable());
        assert!(!Mailbox::new("[Gmail]", &["\\Noselect"]).selectable());
        assert!(Mailbox::new("Spam", &["\\Junk"]).has("\\junk"));
    }

    #[test]
    fn accounts_have_a_key() {
        let account = Account {
            host: "IMAP.example.com".into(),
            port: IMAPS_PORT,
            username: "Ana@example.com".into(),
        };
        assert_eq!(account.key(), "ana@example.com@imap.example.com:993");
        assert_eq!(account.describe(), "Ana@example.com");
    }
}

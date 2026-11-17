//! Messages a live connector read (over IMAP or from the Gmail API), as their RFC 822 bytes:
//! [`import_raw_mail`].

use std::collections::BTreeSet;

use knode_format::{Provenance, Space};

use super::message::{self, Origin};
use super::{MailError, MailOptions, MailReport, MailSource, thread, write};

/// One message as a mail server gives it: its bytes (RFC 822, the same bytes an `mbox` or an
/// `.emlx` file holds) and the folders it is in (Gmail labels or IMAP mailboxes).
#[derive(Clone, PartialEq, Eq)]
pub struct RawMail {
    /// The message, headers and body.
    pub bytes: Vec<u8>,
    /// The folders it is in, by name (`Inbox`, `Work/Clients`). When empty, the message's own
    /// `X-Gmail-Labels` header names them, if it has one.
    pub folders: Vec<String>,
}

impl RawMail {
    /// A message in these folders.
    pub fn new(bytes: impl Into<Vec<u8>>, folders: impl IntoIterator<Item = String>) -> Self {
        RawMail {
            bytes: bytes.into(),
            folders: folders.into_iter().collect(),
        }
    }
}

impl std::fmt::Debug for RawMail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The content of a message never reaches a log.
        f.debug_struct("RawMail")
            .field("bytes", &self.bytes.len())
            .field("folders", &self.folders.len())
            .finish()
    }
}

/// Imports messages a connector read into `space`, written by `by`, as `source`
/// ([`MailSource::Imap`] or [`MailSource::Gmail`]). They are threaded and written like the
/// messages of an `mbox` file: a message already in the space from any import of the same mail
/// (a Gmail Takeout `.mbox`, Apple Mail, IMAP or the Gmail API) is found by its `Message-ID`
/// (or, for a message without one, by its sender, date, subject and text) and added only once;
/// [`MailReport::added`] counts the new ones. Spam and trash are left out, and so are the
/// messages the folder and date filters of `options` leave out. Attachments are not read: the
/// files are named in the thread.
pub fn import_raw_mail(
    space: &mut Space,
    source: MailSource,
    mails: Vec<RawMail>,
    options: &MailOptions,
    by: &Provenance,
) -> Result<MailReport, MailError> {
    let mut report = MailReport::new(source);
    let mut collected = message::Collector::new(options);
    for (index, raw) in mails.into_iter().enumerate() {
        let order = collected.next_order();
        match message::parse(&raw.bytes, Origin::Elsewhere, None, order) {
            Some(mut mail) => {
                if !raw.folders.is_empty() {
                    mail.folders = raw.folders.into_iter().collect::<BTreeSet<_>>();
                }
                collected.add(mail);
            }
            None => report.skip(
                format!("message {}", index + 1),
                "it has no headers: it is not a mail message",
            ),
        }
    }
    report.filtered = collected.filtered;
    report.junk = collected.junk;
    report.duplicates = collected.duplicates;
    let threads = thread::group(collected.into_mails());
    let options = MailOptions {
        attachments: false,
        ..options.clone()
    };
    write::write(space, source, threads, &options, by, &mut report)?;
    Ok(report)
}

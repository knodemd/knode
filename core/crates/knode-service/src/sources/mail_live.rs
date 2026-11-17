//! What the live mail connectors ([`imap`](super::imap) and [`gmail`](super::gmail)) share:
//! the batch that hands their messages to the mail importer, and Gmail's system labels.

use knode_format::{Provenance, Space};
use knode_ingest::sources::{Batch, SourceError, Written};
use knode_vault::mail::{MailError, MailOptions, MailSource, RawMail, import_raw_mail};

/// Messages a mail connector read, written by the mail importer
/// ([`knode_vault::mail::import_raw_mail`]): one node per thread, the people of each thread,
/// edits made in Knode kept, and a message already in the space (from a Takeout `.mbox`, Apple
/// Mail, IMAP or the Gmail API) found by its `Message-ID` and not added again.
pub(crate) struct MailBatch {
    pub source: MailSource,
    pub mails: Vec<RawMail>,
}

impl Batch for MailBatch {
    fn len(&self) -> usize {
        self.mails.len()
    }

    fn write(self: Box<Self>, space: &mut Space, by: &Provenance) -> Result<Written, SourceError> {
        let report = import_raw_mail(space, self.source, self.mails, &MailOptions::default(), by)
            .map_err(|e| match e {
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

/// The name of a Gmail system label, as Google Takeout writes it in `X-Gmail-Labels`, so a
/// message brought in live is in the same folders as the same message from an export. `None`
/// for labels that are a state rather than a folder (`UNREAD`, `CHAT`) and for user labels,
/// which keep their own name.
///
/// `id` is a label id of the Gmail API (`INBOX`, `CATEGORY_UPDATES`) or a system label of
/// Gmail's IMAP extension (`\Inbox`, `\Sent`).
pub(crate) fn gmail_system_label(id: &str) -> Option<Option<&'static str>> {
    let name = match id.trim_start_matches('\\').to_ascii_uppercase().as_str() {
        "INBOX" => Some("Inbox"),
        "SENT" => Some("Sent"),
        "IMPORTANT" => Some("Important"),
        "STARRED" => Some("Starred"),
        "DRAFT" | "DRAFTS" => Some("Drafts"),
        "SPAM" | "JUNK" => Some("Spam"),
        "TRASH" => Some("Trash"),
        "CATEGORY_PERSONAL" => Some("Category Personal"),
        "CATEGORY_SOCIAL" => Some("Category Social"),
        "CATEGORY_PROMOTIONS" => Some("Category Promotions"),
        "CATEGORY_UPDATES" => Some("Category Updates"),
        "CATEGORY_FORUMS" => Some("Category Forums"),
        "UNREAD" | "CHAT" | "OPENED" => None,
        _ => return None,
    };
    Some(name)
}

/// Whether a Gmail label id (of the API or of IMAP) is spam or trash.
pub(crate) fn is_gmail_junk(id: &str) -> bool {
    matches!(
        id.trim_start_matches('\\').to_ascii_uppercase().as_str(),
        "SPAM" | "TRASH" | "JUNK"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_labels_take_the_names_takeout_gives_them() {
        assert_eq!(gmail_system_label("INBOX"), Some(Some("Inbox")));
        assert_eq!(gmail_system_label("\\Inbox"), Some(Some("Inbox")));
        assert_eq!(gmail_system_label("\\Draft"), Some(Some("Drafts")));
        assert_eq!(
            gmail_system_label("CATEGORY_UPDATES"),
            Some(Some("Category Updates"))
        );
        assert_eq!(gmail_system_label("UNREAD"), Some(None));
        assert_eq!(gmail_system_label("Label_12"), None);
        assert!(is_gmail_junk("SPAM"));
        assert!(is_gmail_junk("\\Trash"));
        assert!(!is_gmail_junk("INBOX"));
    }
}

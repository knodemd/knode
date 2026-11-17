//! Slack on this machine: signing in and out of the account the Slack connector reads.
//!
//! The account belongs to the machine, not to a space: one sign-in serves every space Slack is
//! connected to here. Its token is one of the connector's secrets ([`SourceSecrets`]), filed
//! as `_source-slack-token`. It is forgotten when the person signs out, and when Slack is
//! disconnected from the last space that used it on this machine.

use std::io;
use std::path::Path;
use std::time::Duration;

use knode_ingest::slack::{Account, SOURCE_ID, SlackConnector};

use super::{SourceError, SourceSecrets, StateFile};

/// How long signing in waits for the browser to come back from Slack.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

/// Signs in to Slack: `open` is given the page of Slack to open in the browser, and the call
/// returns once the person allowed Knode there (or refused, or [`SIGN_IN_TIMEOUT`] passed).
/// The token goes into the system keychain.
pub fn sign_in(home: &Path, open: &mut dyn FnMut(&str)) -> Result<Account, SourceError> {
    let connector = SlackConnector::from_env();
    let pending = connector.begin_sign_in()?;
    open(pending.url());
    let secrets = SourceSecrets::new(home, SOURCE_ID);
    connector.finish_sign_in(pending, &secrets, SIGN_IN_TIMEOUT)
}

/// Who is signed in to Slack on this machine, if anyone.
pub fn account(home: &Path) -> Result<Option<Account>, SourceError> {
    SlackConnector::from_env().account(&SourceSecrets::new(home, SOURCE_ID))
}

/// Signs out of Slack: asks Slack to revoke the token and forgets it. Returns whether it was
/// signed in.
pub fn sign_out(home: &Path) -> Result<bool, SourceError> {
    SlackConnector::from_env().sign_out(&SourceSecrets::new(home, SOURCE_ID), true)
}

/// After the source `id` was disconnected from a space and its state there forgotten: forgets
/// the Slack token when no space on this machine uses Slack any more. Returns whether it did.
/// The token is only forgotten here; signing out also revokes it with Slack.
pub fn forget_if_unused(home: &Path, id: &str) -> io::Result<bool> {
    if id != SOURCE_ID {
        return Ok(false);
    }
    forget_unused(&StateFile::load(home), &SourceSecrets::new(home, SOURCE_ID))
}

fn forget_unused(states: &StateFile, secrets: &SourceSecrets) -> io::Result<bool> {
    use knode_ingest::sources::Secrets;
    let suffix = format!("/{SOURCE_ID}");
    if states.sources.keys().any(|key| key.ends_with(&suffix)) {
        return Ok(false);
    }
    secrets.delete("token")
}

#[cfg(test)]
mod tests {
    use knode_ingest::sources::Secrets;

    use super::*;
    use crate::keys::MemoryKeyStore;

    #[test]
    fn the_token_is_forgotten_with_the_last_space() {
        let secrets = SourceSecrets::in_store(Box::new(MemoryKeyStore::new()), SOURCE_ID);
        secrets.set("token", "{}").unwrap();
        let mut states = StateFile::default();
        states.entry("space-a/slack");
        states.entry("space-b/telegram");
        assert!(!forget_unused(&states, &secrets).unwrap());
        assert!(secrets.get("token").unwrap().is_some());
        states.remove("space-a/slack");
        assert!(forget_unused(&states, &secrets).unwrap());
        assert!(secrets.get("token").unwrap().is_none());
        assert!(!forget_if_unused(Path::new("/nonexistent"), "telegram").unwrap());
    }
}

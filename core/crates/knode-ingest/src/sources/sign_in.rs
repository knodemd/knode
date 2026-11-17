//! Signing a connector in to the account it reads, step by step: [`SignIn`].
//!
//! A connector that needs [`Permission::SignIn`](super::Permission::SignIn) returns a
//! [`SignIn`] from [`Connector::sign_in`](super::Connector::sign_in). The person goes through
//! its steps in the Mac app's Connect sheet or in `knode sources sign-in`: each [`SignInStep`]
//! is a few fields to fill in (a server, a user name, an app password, a code), a page to open
//! in the browser (Sign in with Google), or both, and a button to go on. What the person enters
//! goes to Knode Service, which hands it to the connector; the connector keeps what it needs
//! (a refresh token, an app password) in the system keychain through [`Secrets`], never in a
//! space, a settings file or a log.
//!
//! The account belongs to this machine, not to a space: one sign-in serves every space the
//! connector is connected to.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Secrets, SourceError};

/// A field of a sign-in step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInField {
    /// Its stable name, the key of its value in the answer: `server`, `username`, `password`.
    pub id: String,
    /// Its label for people.
    pub label: String,
    /// What it holds, as an example: `imap.example.com`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    /// Hidden as it is typed, such as a password.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
    /// Monospaced, such as a code or a server name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
}

impl SignInField {
    /// A field with this id and label.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        SignInField {
            id: id.into(),
            label: label.into(),
            placeholder: String::new(),
            secret: false,
            code: false,
        }
    }

    /// The same field, with this example.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The same field, hidden as it is typed.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// The same field, monospaced.
    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }
}

/// One step of signing in: fields to fill in, a page to open in the browser, or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInStep {
    /// Its stable name, sent back with the answer: `account`, `browser`.
    pub id: String,
    /// Its title for people.
    pub title: String,
    /// What to do, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The fields to fill in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SignInField>,
    /// A page to open in the browser before going on, such as Google's sign-in page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_url: Option<String>,
    /// The title of the button that goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_title: Option<String>,
}

impl SignInStep {
    /// A step with this id and title, and nothing else yet.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        SignInStep {
            id: id.into(),
            title: title.into(),
            message: None,
            fields: Vec::new(),
            browser_url: None,
            continue_title: None,
        }
    }

    /// The same step, saying what to do.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// The same step, with one more field.
    pub fn field(mut self, field: SignInField) -> Self {
        self.fields.push(field);
        self
    }

    /// The same step, opening this page in the browser.
    pub fn browser(mut self, url: impl Into<String>) -> Self {
        self.browser_url = Some(url.into());
        self
    }

    /// The same step, with this title on the button that goes on.
    pub fn continue_title(mut self, title: impl Into<String>) -> Self {
        self.continue_title = Some(title.into());
        self
    }
}

/// What the person entered in a step, by field id. Never logged.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SignInValues(pub BTreeMap<String, String>);

impl SignInValues {
    /// The value of the field `id`, trimmed; `None` when it is missing or empty.
    pub fn get(&self, id: &str) -> Option<&str> {
        self.0.get(id).map(|v| v.trim()).filter(|v| !v.is_empty())
    }

    /// The same values with `id` set to `value`.
    pub fn with(mut self, id: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(id.into(), value.into());
        self
    }
}

impl std::fmt::Debug for SignInValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Passwords and codes never reach a log: only the names of the fields.
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// How a connector signs in to the account it reads. Steps go on until one returns `None`:
/// the person is signed in and the connector can be connected to a space.
pub trait SignIn: Send + Sync {
    /// The first step, or `None` when the connector is signed in already.
    fn start(&self, secrets: &dyn Secrets) -> Result<Option<SignInStep>, SourceError>;

    /// Gives the step `step` what the person entered and returns the next step, or `None` once
    /// signed in. An error is shown with the step, which stays to be tried again.
    fn answer(
        &self,
        step: &str,
        values: &SignInValues,
        secrets: &dyn Secrets,
    ) -> Result<Option<SignInStep>, SourceError>;

    /// The account the connector is signed in to, for people (an address), or `None` when it
    /// is not signed in.
    fn account(&self, secrets: &dyn Secrets) -> Result<Option<String>, SourceError>;

    /// Signs out: forgets every secret kept for the account. Returns whether it was signed in.
    fn sign_out(&self, secrets: &dyn Secrets) -> Result<bool, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_never_show_in_debug() {
        let values = SignInValues::default().with("password", "hunter2");
        assert_eq!(values.get("password"), Some("hunter2"));
        assert!(!format!("{values:?}").contains("hunter2"));
    }

    #[test]
    fn steps_serialize_as_the_app_reads_them() {
        let step = SignInStep::new("account", "Your mail account")
            .field(SignInField::new("password", "App password").secret())
            .browser("https://example.com/");
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["fields"][0]["secret"], true);
        assert_eq!(json["browser_url"], "https://example.com/");
        assert!(json.get("continue_title").is_none());
    }
}

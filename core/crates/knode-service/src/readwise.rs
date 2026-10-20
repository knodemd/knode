//! The Readwise access token, for `import.run` with `from` `readwise-api`.
//!
//! The token is never written to a space or a log: it is read from the `READWISE_TOKEN`
//! environment variable, or from the credential store agent keys use (the Keychain on macOS),
//! where `knode readwise token set` keeps it.

use std::io;
use std::path::Path;

use crate::keys::default_key_store;

/// The environment variable that gives the token, first.
pub const TOKEN_VAR: &str = "READWISE_TOKEN";

/// The name the token is kept under in the credential store. Agent names start with a letter,
/// so it is never the key of an agent.
pub const STORE_NAME: &str = "_readwise";

/// The Readwise token of this machine for the Knode home `home`: `READWISE_TOKEN`, else the one
/// in the credential store, else `None`.
pub fn token(home: &Path) -> io::Result<Option<String>> {
    if let Some(token) = std::env::var(TOKEN_VAR)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        return Ok(Some(token));
    }
    default_key_store(home).get(STORE_NAME)
}

/// Keeps `token` in the credential store of the Knode home `home`. Returns where it is kept.
pub fn set_token(home: &Path, token: &str) -> io::Result<String> {
    let token = token.trim();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a Readwise token is one word, from readwise.io/access_token",
        ));
    }
    let store = default_key_store(home);
    store.set(STORE_NAME, token)?;
    Ok(store.describe())
}

/// Forgets the token kept in the credential store. Returns whether there was one.
pub fn delete_token(home: &Path) -> io::Result<bool> {
    default_key_store(home).delete(STORE_NAME)
}

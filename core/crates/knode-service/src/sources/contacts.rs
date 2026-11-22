//! The names of people, from the person's Contacts, for connectors that show people by their
//! phone numbers and email addresses (Messages).
//!
//! Knode Service never reads Contacts: on macOS only an app the person allowed can. The Mac app
//! asks for access when the person wants names in Messages, and hands the service a map of
//! phone numbers and email addresses to names with `sources.contact_names`. The service keeps
//! it in a [`ContactBook`], shared by the connectors that name people, and in
//! `$KNODE_HOME/sources/contact-names.json` ([`CONTACTS_FILE`]), a file only its owner can read,
//! so names survive a restart before the app opens again. It is never written to a space; an
//! empty map forgets every name and removes the file.
//!
//! Phone numbers are matched by [`normalize_handle`] on both sides: international numbers in
//! E.164 (`+` and the digits), others as their national digits, matched against the end of an
//! international number (at least [`MIN_DIGITS`] of them). Email addresses are matched without
//! regard to case.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use knode_ingest::imessage::ContactNames;

use super::SOURCES_DIR;
use crate::keys::write_private;

/// The file in [`SOURCES_DIR`] with the names the app gave.
pub const CONTACTS_FILE: &str = "contact-names.json";

/// The fewest digits two phone numbers must share at their end to be taken as the same.
pub const MIN_DIGITS: usize = 7;

/// A phone number or an email address in the form both sides compare: an email address in lower
/// case; a phone number in E.164 (`+15550100`) when it has a country code (`+` or `00`), else
/// its digits without the leading zeros of a trunk prefix (`05550100` is `5550100`). Spaces,
/// dashes, dots, brackets and `tel:` or `mailto:` are left out. `None` for an empty handle.
pub fn normalize_handle(handle: &str) -> Option<String> {
    let mut h = handle.trim();
    for prefix in ["tel:", "mailto:", "sms:"] {
        if h.len() >= prefix.len() && h[..prefix.len()].eq_ignore_ascii_case(prefix) {
            h = h[prefix.len()..].trim();
        }
    }
    if h.is_empty() {
        return None;
    }
    if h.contains('@') {
        return Some(h.to_lowercase());
    }
    let is_phone = h
        .chars()
        .all(|c| c.is_ascii_digit() || " +-.()/\u{a0}".contains(c));
    let digits: String = h.chars().filter(char::is_ascii_digit).collect();
    if !is_phone || digits.is_empty() {
        return Some(h.to_lowercase());
    }
    if h.starts_with('+') {
        return Some(format!("+{digits}"));
    }
    if let Some(rest) = digits.strip_prefix("00") {
        return Some(format!("+{rest}"));
    }
    let national = digits.trim_start_matches('0');
    Some(if national.is_empty() {
        digits
    } else {
        national.to_string()
    })
}

/// The names, indexed for looking handles up.
#[derive(Debug, Default)]
struct Index {
    /// Every normalised handle given, to its name.
    exact: HashMap<String, String>,
    /// The digits of every phone number given, to its name.
    digits: HashMap<String, String>,
    /// Every ending of at least [`MIN_DIGITS`] digits of those, shorter than the whole number,
    /// to its name: the first in order wins.
    endings: HashMap<String, String>,
}

impl Index {
    fn new(names: &BTreeMap<String, String>) -> Index {
        let mut index = Index::default();
        for (key, name) in names {
            index.exact.entry(key.clone()).or_insert(name.clone());
            if let Some(digits) = phone_digits(key) {
                index.digits.entry(digits.clone()).or_insert(name.clone());
                for start in 1..=digits.len().saturating_sub(MIN_DIGITS) {
                    index
                        .endings
                        .entry(digits[start..].to_string())
                        .or_insert(name.clone());
                }
            }
        }
        index
    }

    fn name(&self, handle: &str) -> Option<&String> {
        let key = normalize_handle(handle)?;
        if let Some(name) = self.exact.get(&key) {
            return Some(name);
        }
        let digits = phone_digits(&key)?;
        // The longest number given that this one ends with, then one given that ends with it.
        (MIN_DIGITS..=digits.len())
            .rev()
            .find_map(|len| self.digits.get(&digits[digits.len() - len..]))
            .or_else(|| self.endings.get(&digits))
    }
}

/// The digits of a normalised phone number, when it is one.
fn phone_digits(key: &str) -> Option<String> {
    let digits = key.strip_prefix('+').unwrap_or(key);
    (digits.len() >= MIN_DIGITS && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| digits.to_string())
}

/// The names the app gave, shared by the connectors that name people. Empty until it gives
/// some, or [`ContactBook::load`] reads the ones it gave before.
#[derive(Debug, Default)]
pub struct ContactBook {
    names: RwLock<(BTreeMap<String, String>, Index)>,
}

impl ContactBook {
    /// An empty book.
    pub fn new() -> ContactBook {
        ContactBook::default()
    }

    /// The file the names are kept in, in the Knode home `home`.
    pub fn path(home: &Path) -> PathBuf {
        home.join(SOURCES_DIR).join(CONTACTS_FILE)
    }

    /// Reads the names kept in the Knode home `home`, if any. A file that cannot be read
    /// leaves the book empty: people are then shown by number until the app gives them again.
    pub fn load(&self, home: &Path) {
        let names: BTreeMap<String, String> = fs::read(Self::path(home))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        self.set(normalize(names));
    }

    /// Replaces every name with `names` (phone numbers and email addresses to names, in any
    /// form [`normalize_handle`] reads) and keeps them in the Knode home `home`, or removes the
    /// file when there are none. Returns how many handles it kept.
    pub fn replace(&self, home: &Path, names: BTreeMap<String, String>) -> io::Result<usize> {
        let names = normalize(names);
        let file = Self::path(home);
        if names.is_empty() {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        } else {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(home.join(SOURCES_DIR))?;
            let bytes = serde_json::to_vec(&names).map_err(io::Error::other)?;
            write_private(&file, &bytes)?;
        }
        let count = names.len();
        self.set(names);
        Ok(count)
    }

    /// How many handles have a name.
    pub fn len(&self) -> usize {
        self.read().0.len()
    }

    /// Whether no handle has a name.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn set(&self, names: BTreeMap<String, String>) {
        let index = Index::new(&names);
        *self.names.write().unwrap_or_else(|e| e.into_inner()) = (names, index);
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, (BTreeMap<String, String>, Index)> {
        self.names.read().unwrap_or_else(|e| e.into_inner())
    }
}

impl ContactNames for ContactBook {
    fn name(&self, handle: &str) -> Option<String> {
        self.read().1.name(handle).cloned()
    }
}

/// The names with their handles normalised and empty ones left out.
fn normalize(names: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (handle, name) in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if let Some(key) = normalize_handle(&handle) {
            out.entry(key).or_insert_with(|| name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(pairs: &[(&str, &str)]) -> ContactBook {
        let book = ContactBook::new();
        book.set(normalize(
            pairs
                .iter()
                .map(|(h, n)| (h.to_string(), n.to_string()))
                .collect(),
        ));
        book
    }

    #[test]
    fn handles_are_normalised_as_the_app_does() {
        let cases = [
            ("+1 (555) 010-0123", "+15550100123"),
            ("0044 20 7946 0958", "+442079460958"),
            ("tel:+34 600 000 000", "+34600000000"),
            ("020 7946 0958", "2079460958"),
            ("555.010.0123", "5550100123"),
            (" Ada@Example.COM ", "ada@example.com"),
            ("mailto:grace@example.com", "grace@example.com"),
            ("Apple", "apple"),
            ("12345", "12345"),
        ];
        for (raw, normal) in cases {
            assert_eq!(normalize_handle(raw).as_deref(), Some(normal), "{raw}");
        }
        assert_eq!(normalize_handle("  "), None);
    }

    #[test]
    fn numbers_match_with_or_without_a_country_code() {
        let b = book(&[
            ("+1 555 010 0123", "Ada Lovelace"),
            ("020 7946 0958", "Grace Hopper"),
            ("Linus@Example.com", "Linus"),
        ]);
        assert_eq!(b.name("+15550100123").as_deref(), Some("Ada Lovelace"));
        assert_eq!(b.name("5550100123").as_deref(), Some("Ada Lovelace"));
        assert_eq!(b.name("+442079460958").as_deref(), Some("Grace Hopper"));
        assert_eq!(b.name("linus@example.com").as_deref(), Some("Linus"));
        assert_eq!(b.name("+15550199999"), None);
        assert_eq!(b.name("0123"), None);
    }

    #[test]
    fn replacing_keeps_the_names_in_a_private_file_and_none_removes_it() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let b = ContactBook::new();
        let names = BTreeMap::from([
            ("+1 555 0100".to_string(), "Ada".to_string()),
            ("blank@example.com".to_string(), " ".to_string()),
        ]);
        assert_eq!(b.replace(home.path(), names).unwrap(), 1);
        let file = ContactBook::path(home.path());
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let again = ContactBook::new();
        again.load(home.path());
        assert_eq!(again.name("+15550100").as_deref(), Some("Ada"));
        assert_eq!(b.replace(home.path(), BTreeMap::new()).unwrap(), 0);
        assert!(!file.exists());
        assert!(b.is_empty());
        assert_eq!(b.name("+15550100"), None);
    }
}

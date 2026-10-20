//! Agent keys: making them, their fingerprints, and where a machine keeps them.
//!
//! A key is `knk_` and 64 lower case hexadecimal digits: 32 bytes from the system's secure
//! random source. Spaces only ever hold its fingerprint, `sha256:` and the SHA-256 of the key's
//! text. The key itself stays on the machine that made it, in a [`KeyStore`]: the Keychain on
//! macOS, the Secret Service on Linux when it is there, or else a file under the Knode home that
//! only its owner can read. See section 16 of the .knode specification.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// What every key starts with.
pub const KEY_PREFIX: &str = "knk_";
/// What every fingerprint starts with.
pub const FINGERPRINT_PREFIX: &str = "sha256:";
/// The environment variable that picks where keys are kept: `file` keeps them in files under
/// the Knode home, even where the system has a credential store.
pub const KEY_STORE_VAR: &str = "KNODE_KEY_STORE";
/// The folder in the Knode home where the file store keeps keys, one file per agent.
pub const KEYS_DIR: &str = "agent-keys";
/// The file in the Knode home that proves a client acts for the person who owns it.
pub const OWNER_TOKEN_FILE: &str = "owner.token";
/// The name keys are filed under in the Keychain and the Secret Service.
pub const STORE_SERVICE: &str = "md.knode.agent-key";

/// `n` bytes from the system's secure random source.
fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Makes a new agent key: `knk_` and 64 lower case hexadecimal digits, 256 random bits.
pub fn generate_key() -> io::Result<String> {
    Ok(format!("{KEY_PREFIX}{}", hex(&random_bytes::<32>()?)))
}

/// Whether `text` has the shape of an agent key.
pub fn is_key(text: &str) -> bool {
    text.strip_prefix(KEY_PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

/// The fingerprint of a key: `sha256:` and the lower case hexadecimal SHA-256 of its text.
pub fn fingerprint(key: &str) -> String {
    format!(
        "{FINGERPRINT_PREFIX}{}",
        hex(&Sha256::digest(key.as_bytes()))
    )
}

/// Where a machine keeps the keys of its agents, by agent name.
pub trait KeyStore: Send + Sync {
    /// The key of the agent, or `None` when this store has none.
    fn get(&self, agent: &str) -> io::Result<Option<String>>;
    /// Keeps the key of the agent, replacing the one it had.
    fn set(&self, agent: &str, key: &str) -> io::Result<()>;
    /// Forgets the key of the agent. Returns whether there was one.
    fn delete(&self, agent: &str) -> io::Result<bool>;
    /// Where the keys are, for people: "the Keychain", "a file in ...".
    fn describe(&self) -> String;
}

/// The key store of this machine for the Knode home `home`: a file store when
/// `KNODE_KEY_STORE=file`; else the Keychain on macOS; else the Secret Service through
/// `secret-tool` on Linux when it works, with files as the fallback.
pub fn default_key_store(home: &Path) -> Box<dyn KeyStore> {
    let files = FileKeyStore::new(home.join(KEYS_DIR));
    if std::env::var(KEY_STORE_VAR).is_ok_and(|v| v == "file") {
        return Box::new(files);
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(KeychainKeyStore::new(home))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(SecretToolKeyStore::new(home, files))
    }
}

/// The account a key is filed under in a system store: the agent's name and the Knode home, so
/// two Knode homes on one machine never share keys.
fn account(agent: &str, home: &Path) -> String {
    format!("{agent} ({})", home.display())
}

/// Keys in memory, for tests and embedding.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    keys: Mutex<std::collections::HashMap<String, String>>,
}

impl MemoryKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, agent: &str) -> io::Result<Option<String>> {
        Ok(lock(&self.keys).get(agent).cloned())
    }

    fn set(&self, agent: &str, key: &str) -> io::Result<()> {
        lock(&self.keys).insert(agent.to_string(), key.to_string());
        Ok(())
    }

    fn delete(&self, agent: &str) -> io::Result<bool> {
        Ok(lock(&self.keys).remove(agent).is_some())
    }

    fn describe(&self) -> String {
        "memory".into()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keys in files that only their owner can read: `<dir>/<agent>.key`, mode `0600`, in a
/// folder of mode `0700`.
#[derive(Debug, Clone)]
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    /// A store in `dir`, made when the first key is kept.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileKeyStore { dir: dir.into() }
    }

    fn file(&self, agent: &str) -> io::Result<PathBuf> {
        let plain = !agent.is_empty()
            && agent
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{agent:?} is not an agent name"),
            ));
        }
        Ok(self.dir.join(format!("{agent}.key")))
    }
}

impl KeyStore for FileKeyStore {
    fn get(&self, agent: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.file(agent)?) {
            Ok(text) => Ok(Some(text.trim().to_string()).filter(|k| !k.is_empty())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set(&self, agent: &str, key: &str) -> io::Result<()> {
        let file = self.file(agent)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        write_private(&file, key.as_bytes())
    }

    fn delete(&self, agent: &str) -> io::Result<bool> {
        match fs::remove_file(self.file(agent)?) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn describe(&self) -> String {
        format!("a file only you can read in {}", self.dir.display())
    }
}

/// Writes a file that only its owner can read or write, replacing it in one step.
pub(crate) fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = file.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = file.with_file_name(tmp_name);
    let _ = fs::remove_file(&tmp);
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    drop(out);
    fs::rename(&tmp, file)
}

/// The owner token of the Knode home `home`, made the first time: 64 random hexadecimal digits
/// in `owner.token`, mode `0600`. A client that presents it acts for the person who owns the
/// Knode home, not as an agent.
pub fn owner_token(home: &Path) -> io::Result<String> {
    let file = home.join(OWNER_TOKEN_FILE);
    if let Some(token) = read_owner_token(&file)? {
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        return Ok(token);
    }
    let token = hex(&random_bytes::<32>()?);
    write_private(&file, token.as_bytes())?;
    Ok(token)
}

/// The owner token in `file`, or `None` when there is none.
pub(crate) fn read_owner_token(file: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Keys in the macOS Keychain, as generic passwords of the service `md.knode.agent-key`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct KeychainKeyStore {
    home: PathBuf,
}

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    /// The Keychain store for the Knode home `home`.
    pub fn new(home: &Path) -> Self {
        KeychainKeyStore {
            home: home.to_path_buf(),
        }
    }
}

#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
impl KeyStore for KeychainKeyStore {
    fn get(&self, agent: &str) -> io::Result<Option<String>> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(STORE_SERVICE, &account(agent, &self.home)) {
            Ok(bytes) => Ok(String::from_utf8(bytes).ok().filter(|k| !k.is_empty())),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(io::Error::other(format!("the Keychain: {e}"))),
        }
    }

    fn set(&self, agent: &str, key: &str) -> io::Result<()> {
        use security_framework::passwords::set_generic_password;
        set_generic_password(STORE_SERVICE, &account(agent, &self.home), key.as_bytes())
            .map_err(|e| io::Error::other(format!("the Keychain: {e}")))
    }

    fn delete(&self, agent: &str) -> io::Result<bool> {
        use security_framework::passwords::delete_generic_password;
        match delete_generic_password(STORE_SERVICE, &account(agent, &self.home)) {
            Ok(()) => Ok(true),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            Err(e) => Err(io::Error::other(format!("the Keychain: {e}"))),
        }
    }

    fn describe(&self) -> String {
        "the Keychain".into()
    }
}

/// Keys in the Secret Service (GNOME Keyring, KWallet) through `secret-tool`, falling back to
/// files when it is missing or no Secret Service runs.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone)]
pub struct SecretToolKeyStore {
    home: PathBuf,
    files: FileKeyStore,
}

#[cfg(not(target_os = "macos"))]
impl SecretToolKeyStore {
    /// The store for the Knode home `home`, with `files` as the fallback.
    pub fn new(home: &Path, files: FileKeyStore) -> Self {
        SecretToolKeyStore {
            home: home.to_path_buf(),
            files,
        }
    }

    fn tool(&self, args: &[&str], input: Option<&str>) -> Option<std::process::Output> {
        use std::process::{Command, Stdio};
        let mut child = Command::new("secret-tool")
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes()).ok()?;
        }
        child.wait_with_output().ok()
    }

    fn attributes(&self, agent: &str) -> [String; 4] {
        [
            "service".into(),
            STORE_SERVICE.into(),
            "account".into(),
            account(agent, &self.home),
        ]
    }
}

#[cfg(not(target_os = "macos"))]
impl KeyStore for SecretToolKeyStore {
    fn get(&self, agent: &str) -> io::Result<Option<String>> {
        let attrs = self.attributes(agent);
        let mut args = vec!["lookup"];
        args.extend(attrs.iter().map(String::as_str));
        if let Some(out) = self.tool(&args, None).filter(|o| o.status.success()) {
            let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !key.is_empty() {
                return Ok(Some(key));
            }
        }
        self.files.get(agent)
    }

    fn set(&self, agent: &str, key: &str) -> io::Result<()> {
        let attrs = self.attributes(agent);
        let label = format!("Knode agent key for {agent}");
        let mut args = vec!["store", "--label", label.as_str()];
        args.extend(attrs.iter().map(String::as_str));
        if self
            .tool(&args, Some(key))
            .is_some_and(|o| o.status.success())
            && self.get(agent)?.as_deref() == Some(key)
        {
            self.files.delete(agent)?;
            return Ok(());
        }
        self.files.set(agent, key)
    }

    fn delete(&self, agent: &str) -> io::Result<bool> {
        let attrs = self.attributes(agent);
        let mut args = vec!["clear"];
        args.extend(attrs.iter().map(String::as_str));
        let cleared = self.tool(&args, None).is_some_and(|o| o.status.success());
        Ok(self.files.delete(agent)? || cleared)
    }

    fn describe(&self) -> String {
        "the Secret Service, or a file only you can read where it is missing".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_fingerprints_follow_the_spec() {
        let zero = format!("{KEY_PREFIX}{}", "0".repeat(64));
        assert!(is_key(&zero));
        assert_eq!(
            fingerprint(&zero),
            "sha256:2d8e7ee0d1fd2f7fa95d62fff7405c54a6c86833c447fe84c53f2de800dfb3a1"
        );
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        assert!(is_key(&a) && is_key(&b), "{a}");
        assert_ne!(a, b);
        assert!(!is_key("knk_123"));
        assert!(!is_key(&a.to_uppercase()));
    }

    #[test]
    fn the_file_store_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(dir.path().join(KEYS_DIR));
        assert_eq!(store.get("claude-code").unwrap(), None);
        store.set("claude-code", "knk_a").unwrap();
        assert_eq!(store.get("claude-code").unwrap().as_deref(), Some("knk_a"));
        let file = dir.path().join(KEYS_DIR).join("claude-code.key");
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let mode = fs::metadata(dir.path().join(KEYS_DIR))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(store.get("../x").is_err());
        assert!(store.delete("claude-code").unwrap());
        assert!(!store.delete("claude-code").unwrap());

        let memory = MemoryKeyStore::new();
        memory.set("codex", "knk_b").unwrap();
        assert_eq!(memory.get("codex").unwrap().as_deref(), Some("knk_b"));
    }

    #[test]
    fn the_owner_token_is_made_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let token = owner_token(dir.path()).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(owner_token(dir.path()).unwrap(), token);
        let mode = fs::metadata(dir.path().join(OWNER_TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

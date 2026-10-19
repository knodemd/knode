//! Where Knode keeps its files: the Knode home, the socket and the lock.

use std::env;
use std::path::PathBuf;

use crate::{Error, Result};

/// The socket file in the Knode home.
pub const SOCKET_FILE: &str = "service.sock";
/// The lock file in the Knode home. The running service holds an exclusive lock on it and
/// writes its process id in it.
pub const LOCK_FILE: &str = "service.lock";

/// The Knode home: `KNODE_HOME` when it is set, else `~/Library/Application Support/Knode` on
/// macOS, else `$XDG_DATA_HOME/knode` (by default `~/.local/share/knode`) on other systems.
pub fn knode_home() -> Result<PathBuf> {
    if let Some(home) = env::var_os("KNODE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    let home = user_home()?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support/Knode"));
    }
    let data = env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    Ok(data.join("knode"))
}

/// The socket of Knode Service in the default Knode home.
pub fn socket_path() -> Result<PathBuf> {
    Ok(knode_home()?.join(SOCKET_FILE))
}

pub(crate) fn user_home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or(Error::NoHome)
}

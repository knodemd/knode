//! Where Knode keeps its files: the Knode home, the socket and the lock. The Knode home comes
//! from `knode-config`, so the service, the CLI and the app always agree on it.

use std::path::PathBuf;

use crate::{Error, Result};

/// The socket file in the Knode home.
pub const SOCKET_FILE: &str = knode_config::SERVICE_SOCKET;
/// The lock file in the Knode home. The running service holds an exclusive lock on it and
/// writes its process id in it.
pub const LOCK_FILE: &str = "service.lock";

/// The Knode home: `KNODE_HOME` when it is set, else `~/Library/Application Support/Knode` on
/// macOS, else `$XDG_DATA_HOME/knode` (by default `~/.local/share/knode`) on other systems.
/// See [`knode_config::Paths`].
pub fn knode_home() -> Result<PathBuf> {
    Ok(paths()?.home)
}

/// The socket of Knode Service in the default Knode home.
pub fn socket_path() -> Result<PathBuf> {
    Ok(paths()?.service_socket())
}

pub(crate) fn paths() -> Result<knode_config::Paths> {
    knode_config::Paths::from_env().map_err(|_| Error::NoHome)
}

/// The home folder of the user, for the launch agent and the systemd unit.
pub(crate) fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or(Error::NoHome)
}

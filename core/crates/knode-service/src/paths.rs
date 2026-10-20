//! Where Knode keeps its files: the Knode home, the socket and the lock. The Knode home comes
//! from `knode-config`, so the service, the CLI and the app always agree on it.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The socket file in the Knode home.
pub const SOCKET_FILE: &str = knode_config::SERVICE_SOCKET;
/// The lock file in the Knode home. The running service holds an exclusive lock on it and
/// writes its process id in it.
pub const LOCK_FILE: &str = "service.lock";
/// The log in the Knode home where a service started in the background writes its messages.
pub const LOG_FILE: &str = "service.log";

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

/// The longest socket path the system allows, in bytes: `sun_path` holds it with a final NUL.
pub const SOCKET_PATH_MAX: usize = if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)) {
    103
} else {
    107
};

/// Fails with [`Error::SocketPathTooLong`] when `socket` is too long to listen on, as in a
/// deep `KNODE_HOME`.
pub fn check_socket_path(socket: &Path) -> Result<()> {
    let len = socket.as_os_str().as_bytes().len();
    if len > SOCKET_PATH_MAX {
        return Err(Error::SocketPathTooLong {
            path: socket.display().to_string(),
            len,
            max: SOCKET_PATH_MAX,
        });
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_path_up_to_the_limit_is_fine() {
        let path = PathBuf::from(format!("/{}", "a".repeat(SOCKET_PATH_MAX - 1)));
        assert!(check_socket_path(&path).is_ok());
    }

    #[test]
    fn a_longer_socket_path_says_why_it_cannot_work() {
        let path = PathBuf::from(format!("/{}", "a".repeat(SOCKET_PATH_MAX)));
        let error = check_socket_path(&path).unwrap_err();
        assert!(matches!(error, Error::SocketPathTooLong { len, max, .. }
            if len == SOCKET_PATH_MAX + 1 && max == SOCKET_PATH_MAX));
        assert!(
            error
                .to_string()
                .contains("set KNODE_HOME to a shorter folder")
        );
    }
}

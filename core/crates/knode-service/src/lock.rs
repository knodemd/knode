//! The single-instance lock: an exclusive `flock` on `service.lock` in the Knode home.
//!
//! The kernel releases the lock when the process ends, however it ends, so a lock left by a
//! crashed service never blocks the next one.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::{Error, Result};

/// Holds the lock until dropped.
#[derive(Debug)]
pub(crate) struct InstanceLock {
    file: File,
}

impl InstanceLock {
    /// Takes the lock at `path` without waiting, and writes this process id in it. Fails with
    /// [`Error::AlreadyRunning`] when another service holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        // SAFETY: flock only reads the descriptor, which stays open for the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                return Err(Error::AlreadyRunning {
                    pid: text.trim().parse().ok(),
                });
            }
            return Err(err.into());
        }
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(InstanceLock { file })
    }
}

/// Whether a service holds the lock at `path` now.
pub(crate) fn is_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    // SAFETY: flock only reads the descriptor, which stays open for both calls.
    unsafe {
        if libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            false
        } else {
            true
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        // SAFETY: as above; closing the file would release the lock anyway.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

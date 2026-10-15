//! KhonsuGraph's own writer lock for a graph directory.
//!
//! HawDB documents a single-writer lease per directory, but whether a second
//! process is refused depends on the platform. KhonsuGraph therefore takes an
//! OS file lock before opening a graph for writing, and records who holds it so
//! the "directory is in use" message can say by whom.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOCK_FILE: &str = "khonsugraph.lock";
pub const OWNER_FILE: &str = "khonsugraph.owner.json";

/// Who holds the writer lock, as recorded by the holder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockOwner {
    pub pid: u32,
    pub program: String,
    /// Seconds since the Unix epoch.
    pub since: u64,
}

impl LockOwner {
    fn current(program: &str) -> Self {
        Self {
            pid: std::process::id(),
            program: program.to_string(),
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock. The owner is `None` when it did not
    /// record itself (e.g. an older build or a crash between lock and write).
    Held(Option<LockOwner>),
    Io(std::io::Error),
}

/// Held for as long as the graph is open for writing; released on drop.
#[derive(Debug)]
pub struct WriterLock {
    file: File,
    owner_path: PathBuf,
}

impl WriterLock {
    pub fn acquire(dir: &Path, program: &str) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::Held(read_owner(dir))),
            Err(TryLockError::Error(err)) => return Err(LockError::Io(err)),
        }
        let owner_path = dir.join(OWNER_FILE);
        let owner = serde_json::to_vec_pretty(&LockOwner::current(program)).expect("owner serializes");
        fs::write(&owner_path, owner).map_err(LockError::Io)?;
        Ok(Self { file, owner_path })
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.owner_path);
        let _ = self.file.unlock();
    }
}

/// Reads the recorded owner of a directory's writer lock, if any.
pub fn read_owner(dir: &Path) -> Option<LockOwner> {
    let bytes = fs::read(dir.join(OWNER_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// True when some process currently holds the writer lock.
pub fn is_locked(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(dir.join(LOCK_FILE)) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_reports_the_owner_and_drop_releases() {
        let dir = tempfile::tempdir().unwrap();
        let first = WriterLock::acquire(dir.path(), "test-gui").unwrap();
        assert!(is_locked(dir.path()));
        match WriterLock::acquire(dir.path(), "test-cli") {
            Err(LockError::Held(Some(owner))) => {
                assert_eq!(owner.pid, std::process::id());
                assert_eq!(owner.program, "test-gui");
            }
            other => panic!("expected held lock, got {other:?}"),
        }
        drop(first);
        assert!(!is_locked(dir.path()));
        assert!(read_owner(dir.path()).is_none());
        WriterLock::acquire(dir.path(), "test-cli").unwrap();
    }
}

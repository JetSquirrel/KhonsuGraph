//! Small JSON files that more than one process changes: settings, sources,
//! approvals, history, a dashboard's comments. The window, a `khonsu` command
//! and an agent's command can all be at one of them in the same moment.
//!
//! Three rules keep what is in them:
//!
//! - **A file that can't be read is not an empty one.** [`read`] tells a
//!   missing file (`None`) from one that is unreadable or not valid JSON (an
//!   error). A change is never made on top of the second: it would write the
//!   defaults over everything the file held.
//! - **A write lands whole or not at all.** [`write`] writes a temporary file
//!   of its own beside the target, flushes it, and renames it into place, so a
//!   reader sees the old file or the new one, never part of either.
//! - **One change at a time.** [`update`] holds an OS file lock across its
//!   read, change and write, so two writers can't both start from the same
//!   file and lose one of the changes. The lock is the system's: it goes when
//!   its process does, however that ends.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a change waits for another process to finish its own.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// What `path` holds: `None` when there is no such file, or it is empty.
/// An error names the file when it can't be read or isn't the JSON expected.
pub fn read<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io::Error::new(error.kind(), format!("{}: {error}", path.display()))),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} can't be read ({error}), so nothing was changed. Fix the file, or move it aside to start again",
                path.display()
            ),
        )
    })
}

/// Replaces `path` with `value` as pretty JSON, whole or not at all.
pub fn write<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_bytes(path, &bytes)
}

/// Replaces `path` with `bytes`, whole or not at all.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    // A name no other writer has: this process, and this write within it.
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let temporary = path.with_file_name(name);
    let written = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Renames over `to`. On Windows that is refused for the moment another
/// process has `to` open, so it is tried again for a short while.
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match fs::rename(from, to) {
            Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied && started.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(15));
            }
            other => return other,
        }
    }
}

/// Held while one process changes a file; released on drop.
#[derive(Debug)]
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Where the lock for `path` lives: beside it, as `<name>.lock`. It is never
/// the file itself, which a write replaces, and it is left in place: taking a
/// lock file away while another process waits on it lets two in at once.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Waits for the right to change `path`, up to [`LOCK_WAIT`].
pub fn lock(path: &Path) -> io::Result<Lock> {
    let at = lock_path(path);
    if let Some(dir) = at.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&at)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => std::thread::sleep(Duration::from_millis(10)),
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} is being changed by another process; try again", path.display()),
                ))
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Reads `path` (the default when there is no such file), applies `change`,
/// and writes the result, with the file locked throughout. Nothing is written
/// when the file can't be read or `change` fails.
pub fn update<T, R, E>(path: &Path, change: impl FnOnce(&mut T) -> Result<R, E>) -> Result<(R, T), E>
where
    T: DeserializeOwned + Serialize + Default,
    E: From<io::Error>,
{
    let _lock = lock(path)?;
    let mut value: T = read(path)?.unwrap_or_default();
    let out = change(&mut value)?;
    write(path, &value)?;
    Ok((out, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type Counts = BTreeMap<String, u64>;

    #[test]
    fn a_missing_or_empty_file_is_nothing_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.json");
        assert_eq!(read::<Counts>(&path).unwrap(), None);
        fs::write(&path, " \n").unwrap();
        assert_eq!(read::<Counts>(&path).unwrap(), None);
        fs::write(&path, "{\"a\": 1").unwrap();
        let error = read::<Counts>(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("counts.json") && error.to_string().contains("nothing was changed"), "{error}");
    }

    #[test]
    fn a_change_is_refused_on_a_file_that_cannot_be_read_and_the_file_is_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.json");
        let broken = "{\"kept\": 7, oops";
        fs::write(&path, broken).unwrap();
        let result: Result<((), Counts), io::Error> = update(&path, |counts: &mut Counts| {
            counts.insert("new".into(), 1);
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        // A change that fails writes nothing either.
        fs::write(&path, "{\"kept\": 7}").unwrap();
        let result: Result<((), Counts), io::Error> = update(&path, |counts: &mut Counts| {
            counts.clear();
            Err(io::Error::other("no"))
        });
        assert!(result.is_err());
        assert_eq!(read::<Counts>(&path).unwrap().unwrap()["kept"], 7);
    }

    #[test]
    fn a_write_leaves_no_temporary_file_and_replaces_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("counts.json");
        write(&path, &Counts::from([("a".to_string(), 1)])).unwrap();
        write(&path, &Counts::from([("b".to_string(), 2)])).unwrap();
        assert_eq!(read::<Counts>(&path).unwrap().unwrap(), Counts::from([("b".to_string(), 2)]));
        let names: Vec<String> = fs::read_dir(path.parent().unwrap()).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["counts.json"]);
    }

    /// Eight writers, each adding its own keys by read-modify-write, while a
    /// reader keeps reading: no change is lost and no read sees half a file.
    #[test]
    fn concurrent_writers_lose_nothing_and_a_reader_never_sees_part_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.json");
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut reads = 0;
                while !done.load(Ordering::Relaxed) {
                    read::<Counts>(&path).expect("a whole file or none");
                    reads += 1;
                }
                reads
            });
            let writers: Vec<_> = (0..8)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        for step in 0..25 {
                            let _: ((), Counts) = update(path, |counts: &mut Counts| {
                                counts.insert(format!("{writer}-{step}"), step);
                                *counts.entry("total".into()).or_default() += 1;
                                Ok::<_, io::Error>(())
                            })
                            .unwrap();
                        }
                    })
                })
                .collect();
            writers.into_iter().for_each(|writer| writer.join().unwrap());
            done.store(true, Ordering::Relaxed);
            assert!(reader.join().unwrap() > 0);
        });
        let counts = read::<Counts>(&path).unwrap().unwrap();
        assert_eq!(counts["total"], 200);
        assert_eq!(counts.len(), 201);
    }
}

//! Data-source plugins: discovery, running one plugin operation in its own
//! process, and syncing a configured source into a graph.
//!
//! Built-in and third-party plugins go through the same path: a directory with
//! a `khonsu-plugin.json` manifest and an executable speaking
//! [`khonsu_plugin_protocol`]. Built-in plugins are looked up next to the
//! running executable (`plugins/<id>/` or a bare `khonsu-plugin-<id>` binary).

use crate::engine::{Graph, QueryError};
use crate::merge::{next_sync_stamp, EdgePattern, MergeStats, Merger};
use crate::settings::{self, LastSync, RunStatus, Source, SyncRun};
use crate::{secrets, util};
use khonsu_plugin_protocol::{
    parse_line, Describe, EdgeRecord, Manifest, Message, NodeRecord, Permissions, Request, SyncMode, MANIFEST_FILE,
    PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// The longest line a plugin may send. A longer one fails the sync rather than
/// filling memory.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// What is kept of one stderr line, for error messages.
const MAX_STDERR_LINE: usize = 4096;
/// The most `describe` or `test` may print.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// How long `describe` may take: it says what a plugin is, and needs no network.
const DESCRIBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a connection test may take: long enough for a slow login.
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// How long an `analyze` may take: it works on what it was sent, and needs no network.
pub const ANALYZE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// The most findings an `analyze` may send. Past this it fails rather than filling memory.
const MAX_FINDINGS: usize = 100_000;
/// The most an archive may unpack to, and how many entries it may hold.
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;

/// IDs of plugins shipped with KhonsuGraph.
pub const BUILTIN_PLUGINS: &[&str] = &["csv", "aws", "neo4j"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginInfo {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub builtin: bool,
}

impl PluginInfo {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The entry executable: a file in the plugin directory, or, for a bare
    /// command name such as `python3` or `node`, a program on `PATH`.
    pub fn executable(&self) -> PathBuf {
        let entry = &self.manifest.entry[0];
        let mut exe = self.dir.join(entry);
        if cfg!(windows) && exe.extension().is_none() {
            exe.set_extension("exe");
        }
        let bare = !entry.contains(['/', '\\']);
        if !exe.is_file() && bare {
            if let Some(found) = on_path(entry) {
                return found;
            }
        }
        exe
    }

    /// The one place a plugin's process is made, for every operation, so it is
    /// the one place that refuses a plugin not approved as it is now: nothing
    /// of it runs, and it is sent no configuration or secrets.
    fn command(&self) -> Result<Command, PluginError> {
        require_approved(self)?;
        let mut command = Command::new(self.executable());
        command.args(&self.manifest.entry[1..]).current_dir(&self.dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        Ok(command)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {0} is not installed")]
    NotFound(String),
    #[error("invalid plugin at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("plugin {plugin} failed: {message}")]
    Failed { plugin: String, message: String },
    #[error("sync cancelled")]
    Cancelled,
    #[error("plugin {plugin} needs approval before it runs: {reason}")]
    NotApproved { plugin: String, permissions: Box<Permissions>, reason: String },
    #[error("{0}")]
    Query(#[from] QueryError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Directories searched for built-in plugins, nearest first.
fn builtin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(paths) = std::env::var_os("KHONSU_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    // By its real path too: called through the `khonsu` link in
    // /usr/local/bin, the executable's own folder is where the plugins are.
    let exes = std::env::current_exe().into_iter().flat_map(|exe| {
        let real = exe.canonicalize().ok().filter(|real| *real != exe);
        std::iter::once(exe).chain(real)
    });
    for exe in exes {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            // Test binaries live in target/<profile>/deps.
            if dir.ends_with("deps") {
                if let Some(parent) = dir.parent() {
                    dirs.push(parent.to_path_buf());
                }
            }
            // macOS bundles keep extra binaries in Contents/Resources.
            dirs.push(dir.join("../Resources"));
        }
    }
    dirs
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, PluginError> {
    let path = dir.join(MANIFEST_FILE);
    let bytes = fs::read(&path).map_err(|e| PluginError::Invalid {
        path: path.clone(),
        message: e.to_string(),
    })?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| PluginError::Invalid {
        path: path.clone(),
        message: e.to_string(),
    })?;
    if manifest.entry.is_empty() {
        return Err(PluginError::Invalid {
            path,
            message: "entry is empty".into(),
        });
    }
    if manifest.protocol != PROTOCOL_VERSION {
        return Err(PluginError::Invalid {
            path,
            message: format!("protocol {} is not supported (expected {PROTOCOL_VERSION})", manifest.protocol),
        });
    }
    if !is_valid_id(&manifest.id) {
        return Err(PluginError::Invalid {
            path,
            message: "id must be letters, digits, '-' or '_'".into(),
        });
    }
    if let Some(bad) = manifest.entry.iter().find(|part| leaves_the_folder(part)) {
        return Err(PluginError::Invalid {
            path,
            message: format!("entry {bad:?} points outside the plugin folder"),
        });
    }
    Ok(manifest)
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// An absolute path, a drive, or a `..` part, on any platform: a manifest is
/// written on one machine and read on another.
fn leaves_the_folder(part: &str) -> bool {
    part.starts_with(['/', '\\']) || part.contains(':') || part.split(['/', '\\']).any(|piece| piece == "..")
}

/// Whether a plugin may run: built in, or approved as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Approved,
    /// Never approved.
    NotApproved,
    /// Approved, and its version, permissions or files are different now.
    Changed,
}

/// A hash of everything in a plugin's folder, so that a change to its files
/// needs approval again even under the same version number. It says nothing
/// about a program it finds on `PATH`, such as `python3`.
pub fn identity(plugin: &PluginInfo) -> Result<String, PluginError> {
    let mut hash = Sha256::new();
    hash_folder(&plugin.dir, "", &mut hash)?;
    let digest = hash.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash_folder(dir: &Path, prefix: &str, hash: &mut Sha256) -> Result<(), PluginError> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        // What an interpreter writes for itself as it runs.
        if name == "__pycache__" || name.ends_with(".pyc") {
            continue;
        }
        hash.update(relative.as_bytes());
        hash.update([0]);
        if kind.is_dir() {
            hash_folder(&entry.path(), &format!("{relative}/"), hash)?;
        } else if kind.is_symlink() {
            // Installing refuses these, so one here was added since.
            hash.update(b"symlink");
            hash.update(fs::read_link(entry.path())?.to_string_lossy().as_bytes());
        } else {
            std::io::copy(&mut fs::File::open(entry.path())?, hash)?;
        }
        hash.update([0]);
    }
    Ok(())
}

pub fn approval_state(plugin: &PluginInfo) -> ApprovalState {
    if plugin.builtin {
        return ApprovalState::Approved;
    }
    let approvals = settings::load_approvals();
    let Some(approval) = approvals.iter().find(|approval| approval.plugin == plugin.id()) else {
        return ApprovalState::NotApproved;
    };
    let same = approval.version == plugin.manifest.version
        && approval.permissions == plugin.manifest.permissions
        && approval.identity.is_some()
        && approval.identity == identity(plugin).ok();
    if same {
        ApprovalState::Approved
    } else {
        ApprovalState::Changed
    }
}

/// Approves a plugin as it is now.
pub fn approve(plugin: &PluginInfo) -> Result<(), PluginError> {
    settings::approve(plugin.id(), &plugin.manifest.version, &plugin.manifest.permissions, &identity(plugin)?)?;
    Ok(())
}

/// Looks a bare command name up on `PATH`, trying `PATHEXT` extensions on Windows.
fn on_path(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) && Path::new(name).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        vec![String::new()]
    };
    // Windows' WindowsApps folder holds Store install stubs (python3.exe
    // that only prints a hint), not the programs themselves.
    let stub = |dir: &Path| dir.ends_with("Microsoft/WindowsApps");
    std::env::split_paths(&std::env::var_os("PATH")?).filter(|dir| !stub(dir)).find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|candidate| candidate.is_file())
    })
}

fn find_builtin(id: &str) -> Option<PluginInfo> {
    for base in builtin_dirs() {
        let dir = base.join("plugins").join(id);
        if let Ok(manifest) = read_manifest(&dir) {
            return Some(PluginInfo {
                manifest,
                dir,
                builtin: true,
            });
        }
        let exe_name = format!("khonsu-plugin-{id}{}", std::env::consts::EXE_SUFFIX);
        if base.join(&exe_name).is_file() {
            let probe = PluginInfo {
                manifest: Manifest {
                    id: id.to_string(),
                    name: id.to_string(),
                    version: String::new(),
                    protocol: PROTOCOL_VERSION,
                    entry: vec![exe_name],
                    description: String::new(),
                    permissions: Permissions::default(),
                },
                dir: base.clone(),
                builtin: true,
            };
            // A bare binary describes itself.
            if let Ok(describe) = describe(&probe) {
                let mut manifest = probe.manifest;
                manifest.name = describe.name;
                manifest.version = describe.version;
                manifest.description = describe.description;
                manifest.permissions = describe.permissions;
                return Some(PluginInfo {
                    manifest,
                    dir: base,
                    builtin: true,
                });
            }
        }
    }
    None
}

/// Every plugin available: built-in first, then user-installed ones.
pub fn list() -> Vec<PluginInfo> {
    let mut plugins: Vec<PluginInfo> = BUILTIN_PLUGINS.iter().filter_map(|id| find_builtin(id)).collect();
    if let Ok(entries) = fs::read_dir(settings::plugins_dir()) {
        let mut installed: Vec<PluginInfo> = entries
            .flatten()
            .filter_map(|e| {
                let dir = e.path();
                read_manifest(&dir).ok().map(|manifest| PluginInfo {
                    manifest,
                    dir,
                    builtin: false,
                })
            })
            .filter(|p| !plugins.iter().any(|b| b.id() == p.id()))
            .collect();
        installed.sort_by(|a, b| a.id().cmp(b.id()));
        plugins.extend(installed);
    }
    plugins
}

pub fn find(id: &str) -> Result<PluginInfo, PluginError> {
    list()
        .into_iter()
        .find(|p| p.id() == id)
        .ok_or_else(|| PluginError::NotFound(id.to_string()))
}

/// Installs a plugin from a local directory, or a `.zip` of one, by copying
/// it into the user plugin directory. Returns the installed plugin.
pub fn install(from: &Path) -> Result<PluginInfo, PluginError> {
    if from.is_file() {
        return install_archive(from);
    }
    let manifest = read_manifest(from)?;
    if BUILTIN_PLUGINS.contains(&manifest.id.as_str()) {
        return Err(PluginError::Other(format!("{} is a built-in plugin ID", manifest.id)));
    }
    // Checked in a staging folder, so that a bad plugin leaves the one installed
    // before it in place.
    let plugins = settings::plugins_dir();
    fs::create_dir_all(&plugins)?;
    let staging = plugins.join(format!(".install-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        copy_dir(from, &staging)?;
        let staged = PluginInfo {
            manifest: manifest.clone(),
            dir: staging.clone(),
            builtin: false,
        };
        if !staged.executable().is_file() {
            return Err(PluginError::Invalid {
                path: staged.executable(),
                message: "entry executable not found".into(),
            });
        }
        let dest = plugins.join(&manifest.id);
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
        }
        fs::rename(&staging, &dest)?;
        Ok(PluginInfo {
            manifest,
            dir: dest,
            builtin: false,
        })
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn uninstall(id: &str) -> Result<(), PluginError> {
    if !is_valid_id(id) {
        return Err(PluginError::NotFound(id.to_string()));
    }
    let dir = settings::plugins_dir().join(id);
    if !dir.exists() {
        return Err(PluginError::NotFound(id.to_string()));
    }
    fs::remove_dir_all(dir)?;
    Ok(())
}

/// Unpacks a `.zip` holding a plugin folder (the manifest at its top, or in
/// its one top-level folder) and installs that folder.
fn install_archive(archive: &Path) -> Result<PluginInfo, PluginError> {
    let invalid = |message: String| PluginError::Invalid {
        path: archive.to_path_buf(),
        message,
    };
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?).map_err(|e| invalid(format!("not a zip archive: {e}")))?;
    let staging = settings::plugins_dir().join(format!(".unpack-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let result = (|| {
        unpack(&mut zip, &staging, MAX_ARCHIVE_BYTES, MAX_ARCHIVE_ENTRIES).map_err(|message| match message {
            UnpackError::Refused(message) => invalid(message),
            UnpackError::Io(error) => PluginError::Io(error),
        })?;
        let root = if staging.join(MANIFEST_FILE).is_file() {
            staging.clone()
        } else {
            let folders: Vec<PathBuf> = fs::read_dir(&staging)?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            match folders.as_slice() {
                [only] if only.join(MANIFEST_FILE).is_file() => only.clone(),
                _ => return Err(invalid(format!("no {MANIFEST_FILE} at the top of the archive or in its one folder"))),
            }
        };
        install(&root)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

enum UnpackError {
    Refused(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for UnpackError {
    fn from(error: std::io::Error) -> Self {
        UnpackError::Io(error)
    }
}

/// Unpacks an archive into `staging`, refusing entries that would land outside
/// it, symbolic links, and an archive larger than it says or than the caps.
fn unpack(zip: &mut zip::ZipArchive<fs::File>, staging: &Path, max_bytes: u64, max_entries: usize) -> Result<(), UnpackError> {
    if zip.len() > max_entries {
        return Err(UnpackError::Refused(format!("it holds {} entries, and at most {max_entries} are allowed", zip.len())));
    }
    let mut left = max_bytes;
    {
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| UnpackError::Refused(e.to_string()))?;
            // Entries that would land outside the folder (`../`, absolute) are refused.
            let Some(name) = entry.enclosed_name() else {
                return Err(UnpackError::Refused(format!("unsafe path in archive: {}", entry.name())));
            };
            if entry.is_symlink() {
                return Err(UnpackError::Refused(format!("symbolic link in archive: {}", entry.name())));
            }
            let target = staging.join(name);
            if entry.is_dir() {
                fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // The declared size is the archive's word for it, so count what comes out.
            let mut limited = (&mut entry).take(left + 1);
            let written = std::io::copy(&mut limited, &mut fs::File::create(&target)?)?;
            if written > left {
                return Err(UnpackError::Refused(format!("it unpacks to more than {max_bytes} bytes")));
            }
            left -= written;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o755))?;
            }
        }
    }
    Ok(())
}

/// Copies a plugin folder. A symbolic link is refused: following it would copy
/// in whatever it points at.
fn copy_dir(from: &Path, to: &Path) -> Result<(), PluginError> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(PluginError::Invalid {
                path: entry.path(),
                message: "a plugin folder may not hold symbolic links".into(),
            });
        }
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Reads one line of at most `max` bytes without holding more than that.
/// `Ok(None)` at the end of the stream. `Ok(Some(Err(())))` when the line was
/// longer, with the rest skipped if `truncate` and otherwise left unread.
#[allow(clippy::type_complexity)]
fn read_line_capped(reader: &mut impl BufRead, max: usize, truncate: bool) -> std::io::Result<Option<Result<String, ()>>> {
    let mut line = Vec::new();
    let mut read_any = false;
    let mut too_long = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        read_any = true;
        let (take, found) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (at, true),
            None => (available.len(), false),
        };
        let room = max.saturating_sub(line.len());
        line.extend_from_slice(&available[..take.min(room)]);
        too_long |= take > room;
        reader.consume(take + usize::from(found));
        if found {
            break;
        }
        if too_long && !truncate {
            return Ok(Some(Err(())));
        }
    }
    if !read_any {
        return Ok(None);
    }
    if too_long && !truncate {
        return Ok(Some(Err(())));
    }
    while line.last() == Some(&b'\r') {
        line.pop();
    }
    // Not valid UTF-8 is a mistake to report, not a reason to stop reading.
    Ok(Some(Ok(String::from_utf8_lossy(&line).into_owned())))
}

/// Collects the last lines a plugin wrote to stderr, for error messages.
fn drain_stderr(child: &mut Child) -> Arc<Mutex<VecDeque<String>>> {
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = Arc::clone(&tail);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Ok(Some(line)) = read_line_capped(&mut reader, MAX_STDERR_LINE, true) {
                let mut tail = tail.lock().unwrap();
                tail.push_back(line.unwrap_or_default());
                if tail.len() > 20 {
                    tail.pop_front();
                }
            }
        });
    }
    tail
}

fn failure(plugin: &PluginInfo, message: String, stderr: &Mutex<VecDeque<String>>) -> PluginError {
    // Plugins often print the same error to stderr; show it once.
    let tail: Vec<String> = stderr.lock().unwrap().iter().filter(|line| line.trim() != message.trim()).cloned().collect();
    let message = if tail.is_empty() {
        message
    } else {
        format!("{message}\n{}", tail.join("\n"))
    };
    PluginError::Failed {
        plugin: plugin.id().to_string(),
        message,
    }
}

/// What a plugin says about itself. This runs it, so it needs approval like
/// any other operation: what an unapproved plugin declares is in its manifest.
pub fn describe(plugin: &PluginInfo) -> Result<Describe, PluginError> {
    describe_within(plugin, DESCRIBE_TIMEOUT)
}

fn describe_within(plugin: &PluginInfo, timeout: std::time::Duration) -> Result<Describe, PluginError> {
    let failed = |message: String| PluginError::Failed {
        plugin: plugin.id().to_string(),
        message,
    };
    let mut child = plugin
        .command()?
        .arg("describe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(format!("cannot start {}: {e}", plugin.executable().display())))?;
    let stderr = drain_stderr(&mut child);
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut text = Vec::new();
        let read = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut text);
        let _ = tx.send(read.map(|_| text));
    });
    let Ok(read) = rx.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failed(format!("describe took more than {} seconds", timeout.as_secs())));
    };
    // Past the cap the reader has stopped, and nothing else will end the plugin.
    let _ = child.kill();
    let _ = child.wait();
    let text = read?;
    for line in String::from_utf8_lossy(&text).lines() {
        if let Some(Ok(Message::Describe(describe))) = parse_line(line) {
            return Ok(describe);
        }
    }
    Err(failure(plugin, "no describe message".into(), &stderr))
}

fn spawn(plugin: &PluginInfo, operation: &str, request: &Request) -> Result<Child, PluginError> {
    let mut child = plugin
        .command()?
        .arg(operation)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PluginError::Failed {
            plugin: plugin.id().to_string(),
            message: format!("cannot start {}: {e}", plugin.executable().display()),
        })?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    serde_json::to_writer(&mut stdin, request).map_err(std::io::Error::other)?;
    stdin.write_all(b"\n")?;
    drop(stdin);
    Ok(child)
}

pub fn test(plugin: &PluginInfo, request: &Request) -> Result<(bool, String), PluginError> {
    test_within(plugin, request, TEST_TIMEOUT)
}

fn test_within(plugin: &PluginInfo, request: &Request, timeout: std::time::Duration) -> Result<(bool, String), PluginError> {
    let mut child = spawn(plugin, "test", request)?;
    let stderr = drain_stderr(&mut child);
    let out = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut text = Vec::new();
        let read = out.take(MAX_OUTPUT_BYTES).read_to_end(&mut text);
        let _ = tx.send(read.map(|_| text));
    });
    // A plugin that never answers, or keeps its output open after it has, is
    // ended rather than waited on.
    let read = rx.recv_timeout(timeout);
    let _ = child.kill();
    let _ = child.wait();
    let Ok(read) = read else {
        return Err(failure(plugin, format!("test took more than {} seconds", timeout.as_secs()), &stderr));
    };
    let stdout = read?;
    for line in String::from_utf8_lossy(&stdout).lines() {
        match parse_line(line) {
            Some(Ok(Message::TestResult { ok, message })) => return Ok((ok, scrub(&message, &request.secrets))),
            Some(Ok(Message::Error { message })) => return Ok((false, scrub(&message, &request.secrets))),
            _ => {}
        }
    }
    Err(scrub_error(failure(plugin, "no test result".into(), &stderr), &request.secrets))
}

/// What happens during a sync, for progress display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncEvent {
    Progress {
        message: String,
        current: Option<u64>,
        total: Option<u64>,
    },
    Log {
        level: String,
        message: String,
    },
    Counts {
        nodes: u64,
        edges: u64,
    },
}

/// Runs `sync` and hands each record to `on_record`. Terminates the plugin
/// when `cancel` is set. Returns the reported cursor, if any.
/// How a plugin finished a sync it did finish.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncEnd {
    pub cursor: Option<serde_json::Value>,
    /// False when the plugin said part of the source was not enumerated.
    pub complete: bool,
    pub gaps: Vec<String>,
}

pub fn run_sync(
    plugin: &PluginInfo,
    request: &Request,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(SyncEvent),
    mut on_record: impl FnMut(Record) -> Result<(), PluginError>,
) -> Result<SyncEnd, PluginError> {
    let mut child = spawn(plugin, "sync", request)?;
    let stderr = drain_stderr(&mut child);
    let stdout = child.stdout.take().expect("stdout is piped");

    // Read on a thread so cancellation does not wait for the next line. The queue
    // is short because each line in it can be as long as the cap.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<String, ()>>(64);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(line)) = read_line_capped(&mut reader, MAX_LINE_BYTES, false) {
            let too_long = line.is_err();
            if tx.send(line).is_err() || too_long {
                break;
            }
        }
    });

    let mut cursor = None;
    let mut done = false;
    let (mut complete, mut gaps) = (true, Vec::new());
    let (mut nodes, mut edges) = (0u64, 0u64);
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err(PluginError::Cancelled);
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(Ok(line)) => line,
            Ok(Err(())) => {
                let _ = child.kill();
                break Err(failure(plugin, format!("a line of output was longer than {MAX_LINE_BYTES} bytes"), &stderr));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        };
        let message = match parse_line(&line) {
            None => continue,
            Some(Ok(message)) => message,
            Some(Err(err)) => {
                let _ = child.kill();
                let shown: String = line.chars().take(200).collect();
                break Err(failure(plugin, format!("invalid output line: {err}: {shown}"), &stderr));
            }
        };
        let step = match message {
            Message::Node(node) => {
                nodes += 1;
                on_record(Record::Node(node))
            }
            Message::Edge(edge) => {
                edges += 1;
                on_record(Record::Edge(edge))
            }
            Message::Progress { message, current, total } => {
                on_event(SyncEvent::Progress { message, current, total });
                Ok(())
            }
            Message::Log { level, message } => {
                on_event(SyncEvent::Log { level, message });
                Ok(())
            }
            Message::Cursor { value } => {
                cursor = Some(value);
                Ok(())
            }
            Message::Done { complete: whole, gaps: missing, .. } => {
                done = true;
                complete = whole;
                gaps = missing;
                Ok(())
            }
            Message::Error { message } => {
                let _ = child.kill();
                break Err(failure(plugin, message, &stderr));
            }
            Message::Describe(_) | Message::TestResult { .. } | Message::Finding(_) | Message::AnalysisDone { .. } => Ok(()),
        };
        if let Err(err) = step {
            let _ = child.kill();
            break Err(err);
        }
        if (nodes + edges) % 500 == 0 && nodes + edges > 0 {
            on_event(SyncEvent::Counts { nodes, edges });
        }
    };
    let status = child.wait()?;
    result?;
    if !done {
        return Err(failure(
            plugin,
            format!("plugin exited ({status}) without reporting done; records already written are kept and the next sync reconciles them"),
            &stderr,
        ));
    }
    on_event(SyncEvent::Counts { nodes, edges });
    Ok(SyncEnd { cursor, complete, gaps })
}

#[derive(Debug, Clone)]
pub enum Record {
    Node(NodeRecord),
    Edge(EdgeRecord),
}

/// Counts and samples of what a sync would write, without writing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub nodes: u64,
    pub edges: u64,
    pub labels: BTreeMap<String, u64>,
    pub relationship_types: BTreeMap<String, u64>,
    pub sample_nodes: Vec<NodeRecord>,
    pub sample_edges: Vec<EdgeRecord>,
}

const SAMPLES: usize = 8;

pub fn preview(plugin: &PluginInfo, request: &Request, cancel: &AtomicBool, on_event: impl FnMut(SyncEvent)) -> Result<Preview, PluginError> {
    let mut preview = Preview::default();
    let secrets = request.secrets.clone();
    let mut on_event = on_event;
    let scrubbed_events = |event: SyncEvent| on_event(scrub_event(event, &secrets));
    run_sync(plugin, request, cancel, scrubbed_events, |record| {
        match record {
            Record::Node(node) => {
                preview.nodes += 1;
                *preview.labels.entry(node.label.clone()).or_default() += 1;
                if preview.sample_nodes.len() < SAMPLES {
                    preview.sample_nodes.push(node);
                }
            }
            Record::Edge(edge) => {
                preview.edges += 1;
                *preview.relationship_types.entry(edge.rel_type.clone()).or_default() += 1;
                if preview.sample_edges.len() < SAMPLES {
                    preview.sample_edges.push(edge);
                }
            }
        }
        Ok(())
    })
    .map_err(|error| scrub_error(error, &request.secrets))?;
    Ok(preview)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncReport {
    pub source: String,
    pub mode: SyncMode,
    pub received_nodes: u64,
    pub received_edges: u64,
    pub merge: MergeStats,
    pub elapsed_ms: u64,
    /// This sync as kept on the source. `status` says whether it was complete.
    pub run: SyncRun,
}

/// Builds the request a source sends to its plugin, secrets included.
pub fn request_for(source: &Source, mode: SyncMode) -> Result<Request, PluginError> {
    Ok(Request {
        protocol: PROTOCOL_VERSION,
        config: source.config.clone(),
        secrets: secrets::load_all(&source.id, &source.secret_keys).map_err(PluginError::Other)?,
        mode,
        cursor: match mode {
            SyncMode::Incremental => source.cursor.clone(),
            SyncMode::Full => None,
        },
    })
}

/// Refuses a plugin that has not been approved as it is now.
pub fn require_approved(plugin: &PluginInfo) -> Result<(), PluginError> {
    match approval_state(plugin) {
        ApprovalState::Approved => Ok(()),
        state => {
            let reason = if state == ApprovalState::Changed {
                "its version, permissions or files have changed since it was approved"
            } else {
                "it has not been approved"
            };
            Err(PluginError::NotApproved {
                plugin: plugin.id().to_string(),
                permissions: Box::new(plugin.manifest.permissions.clone()),
                reason: reason.into(),
            })
        }
    }
}

/// How an `analyze` ended.
pub(crate) struct AnalysisEnd {
    pub findings: Vec<khonsu_plugin_protocol::FindingRecord>,
    pub subjects: u64,
}

/// Runs `analyze`, sending `request` on a thread so that a plugin that writes
/// before it reads cannot wedge the host. The plugin is ended when `cancel` is
/// set, after `timeout`, or when its output passes the limits a sync has.
pub(crate) fn run_analyze(
    plugin: &PluginInfo,
    request: &khonsu_plugin_protocol::AnalyzeRequest,
    cancel: &AtomicBool,
    timeout: std::time::Duration,
    mut on_event: impl FnMut(SyncEvent),
) -> Result<AnalysisEnd, PluginError> {
    let mut line = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut child = plugin
        .command()?
        .arg("analyze")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PluginError::Failed {
            plugin: plugin.id().to_string(),
            message: format!("cannot start {}: {e}", plugin.executable().display()),
        })?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    thread::spawn(move || {
        // A plugin that exits without reading is reported by what follows.
        let _ = stdin.write_all(&line);
    });
    let stderr = drain_stderr(&mut child);
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<String, ()>>(64);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(line)) = read_line_capped(&mut reader, MAX_LINE_BYTES, false) {
            let too_long = line.is_err();
            if tx.send(line).is_err() || too_long {
                break;
            }
        }
    });

    let started = std::time::Instant::now();
    let mut findings = Vec::new();
    let mut subjects = None;
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err(PluginError::Cancelled);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            break Err(failure(plugin, format!("the analysis took more than {} seconds", timeout.as_secs()), &stderr));
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(Ok(line)) => line,
            Ok(Err(())) => {
                let _ = child.kill();
                break Err(failure(plugin, format!("a line of output was longer than {MAX_LINE_BYTES} bytes"), &stderr));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        };
        match parse_line(&line) {
            None => {}
            Some(Err(err)) => {
                let _ = child.kill();
                let shown: String = line.chars().take(200).collect();
                break Err(failure(plugin, format!("invalid output line: {err}: {shown}"), &stderr));
            }
            Some(Ok(Message::Finding(_))) if findings.len() >= MAX_FINDINGS => {
                let _ = child.kill();
                break Err(failure(plugin, format!("it sent more than {MAX_FINDINGS} findings"), &stderr));
            }
            Some(Ok(Message::Finding(finding))) => findings.push(*finding),
            Some(Ok(Message::AnalysisDone { subjects: seen })) => subjects = Some(seen),
            Some(Ok(Message::Progress { message, current, total })) => on_event(SyncEvent::Progress { message, current, total }),
            Some(Ok(Message::Log { level, message })) => on_event(SyncEvent::Log { level, message }),
            Some(Ok(Message::Error { message })) => {
                let _ = child.kill();
                break Err(failure(plugin, message, &stderr));
            }
            Some(Ok(_)) => {}
        }
    };
    let status = child.wait()?;
    result?;
    let Some(subjects) = subjects else {
        return Err(failure(plugin, format!("plugin exited ({status}) without finishing the analysis"), &stderr));
    };
    Ok(AnalysisEnd { findings, subjects })
}

/// Syncs a configured source into `graph`, then records the outcome in the
/// source's settings. The plugin's declared permissions must be approved.
pub fn sync_source(
    graph: &mut Graph,
    source: &Source,
    mode: SyncMode,
    cancel: &AtomicBool,
    on_event: impl FnMut(SyncEvent),
) -> Result<SyncReport, PluginError> {
    sync_source_with(graph, source, mode, false, cancel, on_event)
}

/// [`sync_source`], and with `append` nothing the source no longer has is
/// removed: what it sends is added to or updates what it sent before. For a
/// source whose files hold only what is new since the last sync.
pub fn sync_source_with(
    graph: &mut Graph,
    source: &Source,
    mode: SyncMode,
    append: bool,
    cancel: &AtomicBool,
    on_event: impl FnMut(SyncEvent),
) -> Result<SyncReport, PluginError> {
    let plugin = find(&source.plugin)?;
    // Before anything of it runs, `describe` included.
    require_approved(&plugin)?;
    let describe = describe(&plugin)?;
    let mode = if mode == SyncMode::Incremental && !describe.supports_incremental {
        SyncMode::Full
    } else {
        mode
    };
    let request = request_for(source, mode)?;
    let started = std::time::Instant::now();
    let synced_at = next_sync_stamp(graph, &source.id, util::unix_now() as i64)?;
    let plugin_tag = format!("{}@{}", plugin.id(), describe.version);
    let mut merger = Merger::new(graph, &source.id, &plugin_tag, synced_at);
    let (mut received_nodes, mut received_edges) = (0, 0);
    let secret_values = request.secrets.clone();
    let mut on_event = on_event;
    let scrubbed_events = |event: SyncEvent| on_event(scrub_event(event, &secret_values));
    let outcome = run_sync(&plugin, &request, cancel, scrubbed_events, |record| {
        match record {
            Record::Node(node) => {
                received_nodes += 1;
                merger.add_node(node)?;
            }
            Record::Edge(edge) => {
                received_edges += 1;
                merger.add_edge(edge);
            }
        }
        Ok(())
    })
    .and_then(|end| {
        // Only a source enumerated whole can say that something has gone.
        let (merge, patterns) = merger.finish(!append && mode == SyncMode::Full && end.complete, &source.patterns)?;
        Ok((end, merge, patterns))
    });

    let number = source.last_run().map_or(1, |run| run.number + 1);
    let run = |status: RunStatus, removals_applied: bool, cursor_advanced: bool, gaps: Vec<String>, message: String| SyncRun {
        run_id: format!("{}#{number}", source.id),
        number,
        mode,
        started_at: synced_at as u64,
        ended_at: util::unix_now(),
        status,
        plugin: plugin_tag.clone(),
        received_nodes,
        received_edges,
        removals_applied,
        cursor_advanced,
        gaps,
        message,
    };
    let mut updated = source.clone();
    let report = match outcome {
        Ok((end, merge, patterns)) => {
            // A partial sync may have missed changes before its cursor, so the
            // next one starts from the same place.
            let advanced = end.complete && end.cursor.is_some();
            if advanced {
                updated.cursor = end.cursor;
            }
            let gaps: Vec<String> = end.gaps.iter().map(|gap| scrub(gap, &request.secrets)).collect();
            let status = if end.complete { RunStatus::Complete } else { RunStatus::Partial };
            let mut all: Vec<EdgePattern> = updated.patterns.clone();
            all.extend(patterns);
            all.sort();
            all.dedup();
            updated.patterns = all;
            updated.last_sync = Some(LastSync {
                at: synced_at as u64,
                ok: end.complete,
                nodes: merge.nodes_written,
                edges: merge.edges_written,
                message: gaps.join("; "),
            });
            let this_run = run(status, !append && mode == SyncMode::Full && end.complete, advanced, gaps, String::new());
            updated.push_run(this_run.clone());
            Ok(SyncReport {
                source: source.id.clone(),
                mode,
                received_nodes,
                received_edges,
                merge,
                elapsed_ms: started.elapsed().as_millis() as u64,
                run: this_run,
            })
        }
        Err(err) => {
            let message = scrub(&err.to_string(), &request.secrets);
            updated.last_sync = Some(LastSync {
                at: synced_at as u64,
                ok: false,
                nodes: 0,
                edges: 0,
                message: message.clone(),
            });
            let status = if matches!(err, PluginError::Cancelled) { RunStatus::Cancelled } else { RunStatus::Failed };
            updated.push_run(run(status, false, false, Vec::new(), message));
            Err(scrub_error(err, &request.secrets))
        }
    };
    settings::upsert_source(updated)?;
    report
}

/// `text` with any secret value this source holds masked: what a plugin says
/// is kept in settings and shown to people.
fn scrub(text: &str, secrets: &serde_json::Map<String, serde_json::Value>) -> String {
    secrets
        .values()
        .filter_map(serde_json::Value::as_str)
        .filter(|secret| !secret.is_empty())
        .fold(text.to_string(), |text, secret| text.replace(secret, "[redacted]"))
}

fn scrub_event(event: SyncEvent, secrets: &serde_json::Map<String, serde_json::Value>) -> SyncEvent {
    match event {
        SyncEvent::Progress { message, current, total } => SyncEvent::Progress { message: scrub(&message, secrets), current, total },
        SyncEvent::Log { level, message } => SyncEvent::Log { level, message: scrub(&message, secrets) },
        other => other,
    }
}

/// An error as it may be shown: what a plugin said can repeat a secret.
fn scrub_error(error: PluginError, secrets: &serde_json::Map<String, serde_json::Value>) -> PluginError {
    match error {
        PluginError::Failed { plugin, message } => PluginError::Failed {
            plugin,
            message: scrub(&message, secrets),
        },
        PluginError::Other(message) => PluginError::Other(scrub(&message, secrets)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_a_plugin_caused_is_masked_too() {
        let secrets = serde_json::json!({"key": "abc123"}).as_object().unwrap().clone();
        let error = PluginError::Failed { plugin: "p".into(), message: "denied abc123".into() };
        assert_eq!(scrub_error(error, &secrets).to_string(), "plugin p failed: denied [redacted]");
        let event = scrub_event(SyncEvent::Log { level: "warn".into(), message: "abc123".into() }, &secrets);
        assert_eq!(event, SyncEvent::Log { level: "warn".into(), message: "[redacted]".into() });
    }

    #[test]
    fn an_entry_may_not_leave_the_plugin_folder_on_any_platform() {
        for bad in ["../evil", "bin/../../evil", "..\\evil", "/usr/bin/evil", "\\\\host\\share", "C:\\evil", "c:evil"] {
            assert!(leaves_the_folder(bad), "{bad}");
        }
        for fine in ["plugin", "bin/plugin", "bin\\plugin.exe", "python3", "plugin.py", "--flag", "a..b"] {
            assert!(!leaves_the_folder(fine), "{fine}");
        }
    }

    #[test]
    fn a_long_line_is_refused_or_cut_without_being_held() {
        use std::io::Cursor;
        let text = format!("short\n{}\nafter\r\n", "x".repeat(100));

        // Refused: the stream stops at the long line.
        let mut reader = BufReader::with_capacity(8, Cursor::new(text.clone()));
        assert_eq!(read_line_capped(&mut reader, 10, false).unwrap(), Some(Ok("short".to_string())));
        assert_eq!(read_line_capped(&mut reader, 10, false).unwrap(), Some(Err(())));

        // Cut: stderr keeps the start, and goes on to the next line.
        let mut reader = BufReader::with_capacity(8, Cursor::new(text));
        assert_eq!(read_line_capped(&mut reader, 10, true).unwrap(), Some(Ok("short".to_string())));
        assert_eq!(read_line_capped(&mut reader, 10, true).unwrap(), Some(Ok("x".repeat(10))));
        assert_eq!(read_line_capped(&mut reader, 10, true).unwrap(), Some(Ok("after".to_string())));
        assert_eq!(read_line_capped(&mut reader, 10, true).unwrap(), None);

        // Exactly the cap is not too long, and bytes that are not UTF-8 do not stop it.
        let mut reader = BufReader::new(Cursor::new(b"0123456789\n\xff\n".to_vec()));
        assert_eq!(read_line_capped(&mut reader, 10, false).unwrap(), Some(Ok("0123456789".to_string())));
        assert_eq!(read_line_capped(&mut reader, 10, false).unwrap(), Some(Ok("\u{fffd}".to_string())));
    }

    #[test]
    fn an_archive_over_its_caps_is_refused_and_its_declared_size_is_not_trusted() {
        use std::io::Write;
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
        fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let path = dir.path().join("big.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for name in ["a.bin", "b.bin"] {
            writer.start_file(name, options).unwrap();
            writer.write_all(&vec![0u8; 100_000]).unwrap();
        }
        writer.finish().unwrap();
        let open = || zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
        let refused = |result: Result<(), UnpackError>| match result {
            Err(UnpackError::Refused(message)) => message,
            Err(UnpackError::Io(error)) => panic!("{error}"),
            Ok(()) => panic!("should be refused"),
        };

        let staging = dir.path().join("a");
        fs::create_dir_all(&staging).unwrap();
        assert!(refused(unpack(&mut open(), &staging, 150_000, 100)).contains("150000"));
        let staging = dir.path().join("b");
        fs::create_dir_all(&staging).unwrap();
        assert!(refused(unpack(&mut open(), &staging, u64::MAX - 1, 1)).contains("2 entries"));
        let staging = dir.path().join("c");
        fs::create_dir_all(&staging).unwrap();
        assert!(unpack(&mut open(), &staging, 200_000, 2).is_ok(), "exactly the caps fit");
    }

    #[test]
    fn a_plugin_folder_hashes_by_what_it_holds() {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
        fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        fs::create_dir_all(dir.path().join("lib")).unwrap();
        fs::write(dir.path().join("plugin.py"), "print()").unwrap();
        fs::write(dir.path().join("lib/util.py"), "x = 1").unwrap();
        let info = PluginInfo { dir: dir.path().to_path_buf(), ..plugin_with_entry("python3") };
        let first = identity(&info).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(identity(&info).unwrap(), first);

        // What an interpreter writes as it runs does not need approval again.
        fs::create_dir_all(dir.path().join("__pycache__")).unwrap();
        fs::write(dir.path().join("__pycache__/util.pyc"), "compiled").unwrap();
        assert_eq!(identity(&info).unwrap(), first);

        // A changed, added or renamed file does, however deep.
        fs::write(dir.path().join("lib/util.py"), "x = 2").unwrap();
        let changed = identity(&info).unwrap();
        assert_ne!(changed, first);
        fs::write(dir.path().join("lib/extra.py"), "").unwrap();
        let added = identity(&info).unwrap();
        assert_ne!(added, changed);
        fs::rename(dir.path().join("lib/extra.py"), dir.path().join("lib/other.py")).unwrap();
        assert_ne!(identity(&info).unwrap(), added);
    }

    #[test]
    fn a_secret_a_plugin_repeats_is_masked_before_it_is_kept() {
        let secrets = serde_json::json!({"secret_access_key": "abc123", "empty": ""}).as_object().unwrap().clone();
        assert_eq!(scrub("denied for abc123 and abc123", &secrets), "denied for [redacted] and [redacted]");
        assert_eq!(scrub("nothing here", &secrets), "nothing here", "an empty secret must not match everywhere");
    }

    fn plugin_with_entry(entry: &str) -> PluginInfo {
        PluginInfo {
            manifest: Manifest {
                id: "script".into(),
                name: "Script".into(),
                version: "0.1.0".into(),
                protocol: 1,
                entry: vec![entry.into(), "plugin.py".into()],
                description: String::new(),
                permissions: Default::default(),
            },
            dir: PathBuf::from("no-such-plugin-dir"),
            builtin: false,
        }
    }

    #[test]
    fn a_bare_entry_missing_from_the_plugin_folder_runs_from_path() {
        // cargo is on PATH wherever these tests run.
        let exe = plugin_with_entry("cargo").executable();
        assert!(exe.is_file(), "{}", exe.display());
        assert!(!exe.starts_with("no-such-plugin-dir"));
    }

    #[test]
    fn an_entry_with_a_folder_stays_inside_the_plugin_folder() {
        let exe = plugin_with_entry("bin/cargo").executable();
        assert!(exe.starts_with("no-such-plugin-dir"), "{}", exe.display());
    }
}

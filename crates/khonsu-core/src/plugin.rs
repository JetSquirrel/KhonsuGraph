//! Data-source plugins: discovery, running one plugin operation in its own
//! process, and syncing a configured source into a graph.
//!
//! Built-in and third-party plugins go through the same path: a directory with
//! a `khonsu-plugin.json` manifest and an executable speaking
//! [`khonsu_plugin_protocol`]. Built-in plugins are looked up next to the
//! running executable (`plugins/<id>/` or a bare `khonsu-plugin-<id>` binary).

use crate::engine::{Graph, QueryError};
use crate::merge::{EdgePattern, MergeStats, Merger};
use crate::settings::{self, LastSync, Source};
use crate::{secrets, util};
use khonsu_plugin_protocol::{
    parse_line, Describe, EdgeRecord, Manifest, Message, NodeRecord, Permissions, Request, SyncMode, MANIFEST_FILE,
    PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// IDs of plugins shipped with KhonsuGraph.
pub const BUILTIN_PLUGINS: &[&str] = &["csv", "aws"];

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

    fn command(&self) -> Command {
        let mut command = Command::new(self.executable());
        command.args(&self.manifest.entry[1..]).current_dir(&self.dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command
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
    #[error("plugin {plugin} needs approval of its permissions before it runs")]
    NotApproved { plugin: String, permissions: Permissions },
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
    if let Ok(exe) = std::env::current_exe() {
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
    if manifest.id.is_empty() || !manifest.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(PluginError::Invalid {
            path,
            message: "id must be letters, digits, '-' or '_'".into(),
        });
    }
    Ok(manifest)
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

/// Installs a plugin from a local directory by copying it into the user
/// plugin directory. Returns the installed plugin.
pub fn install(from: &Path) -> Result<PluginInfo, PluginError> {
    let manifest = read_manifest(from)?;
    if BUILTIN_PLUGINS.contains(&manifest.id.as_str()) {
        return Err(PluginError::Other(format!("{} is a built-in plugin ID", manifest.id)));
    }
    let dest = settings::plugins_dir().join(&manifest.id);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir(from, &dest)?;
    let info = PluginInfo {
        manifest,
        dir: dest,
        builtin: false,
    };
    if !info.executable().is_file() {
        let exe = info.executable();
        fs::remove_dir_all(&info.dir)?;
        return Err(PluginError::Invalid {
            path: exe,
            message: "entry executable not found".into(),
        });
    }
    Ok(info)
}

pub fn uninstall(id: &str) -> Result<(), PluginError> {
    let dir = settings::plugins_dir().join(id);
    if !dir.exists() {
        return Err(PluginError::NotFound(id.to_string()));
    }
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Collects the last lines a plugin wrote to stderr, for error messages.
fn drain_stderr(child: &mut Child) -> Arc<Mutex<VecDeque<String>>> {
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = Arc::clone(&tail);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut tail = tail.lock().unwrap();
                tail.push_back(line);
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

pub fn describe(plugin: &PluginInfo) -> Result<Describe, PluginError> {
    let output = plugin
        .command()
        .arg("describe")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| PluginError::Failed {
            plugin: plugin.id().to_string(),
            message: format!("cannot start {}: {e}", plugin.executable().display()),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(Ok(Message::Describe(describe))) = parse_line(line) {
            return Ok(describe);
        }
    }
    Err(PluginError::Failed {
        plugin: plugin.id().to_string(),
        message: format!("no describe message; stderr: {}", String::from_utf8_lossy(&output.stderr).trim()),
    })
}

fn spawn(plugin: &PluginInfo, operation: &str, request: &Request) -> Result<Child, PluginError> {
    let mut child = plugin
        .command()
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
    let mut child = spawn(plugin, "test", request)?;
    let stderr = drain_stderr(&mut child);
    let mut stdout = String::new();
    child.stdout.take().expect("stdout is piped").read_to_string(&mut stdout)?;
    child.wait()?;
    for line in stdout.lines() {
        match parse_line(line) {
            Some(Ok(Message::TestResult { ok, message })) => return Ok((ok, message)),
            Some(Ok(Message::Error { message })) => return Ok((false, message)),
            _ => {}
        }
    }
    Err(failure(plugin, "no test result".into(), &stderr))
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
pub fn run_sync(
    plugin: &PluginInfo,
    request: &Request,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(SyncEvent),
    mut on_record: impl FnMut(Record) -> Result<(), PluginError>,
) -> Result<Option<serde_json::Value>, PluginError> {
    let mut child = spawn(plugin, "sync", request)?;
    let stderr = drain_stderr(&mut child);
    let stdout = child.stdout.take().expect("stdout is piped");

    // Read on a thread so cancellation does not wait for the next line.
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1024);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut cursor = None;
    let mut done = false;
    let (mut nodes, mut edges) = (0u64, 0u64);
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break Err(PluginError::Cancelled);
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        };
        let message = match parse_line(&line) {
            None => continue,
            Some(Ok(message)) => message,
            Some(Err(err)) => {
                let _ = child.kill();
                break Err(failure(plugin, format!("invalid output line: {err}: {line}"), &stderr));
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
            Message::Done { .. } => {
                done = true;
                Ok(())
            }
            Message::Error { message } => {
                let _ = child.kill();
                break Err(failure(plugin, message, &stderr));
            }
            Message::Describe(_) | Message::TestResult { .. } => Ok(()),
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
    Ok(cursor)
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
    run_sync(plugin, request, cancel, on_event, |record| {
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
    })?;
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

/// Syncs a configured source into `graph`, then records the outcome in the
/// source's settings. The plugin's declared permissions must be approved.
pub fn sync_source(
    graph: &mut Graph,
    source: &Source,
    mode: SyncMode,
    cancel: &AtomicBool,
    on_event: impl FnMut(SyncEvent),
) -> Result<SyncReport, PluginError> {
    let plugin = find(&source.plugin)?;
    let describe = describe(&plugin)?;
    let mode = if mode == SyncMode::Incremental && !describe.supports_incremental {
        SyncMode::Full
    } else {
        mode
    };
    if !plugin.builtin && !settings::is_approved(plugin.id(), &plugin.manifest.version, &plugin.manifest.permissions) {
        return Err(PluginError::NotApproved {
            plugin: plugin.id().to_string(),
            permissions: plugin.manifest.permissions.clone(),
        });
    }
    let request = request_for(source, mode)?;
    let started = std::time::Instant::now();
    let synced_at = util::unix_now() as i64;
    let plugin_tag = format!("{}@{}", plugin.id(), describe.version);
    let mut merger = Merger::new(graph, &source.id, &plugin_tag, synced_at);
    let (mut received_nodes, mut received_edges) = (0, 0);
    let outcome = run_sync(&plugin, &request, cancel, on_event, |record| {
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
    .and_then(|cursor| {
        let (merge, patterns) = merger.finish(mode == SyncMode::Full, &source.patterns)?;
        Ok((cursor, merge, patterns))
    });

    let mut updated = source.clone();
    let report = match outcome {
        Ok((cursor, merge, patterns)) => {
            if cursor.is_some() {
                updated.cursor = cursor;
            }
            let mut all: Vec<EdgePattern> = updated.patterns.clone();
            all.extend(patterns);
            all.sort();
            all.dedup();
            updated.patterns = all;
            updated.last_sync = Some(LastSync {
                at: synced_at as u64,
                ok: true,
                nodes: merge.nodes_written,
                edges: merge.edges_written,
                message: String::new(),
            });
            Ok(SyncReport {
                source: source.id.clone(),
                mode,
                received_nodes,
                received_edges,
                merge,
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        }
        Err(err) => {
            updated.last_sync = Some(LastSync {
                at: synced_at as u64,
                ok: false,
                nodes: 0,
                edges: 0,
                message: err.to_string(),
            });
            Err(err)
        }
    };
    settings::upsert_source(updated)?;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

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

//! Per-user state shared by the desktop app and the CLI: recent graphs, query
//! history, configured data sources and plugin approvals. Stored as JSON in the
//! platform config directory (`KHONSU_CONFIG_DIR` overrides it). Secrets are
//! never written here; see [`crate::secrets`].
//!
//! Reading for display is forgiving: a file that is missing or can't be read
//! shows as its defaults. Changing is not: every change goes through
//! [`crate::store::update`], which refuses a file it can't read rather than
//! write the defaults over it, and locks the file across the read and the
//! write so that the window and a command can't lose each other's change.

use crate::engine::Language;
use crate::merge::EdgePattern;
use crate::store;
use khonsu_plugin_protocol::{Permissions, SyncMode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
#[cfg(test)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 12;
const MAX_HISTORY: usize = 500;

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KHONSU_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("KhonsuGraph")
}

/// Directory holding user-installed plugins, one subdirectory each.
pub fn plugins_dir() -> PathBuf {
    config_dir().join("plugins")
}

/// For display: the defaults when the file is missing or can't be read.
fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    store::read(&config_dir().join(name)).ok().flatten().unwrap_or_default()
}

/// Read, change and write `name` as one step. Refused, with the file left as
/// it was, when what is there can't be read.
fn update<T: DeserializeOwned + Serialize + Default>(name: &str, change: impl FnOnce(&mut T)) -> io::Result<T> {
    store::update(&config_dir().join(name), |value: &mut T| {
        change(value);
        Ok::<_, io::Error>(())
    })
    .map(|((), value)| value)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Most recent first.
    #[serde(default)]
    pub recent: Vec<PathBuf>,
    /// The graph to reopen on start; cleared when the user closes it.
    #[serde(default)]
    pub last_opened: Option<PathBuf>,
    /// `system`, `light` or `dark`.
    #[serde(default)]
    pub theme: Option<String>,
    /// `en` or `zh`.
    #[serde(default)]
    pub locale: Option<String>,
    /// Other preferences of the desktop app (interface size, panel state, ...).
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, String>,
}

impl Settings {
    pub fn load() -> Self {
        load("settings.json")
    }

    /// Changes the stored settings, starting from what is on disk now, and
    /// returns them as written.
    pub fn update(change: impl FnOnce(&mut Self)) -> io::Result<Self> {
        update("settings.json", change)
    }

    /// A stored preference, read fresh from disk.
    pub fn get_value(key: &str) -> Option<String> {
        Self::load().values.get(key).cloned()
    }

    /// Stores one preference (a blocking read-modify-write of settings.json).
    pub fn set_value(key: &str, value: &str) -> io::Result<()> {
        Self::update(|settings| {
            settings.values.insert(key.to_string(), value.to_string());
        })
        .map(|_| ())
    }

    pub fn note_opened(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
        self.last_opened = Some(path.to_path_buf());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub language: Language,
    pub text: String,
    pub graph: PathBuf,
    /// Unix seconds.
    pub at: u64,
    pub ok: bool,
}

pub fn load_history() -> Vec<HistoryEntry> {
    load("history.json")
}

/// Adds an entry (most recent first), replacing an identical earlier query.
pub fn push_history(entry: HistoryEntry) -> io::Result<Vec<HistoryEntry>> {
    update("history.json", |history: &mut Vec<HistoryEntry>| {
        history.retain(|h| !(h.text == entry.text && h.language == entry.language && h.graph == entry.graph));
        history.insert(0, entry);
        history.truncate(MAX_HISTORY);
    })
}

/// Empties the history, whatever the file held: asked for by name, so a file
/// that can't be read is no reason to refuse.
pub fn clear_history() -> io::Result<()> {
    let path = config_dir().join("history.json");
    let _lock = store::lock(&path)?;
    store::write(&path, &Vec::<HistoryEntry>::new())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastSync {
    pub at: u64,
    pub ok: bool,
    pub nodes: u64,
    pub edges: u64,
    pub message: String,
}

/// How one sync ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The plugin enumerated the whole source.
    Complete,
    /// It finished, and said part of the source could not be enumerated.
    Partial,
    /// It stopped early. Records written before that are kept.
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn word(self) -> &'static str {
        match self {
            RunStatus::Complete => "complete",
            RunStatus::Partial => "partial",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

/// One sync of a source. The plugin's cursor is not copied here: it stays on
/// the source, and only whether it moved is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRun {
    /// `<source>#<number>`, counting this source's syncs.
    pub run_id: String,
    pub number: u64,
    pub mode: SyncMode,
    pub started_at: u64,
    pub ended_at: u64,
    pub status: RunStatus,
    /// `<plugin id>@<plugin version>`: what mapped the source to records.
    pub plugin: String,
    pub received_nodes: u64,
    pub received_edges: u64,
    /// Whether records this sync did not see were removed.
    pub removals_applied: bool,
    pub cursor_advanced: bool,
    /// What the plugin said it could not enumerate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// Runs kept per source; the oldest goes first.
pub const MAX_RUNS: usize = 20;

/// A configured data source: one plugin instance writing into one graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    /// Stable ID; prefixes every `khonsu_id` this source writes.
    pub id: String,
    pub name: String,
    pub plugin: String,
    pub graph: PathBuf,
    /// Non-secret configuration values.
    #[serde(default)]
    pub config: Map<String, Json>,
    /// Keys whose values live in the system keychain.
    #[serde(default)]
    pub secret_keys: Vec<String>,
    #[serde(default)]
    pub cursor: Option<Json>,
    #[serde(default)]
    pub patterns: Vec<EdgePattern>,
    #[serde(default)]
    pub last_sync: Option<LastSync>,
    /// The latest syncs, oldest first. Empty for a source last synced before
    /// these were kept.
    #[serde(default)]
    pub runs: Vec<SyncRun>,
}

impl Source {
    pub fn last_run(&self) -> Option<&SyncRun> {
        self.runs.last()
    }

    pub fn last_complete_run(&self) -> Option<&SyncRun> {
        self.runs.iter().rev().find(|run| run.status == RunStatus::Complete)
    }

    /// Adds a run, dropping the oldest beyond [`MAX_RUNS`].
    pub fn push_run(&mut self, run: SyncRun) {
        self.runs.push(run);
        let extra = self.runs.len().saturating_sub(MAX_RUNS);
        self.runs.drain(..extra);
    }
}

pub fn load_sources() -> Vec<Source> {
    load("sources.json")
}

/// The sources, or why they can't be read: for a caller about to act on
/// there being none.
pub fn try_load_sources() -> io::Result<Vec<Source>> {
    Ok(store::read(&config_dir().join("sources.json"))?.unwrap_or_default())
}

pub fn upsert_source(source: Source) -> io::Result<()> {
    update("sources.json", |sources: &mut Vec<Source>| match sources.iter_mut().find(|s| s.id == source.id) {
        Some(existing) => *existing = source,
        None => sources.push(source),
    })
    .map(|_| ())
}

pub fn remove_source(id: &str) -> io::Result<()> {
    update("sources.json", |sources: &mut Vec<Source>| sources.retain(|s| s.id != id)).map(|_| ())
}

/// Makes a source ID from a display name, unique among existing sources.
pub fn new_source_id(name: &str) -> String {
    let base: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() { "source".to_string() } else { base };
    let existing: Vec<String> = load_sources().into_iter().map(|s| s.id).collect();
    let mut id = base.clone();
    let mut n = 2;
    while existing.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// A user's confirmation of a plugin: the version and permissions it declared,
/// and what was in its folder at the time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub plugin: String,
    pub version: String,
    pub permissions: Permissions,
    /// A hash of the plugin's folder. Absent on approvals from before this was
    /// kept, which count as out of date.
    #[serde(default)]
    pub identity: Option<String>,
    pub at: u64,
}

pub fn load_approvals() -> Vec<Approval> {
    load("approvals.json")
}

pub fn approve(plugin: &str, version: &str, permissions: &Permissions, identity: &str) -> io::Result<()> {
    update("approvals.json", |approvals: &mut Vec<Approval>| {
        approvals.retain(|a| a.plugin != plugin);
        approvals.push(Approval {
            plugin: plugin.to_string(),
            version: version.to_string(),
            permissions: permissions.clone(),
            identity: Some(identity.to_string()),
            at: crate::util::unix_now(),
        });
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #69: a settings file that didn't parse read as empty, and the next
    /// `source add` wrote that emptiness, plus one source, over all the others.
    #[test]
    fn a_change_never_overwrites_a_file_that_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sources.json");
        let broken = "[{\"id\": \"shop\", \"name\": \"Shop\"";
        fs::write(&path, broken).unwrap();
        let add = |value: &mut Vec<Json>| value.push(Json::from("new"));
        let error = store::update(&path, |value: &mut Vec<Json>| {
            add(value);
            Ok::<_, io::Error>(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("sources.json"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), broken, "the file is as it was");
        // For display it reads as nothing, without touching the file.
        let shown: Vec<Json> = store::read(&path).ok().flatten().unwrap_or_default();
        assert!(shown.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }
}

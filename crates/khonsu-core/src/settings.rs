//! Per-user state shared by the desktop app and the CLI: recent graphs, query
//! history, configured data sources and plugin approvals. Stored as JSON in the
//! platform config directory (`KHONSU_CONFIG_DIR` overrides it). Secrets are
//! never written here; see [`crate::secrets`].

use crate::engine::Language;
use crate::merge::EdgePattern;
use khonsu_plugin_protocol::Permissions;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
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

fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    fs::read(config_dir().join(name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save<T: Serialize>(name: &str, value: &T) -> io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    fs::rename(tmp, path)
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

    pub fn save(&self) -> io::Result<()> {
        save("settings.json", self)
    }

    /// A stored preference, read fresh from disk.
    pub fn get_value(key: &str) -> Option<String> {
        Self::load().values.get(key).cloned()
    }

    /// Stores one preference (a blocking read-modify-write of settings.json).
    pub fn set_value(key: &str, value: &str) -> io::Result<()> {
        let mut settings = Self::load();
        settings.values.insert(key.to_string(), value.to_string());
        settings.save()
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
    let mut history = load_history();
    history.retain(|h| !(h.text == entry.text && h.language == entry.language && h.graph == entry.graph));
    history.insert(0, entry);
    history.truncate(MAX_HISTORY);
    save("history.json", &history)?;
    Ok(history)
}

pub fn clear_history() -> io::Result<()> {
    save::<Vec<HistoryEntry>>("history.json", &Vec::new())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LastSync {
    pub at: u64,
    pub ok: bool,
    pub nodes: u64,
    pub edges: u64,
    pub message: String,
}

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
}

pub fn load_sources() -> Vec<Source> {
    load("sources.json")
}

pub fn save_sources(sources: &[Source]) -> io::Result<()> {
    save("sources.json", &sources)
}

pub fn upsert_source(source: Source) -> io::Result<()> {
    let mut sources = load_sources();
    match sources.iter_mut().find(|s| s.id == source.id) {
        Some(existing) => *existing = source,
        None => sources.push(source),
    }
    save_sources(&sources)
}

pub fn remove_source(id: &str) -> io::Result<()> {
    let mut sources = load_sources();
    sources.retain(|s| s.id != id);
    save_sources(&sources)
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

/// A user's confirmation of the permissions a plugin version declared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub plugin: String,
    pub version: String,
    pub permissions: Permissions,
    pub at: u64,
}

pub fn load_approvals() -> Vec<Approval> {
    load("approvals.json")
}

/// True when this exact version and permission set was approved before.
pub fn is_approved(plugin: &str, version: &str, permissions: &Permissions) -> bool {
    load_approvals()
        .iter()
        .any(|a| a.plugin == plugin && a.version == version && &a.permissions == permissions)
}

pub fn approve(plugin: &str, version: &str, permissions: &Permissions) -> io::Result<()> {
    let mut approvals = load_approvals();
    approvals.retain(|a| a.plugin != plugin);
    approvals.push(Approval {
        plugin: plugin.to_string(),
        version: version.to_string(),
        permissions: permissions.clone(),
        at: crate::util::unix_now(),
    });
    save("approvals.json", &approvals)
}

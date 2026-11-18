//! A log of what was written to a graph by hand.
//!
//! Everything a data source writes can be made again from the source; a
//! statement run with `khonsu query --write` cannot, and `khonsu graph rebuild`
//! does not carry it over. Each such statement is appended to `writes.jsonl`
//! in the config directory as one JSON line: when, which graph, the
//! statement, who ran it and how it reached the graph. Key material in the
//! statement and secret-looking parameters are masked first, as in evidence.
//!
//! The log is for a person to read back ("what did the agent change?"), not
//! a guarantee: anyone who can run the command can edit the file. It is kept
//! to about [`MAX_BYTES`]; past that the current file becomes
//! `writes.1.jsonl`, replacing the one before.

use crate::engine::Language;
use crate::evidence;
use crate::settings::config_dir;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const FILE: &str = "writes.jsonl";
const PREVIOUS: &str = "writes.1.jsonl";
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// One statement that wrote to a graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Write {
    /// Unix seconds, when it finished.
    pub at: u64,
    pub graph: PathBuf,
    pub language: Language,
    pub statement: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Json>,
    /// The agent that ran it, when one was detected; otherwise not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// `cli` when the command opened the graph itself, `window` when it went
    /// through the window that had the graph open.
    pub via: String,
}

impl Write {
    pub fn new(graph: &Path, language: Language, statement: &str, params: &Map<String, Json>, agent: Option<String>, via: &str) -> Self {
        Self {
            at: crate::util::unix_now(),
            graph: crate::normalize_path(graph),
            language,
            statement: evidence::scrub(statement),
            params: params.iter().map(|(name, value)| (name.clone(), evidence::masked(name, value).0)).collect(),
            agent,
            via: via.into(),
        }
    }
}

/// The log file.
pub fn path() -> PathBuf {
    config_dir().join(FILE)
}

/// Appends `write` to the log.
pub fn record(write: &Write) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let file = dir.join(FILE);
    if fs::metadata(&file).is_ok_and(|meta| meta.len() >= MAX_BYTES) {
        fs::rename(&file, dir.join(PREVIOUS))?;
    }
    let mut line = serde_json::to_string(write).expect("a write serializes");
    line.push('\n');
    // One write call per line, so lines from two commands do not interleave.
    OpenOptions::new().create(true).append(true).open(&file)?.write_all(line.as_bytes())
}

/// The writes logged for `graph`, oldest first, from both files.
pub fn read(graph: &Path) -> Vec<Write> {
    let graph = crate::normalize_path(graph);
    let dir = config_dir();
    [PREVIOUS, FILE]
        .iter()
        .filter_map(|name| fs::read_to_string(dir.join(name)).ok())
        .flat_map(|text| text.lines().filter_map(|line| serde_json::from_str::<Write>(line).ok()).collect::<Vec<_>>())
        .filter(|write| write.graph == graph)
        .collect()
}

//! KhonsuGraph data-source plugin protocol, version 1 (experimental).
//!
//! A plugin is an executable started by the host once per operation:
//!
//! ```text
//! <plugin> describe            # prints one `Describe` message
//! <plugin> test                # reads a `Request` on stdin, prints `TestResult`
//! <plugin> sync                # reads a `Request` on stdin, streams records
//! ```
//!
//! Every message is one JSON object per line on stdout, tagged by `type`.
//! Anything a plugin writes to stderr is treated as diagnostics. Plugins never
//! open the graph directory: the host is the only writer and merges records by
//! their stable IDs. Cancellation is the host terminating the process.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{self, BufRead, Write};

pub const PROTOCOL_VERSION: u32 = 1;

/// Name of the manifest file inside an installed plugin directory.
pub const MANIFEST_FILE: &str = "khonsu-plugin.json";

/// Static plugin manifest, read by the host without running the plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    /// Command to run, relative to the plugin directory. The first element is
    /// the executable; `.exe` is appended on Windows when missing.
    pub entry: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Permissions,
}

/// What a plugin declares it needs. Users confirm this before first use.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Permissions {
    /// Network domains the plugin contacts, e.g. `*.amazonaws.com`.
    #[serde(default)]
    pub network: Vec<String>,
    /// Whether the plugin reads local files the user points it at.
    #[serde(default)]
    pub read_files: bool,
    /// External commands the plugin runs, e.g. `aws`.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Free-form notes, e.g. "read-only IAM policy is enough".
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Output of `<plugin> describe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Describe {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub config: Vec<ConfigField>,
    #[serde(default)]
    pub permissions: Permissions,
    /// Node labels this plugin may produce.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Relationship types this plugin may produce.
    #[serde(default)]
    pub relationship_types: Vec<String>,
    #[serde(default)]
    pub supports_incremental: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Secret values are stored in the system keychain and passed only in
    /// `Request::secrets`, never in `Request::config`.
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub help: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[default]
    String,
    Number,
    Bool,
    /// A local file path chosen by the user.
    File,
    /// Free-form JSON, e.g. a mapping table.
    Json,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    #[default]
    Full,
    Incremental,
}

/// Sent on stdin for `test` and `sync`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    #[serde(default)]
    pub config: Map<String, Value>,
    #[serde(default)]
    pub secrets: Map<String, Value>,
    #[serde(default)]
    pub mode: SyncMode,
    /// Last `Cursor` value reported by this source, for incremental sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
}

/// A node with an ID that is stable within one data source (e.g. an ARN).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeRecord {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

/// A relationship. `source` and `target` are node stable IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeRecord {
    pub id: String,
    pub source: String,
    pub target: String,
    pub rel_type: String,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

/// One line of plugin output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Describe(Describe),
    TestResult {
        ok: bool,
        #[serde(default)]
        message: String,
    },
    Node(NodeRecord),
    Edge(EdgeRecord),
    Progress {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    Log {
        #[serde(default)]
        level: String,
        message: String,
    },
    /// Position to resume from on the next incremental sync.
    Cursor { value: Value },
    Done {
        #[serde(default)]
        nodes: u64,
        #[serde(default)]
        edges: u64,
    },
    Error { message: String },
}

/// Helper for plugin authors: writes messages as JSON lines to stdout.
pub struct Emitter<W: Write> {
    out: W,
}

impl Emitter<io::BufWriter<io::Stdout>> {
    pub fn stdout() -> Self {
        Self {
            out: io::BufWriter::new(io::stdout()),
        }
    }
}

impl<W: Write> Emitter<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }

    pub fn emit(&mut self, message: &Message) -> io::Result<()> {
        serde_json::to_writer(&mut self.out, message)?;
        self.out.write_all(b"\n")?;
        // Progress and terminal messages are flushed so the host sees them promptly.
        if !matches!(message, Message::Node(_) | Message::Edge(_)) {
            self.out.flush()?;
        }
        Ok(())
    }

    pub fn node(&mut self, record: NodeRecord) -> io::Result<()> {
        self.emit(&Message::Node(record))
    }

    pub fn edge(&mut self, record: EdgeRecord) -> io::Result<()> {
        self.emit(&Message::Edge(record))
    }

    pub fn progress(&mut self, message: impl Into<String>, current: Option<u64>, total: Option<u64>) -> io::Result<()> {
        self.emit(&Message::Progress {
            message: message.into(),
            current,
            total,
        })
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// Helper for plugin authors: reads the `Request` the host sends on stdin.
pub fn read_request() -> io::Result<Request> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Parses one line of plugin output. Blank lines yield `None`.
pub fn parse_line(line: &str) -> Option<Result<Message, serde_json::Error>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(serde_json::from_str(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_tagged_lines() {
        let node = Message::Node(NodeRecord {
            id: "i-1".into(),
            label: "Instance".into(),
            properties: Map::new(),
        });
        let line = serde_json::to_string(&node).unwrap();
        assert!(line.starts_with(r#"{"type":"node""#), "{line}");
        assert_eq!(parse_line(&line).unwrap().unwrap(), node);
        assert!(parse_line("   ").is_none());
    }

    #[test]
    fn edge_line_written_by_a_third_party_parses() {
        let line = r#"{"type":"edge","id":"e1","source":"a","target":"b","rel_type":"IN_VPC"}"#;
        match parse_line(line).unwrap().unwrap() {
            Message::Edge(edge) => {
                assert_eq!(edge.rel_type, "IN_VPC");
                assert!(edge.properties.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

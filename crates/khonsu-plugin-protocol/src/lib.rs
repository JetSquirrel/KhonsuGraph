//! KhonsuGraph data-source plugin protocol, version 1 (experimental).
//!
//! A plugin is an executable started by the host once per operation:
//!
//! ```text
//! <plugin> describe            # prints one `Describe` message
//! <plugin> test                # reads a `Request` on stdin, prints `TestResult`
//! <plugin> sync                # reads a `Request` on stdin, streams records
//! <plugin> analyze             # optional: reads an `AnalyzeRequest`, streams findings
//! ```
//!
//! Every message is one JSON object per line on stdout, tagged by `type`.
//! Anything a plugin writes to stderr is treated as diagnostics. Plugins never
//! open the graph directory: the host is the only writer and merges records by
//! their stable IDs. Cancellation is the host terminating the process.
//!
//! `analyze` is only run for an analysis the plugin lists in `Describe::analyses`.
//! The host reads the graph and sends records, and the plugin answers in stable
//! references to them, so what it says can be checked against the graph.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
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
    /// Analyses this plugin offers over a graph. Most plugins offer none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analyses: Vec<AnalysisInfo>,
}

/// An analysis a plugin offers: what it reads, and the words for what it says.
/// All meaning lives here and in the plugin, and the host only checks and shows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalysisInfo {
    /// Letters, digits, `-` or `_`, unique within the plugin.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// What a result does and does not show.
    #[serde(default)]
    pub claim: String,
    /// Labels a finding can be about. The window offers the analysis on a node
    /// with one of them.
    #[serde(default)]
    pub subject_labels: Vec<String>,
    /// Labels whose nodes are read even without a relationship in scope.
    #[serde(default)]
    pub node_labels: Vec<String>,
    /// Relationship types to read, with the nodes at their ends.
    #[serde(default)]
    pub relationship_types: Vec<String>,
    /// Every verdict a finding can have, most in need of attention first. Findings
    /// are ordered by this.
    pub verdicts: Vec<VerdictInfo>,
    /// Every rule a finding can come from.
    pub rules: Vec<RuleInfo>,
    /// Text for other languages: locale, then `title`, `claim`, `verdict.<id>`,
    /// `rule.<id>` or `condition.<id>`. Anything missing falls back on the text above.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub translations: BTreeMap<String, BTreeMap<String, String>>,
}

impl AnalysisInfo {
    /// The text for `key` in `locale`, if the plugin supplies one.
    pub fn translated(&self, locale: &str, key: &str) -> Option<&str> {
        self.translations.get(locale)?.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictInfo {
    pub id: String,
    pub title: String,
    pub tone: VerdictTone,
}

/// How much a verdict asks of the reader, which is all the host needs to know
/// to show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictTone {
    /// Worth acting on.
    Attention,
    /// Could not be decided, so worth getting more data.
    Undecided,
    /// Nothing to act on. This is not a claim that all is well.
    Quiet,
}

/// A rule in words, for a person reading a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleInfo {
    pub rule_id: String,
    pub title: String,
    pub statement: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
}

/// Sent on stdin for `analyze`, as one line. It holds no source configuration
/// or secrets, and record properties have secrets masked.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalyzeRequest {
    pub protocol: u32,
    /// The `AnalysisInfo::id` to run.
    pub analysis: String,
    pub nodes: Vec<AnalysisNode>,
    pub edges: Vec<AnalysisEdge>,
    /// False when the graph held more than the host reads, so absent records
    /// say nothing.
    #[serde(default = "default_complete")]
    pub complete: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalysisNode {
    /// The stable reference to this record: use it to refer to it in findings.
    #[serde(rename = "ref")]
    pub reference: String,
    pub labels: Vec<String>,
    /// What people call it.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AnalysisEdge {
    #[serde(rename = "ref")]
    pub reference: String,
    /// The node this relationship leaves and arrives at, by reference.
    pub from: String,
    pub to: String,
    pub rel_type: String,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

/// Whether a condition holds. Unknown is not the same as false: what was not
/// read cannot rule anything out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionStatus {
    Met,
    NotMet,
    Unknown,
}

/// A field a condition read. The host looks its value up in the record itself,
/// so a plugin cannot misquote it. A relationship's ends read as `@from` and `@to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRef {
    pub record: String,
    pub field: String,
    /// Where the source calls it this, e.g. the API and field it came from.
    #[serde(default)]
    pub source_field: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionRecord {
    pub id: String,
    pub statement: String,
    pub status: ConditionStatus,
    /// What was found, in words.
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub fields: Vec<FieldRef>,
}

/// One relationship crossed: along its arrow, or against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopRef {
    pub edge: String,
    pub along: bool,
}

/// A path as references, which is all that crosses the process boundary. The host
/// checks that it connects and rebuilds it from the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRef {
    /// How it was worked out.
    pub derivation: String,
    pub hops: Vec<HopRef>,
}

/// One finding, as a plugin reports it. Every reference must be to a record in
/// the request, and the host refuses the whole result if one is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRecord {
    /// The same rule and records should give the same ID.
    pub finding_id: String,
    pub rule_id: String,
    /// A `VerdictInfo::id`.
    pub verdict: String,
    /// The node it is about, by reference.
    pub resource: String,
    #[serde(default)]
    pub conditions: Vec<ConditionRecord>,
    /// What could not be evaluated.
    #[serde(default)]
    pub unknowns: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathRef>,
    /// The finding this one rests on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// Records it rests on beyond those its conditions and path name.
    #[serde(default)]
    pub records: Vec<String>,
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
    /// One finding of an `analyze`.
    Finding(Box<FindingRecord>),
    /// The end of an `analyze`. Without it the result is refused.
    AnalysisDone {
        /// How many resources were looked at.
        #[serde(default)]
        subjects: u64,
    },
}

fn default_complete() -> bool {
    true
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

/// Helper for plugin authors: reads the `AnalyzeRequest` the host sends on stdin.
pub fn read_analyze_request() -> io::Result<AnalyzeRequest> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// A hash that stays the same across platforms and Rust versions, for IDs that
/// must not change between runs.
pub fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
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
    fn a_describe_from_before_analyses_parses_with_none() {
        let line = r#"{"type":"describe","name":"Old","version":"1"}"#;
        match parse_line(line).unwrap().unwrap() {
            Message::Describe(describe) => assert!(describe.analyses.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_finding_and_its_path_survive_the_line() {
        let finding = Message::Finding(Box::new(FindingRecord {
            finding_id: "finding:1".into(),
            rule_id: "rule".into(),
            verdict: "flagged".into(),
            resource: "node:a".into(),
            conditions: vec![ConditionRecord {
                id: "c".into(),
                statement: "It holds.".into(),
                status: ConditionStatus::Unknown,
                detail: String::new(),
                fields: vec![FieldRef { record: "node:a".into(), field: "state".into(), source_field: "State".into() }],
            }],
            unknowns: vec![],
            assumptions: vec![],
            path: Some(PathRef { derivation: "by rule".into(), hops: vec![HopRef { edge: "rel:ab".into(), along: false }] }),
            depends_on: Some("finding:0".into()),
            records: vec![],
        }));
        let line = serde_json::to_string(&finding).unwrap();
        assert!(line.contains(r#""status":"unknown""#), "{line}");
        assert_eq!(parse_line(&line).unwrap().unwrap(), finding);
    }

    #[test]
    fn a_record_is_referred_to_as_ref_on_the_wire() {
        let node = AnalysisNode { reference: "node:a".into(), ..AnalysisNode::default() };
        assert!(serde_json::to_string(&node).unwrap().starts_with(r#"{"ref":"node:a""#));
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

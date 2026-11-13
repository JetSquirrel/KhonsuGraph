//! Investigation templates: a question asked the same way each time.
//!
//! A template is a file that names what it needs from a graph, the parameters
//! it takes, and a list of bounded steps: a walk from a node, a query that
//! reads, or an analysis a plugin offers. Running one gives findings that each
//! point at the records behind them, with the caps that applied and anything
//! that stopped the answer from being whole.
//!
//! What a run may do is fixed here, not by what the file says. The graph is
//! read through read transactions, so a query that writes is refused by the
//! engine. Every step has a cap, and no step runs once `cancel` is set. A graph
//! without the labels or types a template needs is refused before any step
//! runs: an empty answer from the wrong graph would read as "nothing found".
//!
//! The same template, parameters and graph contents give the same findings, IDs
//! and order. Nothing here knows any provider's labels or rules: those are in
//! the template files and in plugins. See `docs/templates.md`.

use crate::engine::{Graph, QueryError, QueryOptions};
use crate::evidence::{self, Confidence, Evidence, Field, PathEvidence, SourceCoverage};
use crate::explore::{self, is_identifier, node_name, Direction, ExploreRequest};
use crate::investigation::{check_header, FileError, Fingerprint};
use crate::merge::ID_PROPERTY;
use crate::plugin_analysis;
use crate::protocol::HopRef;
use crate::result::{GraphCollector, GraphData};
use crate::settings::{RunStatus, Source};
use crate::util::sha256_hex;
use crate::Language;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const FORMAT: &str = "khonsu-template";
pub const VERSION: u32 = 1;
/// How a template file's name ends. A file is judged by its contents, not this.
pub const SUFFIX: &str = ".template.json";
/// Findings kept in a run unless asked otherwise; the rest are counted.
pub const DEFAULT_MAX_FINDINGS: usize = 200;
/// The most findings a run may be asked to keep.
pub const MAX_FINDINGS: usize = 5000;
pub const DEFAULT_MAX_ROWS: usize = 200;
/// The most rows a query step may ask for.
pub const MAX_ROWS: usize = 5000;
const MAX_STEPS: usize = 32;

/// What kind of value a parameter takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    /// A node, by its stable `khonsu_id`. It must be in the graph.
    Node,
    String,
    Integer,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub kind: ParameterKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Used when the parameter is not given. Without one it is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Json>,
}

/// What a graph must hold for the template's answers to mean anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationship_types: Vec<String>,
}

/// A number written in the file, or `$name` for an integer parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Amount {
    Fixed(u64),
    Parameter(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepKind {
    /// A bounded walk from node parameters. Each node it reaches is a finding,
    /// with the fewest-hops path the walk took to it.
    Walk {
        /// `$name` of each node parameter to start from.
        from: Vec<String>,
        #[serde(default = "out")]
        direction: Direction,
        /// Relationship types to follow; empty follows every type.
        #[serde(default)]
        types: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_hops: Option<Amount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_nodes: Option<Amount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_edges: Option<Amount>,
        /// Only nodes with one of these labels are findings; empty keeps all.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
    },
    /// A Cypher query that reads. Each row is a finding.
    Query {
        cypher: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_rows: Option<Amount>,
        /// The column that names what a row is about: a node, or a `khonsu_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource_column: Option<String>,
    },
    /// An analysis a plugin offers. Its findings are checked by the host.
    Analysis {
        plugin: String,
        analysis: String,
        /// `$name` of a node parameter: only findings about that node.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
        /// Only findings with this verdict, as the analysis names it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_verdict: Option<String>,
    },
}

fn out() -> Direction {
    Direction::Out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The rule a finding of this step is filed under. Defaults to
    /// `<template id>.<step id>`. An analysis step keeps the plugin's rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// What a finding of this step is called, e.g. `candidate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// What a finding of this step assumes, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    #[serde(flatten)]
    pub kind: StepKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub format: String,
    pub version: u32,
    /// A plain name, e.g. `blast-radius`.
    pub id: String,
    /// The template's own version. A change to what it asks should change this.
    pub revision: String,
    pub name: String,
    /// What it asks, as a person would.
    pub question: String,
    /// What its findings do and do not show.
    pub claim: String,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    pub steps: Vec<Step>,
}

fn invalid(message: impl Into<String>) -> FileError {
    FileError { kind: "invalid", message: message.into() }
}

/// A template's ID: letters, digits, `-`, `_` and `.`, starting with a letter.
fn is_template_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The parameter a `$name` reference names.
fn reference(text: &str) -> Option<&str> {
    text.strip_prefix('$').filter(|name| is_identifier(name))
}

/// Every `$name` in a query's text, outside quoted strings.
fn parameters_in(cypher: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut chars = cypher.char_indices().peekable();
    let mut quote: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        match quote {
            Some(open) => {
                if c == '\\' {
                    chars.next();
                } else if c == open {
                    quote = None;
                }
            }
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '$' => {
                let rest = &cypher[at + 1..];
                let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
                if is_identifier(&rest[..end]) {
                    names.insert(rest[..end].to_string());
                }
            }
            None => {}
        }
    }
    names
}

impl Template {
    /// Reads a file, saying what is wrong with one it cannot use.
    pub fn load(path: &Path) -> Result<Self, FileError> {
        let shown = path.display();
        let text = std::fs::read_to_string(path).map_err(|error| FileError {
            kind: if error.kind() == std::io::ErrorKind::NotFound { "not_found" } else { "io" },
            message: format!("cannot read {shown}: {error}"),
        })?;
        Self::parse(&text).map_err(|error| FileError { kind: error.kind, message: format!("{shown}: {}", error.message) })
    }

    pub fn parse(text: &str) -> Result<Self, FileError> {
        let value = check_header(text, FORMAT, VERSION)?;
        let template: Template = serde_json::from_value(value).map_err(|error| invalid(format!("its contents are not usable: {error}")))?;
        template.validate()?;
        Ok(template)
    }

    /// A hash of what the template says, however its file was laid out.
    pub fn digest(&self) -> String {
        sha256_hex(serde_json::to_string(self).expect("a template serializes").as_bytes())
    }

    fn parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|parameter| parameter.name == name)
    }

    /// Checks everything that can be checked without a graph.
    fn validate(&self) -> Result<(), FileError> {
        if !is_template_id(&self.id) {
            return Err(invalid(format!("{:?} is not a template ID: use letters, digits, '-', '_' and '.'", self.id)));
        }
        if self.revision.trim().is_empty() {
            return Err(invalid("it has no revision"));
        }
        if self.question.trim().is_empty() || self.claim.trim().is_empty() {
            return Err(invalid("it must say what it asks (question) and what its findings show (claim)"));
        }
        let names = self.requires.node_labels.iter().chain(&self.requires.relationship_types);
        if let Some(bad) = names.into_iter().find(|name| !is_identifier(name)) {
            return Err(invalid(format!("it requires {bad:?}, which is not a label or type name")));
        }
        let mut seen = BTreeSet::new();
        for parameter in &self.parameters {
            if !is_identifier(&parameter.name) {
                return Err(invalid(format!("{:?} is not a parameter name", parameter.name)));
            }
            if !seen.insert(parameter.name.as_str()) {
                return Err(invalid(format!("parameter {} is declared twice", parameter.name)));
            }
            if let Some(default) = &parameter.default {
                coerce(parameter, default).map_err(|message| invalid(format!("the default of {message}")))?;
            }
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(invalid(format!("it must have between 1 and {MAX_STEPS} steps, not {}", self.steps.len())));
        }
        let mut seen = BTreeSet::new();
        for step in &self.steps {
            if !is_identifier(&step.id) {
                return Err(invalid(format!("{:?} is not a step ID", step.id)));
            }
            if !seen.insert(step.id.as_str()) {
                return Err(invalid(format!("step {} is there twice", step.id)));
            }
            self.validate_step(step).map_err(|message| invalid(format!("step {}: {message}", step.id)))?;
        }
        Ok(())
    }

    fn validate_step(&self, step: &Step) -> Result<(), String> {
        let node = |text: &str| match reference(text).and_then(|name| self.parameter(name)) {
            Some(parameter) if parameter.kind == ParameterKind::Node => Ok(()),
            Some(parameter) => Err(format!("{text} is a {:?} parameter, and a node is needed", parameter.kind).to_lowercase()),
            None => Err(format!("{text:?} does not name a declared parameter: write $name")),
        };
        let amount = |amount: &Option<Amount>, most: u64, what: &str| match amount {
            Some(Amount::Fixed(number)) if *number == 0 || *number > most => Err(format!("{what} must be between 1 and {most}, not {number}")),
            Some(Amount::Parameter(text)) => match reference(text).and_then(|name| self.parameter(name)) {
                Some(parameter) if parameter.kind == ParameterKind::Integer => Ok(()),
                _ => Err(format!("{what} is {text:?}, which is not a number or an integer parameter")),
            },
            _ => Ok(()),
        };
        match &step.kind {
            StepKind::Walk { from, types, max_hops, max_nodes, max_edges, labels, .. } => {
                if from.is_empty() {
                    return Err("a walk needs a node parameter to start from".into());
                }
                from.iter().try_for_each(|text| node(text))?;
                if let Some(bad) = types.iter().chain(labels).find(|name| !is_identifier(name)) {
                    return Err(format!("{bad:?} is not a label or type name"));
                }
                amount(max_hops, u64::from(explore::MAX_HOPS), "max_hops")?;
                amount(max_nodes, explore::MAX_NODES as u64, "max_nodes")?;
                amount(max_edges, explore::MAX_EDGES as u64, "max_edges")
            }
            StepKind::Query { cypher, max_rows, .. } => {
                if cypher.trim().is_empty() {
                    return Err("its query is empty".into());
                }
                if let Some(unknown) = parameters_in(cypher).into_iter().find(|name| self.parameter(name).is_none()) {
                    return Err(format!("its query uses ${unknown}, which is not a declared parameter"));
                }
                amount(max_rows, MAX_ROWS as u64, "max_rows")
            }
            StepKind::Analysis { plugin, analysis, resource, .. } => {
                if plugin.trim().is_empty() || analysis.trim().is_empty() {
                    return Err("it must name a plugin and an analysis".into());
                }
                // The plugin's findings carry its own rules and verdicts.
                if step.verdict.is_some() || step.rule_id.is_some() {
                    return Err("an analysis step keeps the plugin's rules and verdicts: use only_verdict to keep one verdict".into());
                }
                resource.as_deref().map_or(Ok(()), node)
            }
        }
    }

    /// The plugins its analysis steps run.
    pub fn plugins(&self) -> Vec<String> {
        let plugins: BTreeSet<String> = self
            .steps
            .iter()
            .filter_map(|step| match &step.kind {
                StepKind::Analysis { plugin, .. } => Some(plugin.clone()),
                _ => None,
            })
            .collect();
        plugins.into_iter().collect()
    }
}

/// `value` as `parameter` takes it, or what is wrong with it.
fn coerce(parameter: &Parameter, value: &Json) -> Result<Json, String> {
    let name = &parameter.name;
    match (parameter.kind, value) {
        (ParameterKind::Node | ParameterKind::String, Json::String(text)) if !text.is_empty() => Ok(value.clone()),
        // `-p id=42` reads as a number, and an ID can be made of digits.
        (ParameterKind::Node | ParameterKind::String, Json::Number(number)) => Ok(json!(number.to_string())),
        (ParameterKind::Integer, Json::Number(number)) if number.is_i64() => Ok(value.clone()),
        (ParameterKind::Boolean, Json::Bool(_)) => Ok(value.clone()),
        (kind, other) => Err(format!("{name} must be {}, not {other}", match kind {
            ParameterKind::Node => "a node's khonsu_id",
            ParameterKind::String => "text",
            ParameterKind::Integer => "a whole number",
            ParameterKind::Boolean => "true or false",
        })),
    }
}

/// A template found on disk, or one that could not be read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listed {
    pub file: PathBuf,
    /// Where it was found: `user`, `plugin:<id>` or `folder`.
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<Summary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a template is, without its steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub question: String,
    pub claim: String,
    pub digest: String,
    pub requires: Requires,
    pub plugins: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub steps: Vec<String>,
}

impl Template {
    pub fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            revision: self.revision.clone(),
            name: self.name.clone(),
            question: self.question.clone(),
            claim: self.claim.clone(),
            digest: self.digest(),
            requires: self.requires.clone(),
            plugins: self.plugins(),
            parameters: self.parameters.clone(),
            steps: self.steps.iter().map(|step| step.id.clone()).collect(),
        }
    }
}

/// The templates in `folders`, each `(origin, folder)`, in the order given and
/// by file name within one. A file that is not a usable template is listed with
/// what is wrong, so a mistake in one does not hide it.
pub fn list_in(folders: &[(String, PathBuf)]) -> Vec<Listed> {
    let mut listed = Vec::new();
    for (origin, folder) in folders {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(SUFFIX))).collect();
        files.sort();
        for file in files {
            let (template, error) = match Template::load(&file) {
                Ok(template) => (Some(template.summary()), None),
                Err(error) => (None, Some(error.message)),
            };
            listed.push(Listed { file, origin: origin.clone(), template, error });
        }
    }
    listed
}

/// Where templates are looked for: the user's own folder, then each plugin's
/// `templates` folder. A plugin's templates are files, never run as programs.
pub fn folders() -> Vec<(String, PathBuf)> {
    let mut folders = vec![("user".to_string(), crate::settings::config_dir().join("templates"))];
    folders.extend(crate::plugin::list().into_iter().map(|plugin| (format!("plugin:{}", plugin.id()), plugin.dir.join("templates"))));
    folders
}

/// What a graph lacks that a template needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Missing {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationship_types: Vec<String>,
}

impl Missing {
    pub fn is_empty(&self) -> bool {
        self.node_labels.is_empty() && self.relationship_types.is_empty()
    }

    fn words(&self) -> String {
        let mut parts = Vec::new();
        if !self.node_labels.is_empty() {
            parts.push(format!("no nodes labeled {}", self.node_labels.join(", ")));
        }
        if !self.relationship_types.is_empty() {
            parts.push(format!("no relationships of type {}", self.relationship_types.join(", ")));
        }
        parts.join(" and ")
    }
}

impl Graph {
    /// What `template` needs that this graph does not hold.
    pub fn missing_for(&mut self, template: &Template) -> Result<Missing, QueryError> {
        let schema = self.schema()?;
        let labels: BTreeSet<&str> = schema.labels.iter().filter(|label| label.count > 0).map(|label| label.name.as_str()).collect();
        let types: BTreeSet<&str> = schema.relationship_types.iter().filter(|kind| kind.count > 0).map(|kind| kind.name.as_str()).collect();
        Ok(Missing {
            node_labels: template.requires.node_labels.iter().filter(|label| !labels.contains(label.as_str())).cloned().collect(),
            relationship_types: template.requires.relationship_types.iter().filter(|kind| !types.contains(kind.as_str())).cloned().collect(),
        })
    }
}

/// Whether a finding is what a source reported, or was worked out from that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// A row of what the graph holds, as a query returned it.
    Observation,
    /// Worked out by a walk or a rule from records the graph holds.
    Derived,
}

/// The path behind a finding, by reference. Its records are in the run's
/// `evidence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingPath {
    pub evidence_id: String,
    pub derivation: String,
    pub confidence: Field<Confidence>,
    pub claim: String,
    /// Each hop in words, in the order walked.
    pub hops: Vec<String>,
    /// The evidence IDs it rests on, start to end.
    pub supporting: Vec<String>,
    /// The same path as `khonsu investigation save --path-refs` takes it.
    pub refs: Vec<HopRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The same template, parameters and records give the same ID.
    pub finding_id: String,
    pub step: String,
    pub rule_id: String,
    pub verdict: Field<String>,
    pub class: Class,
    /// The evidence ID of what it is about, and what people call it.
    pub resource: Field<String>,
    pub resource_name: Field<String>,
    /// The finding in a line.
    pub summary: String,
    /// A query row's values by column, with secrets masked and records named
    /// by their evidence IDs.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub values: Map<String, Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<FindingPath>,
    /// A plugin's conditions, as the host checked them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<plugin_analysis::Condition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknowns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// Every record it rests on, by evidence ID. Each is in the run's `evidence`.
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    /// It could not run: `error` says why. Its findings are missing, not none.
    Failed,
    /// Not run, or stopped partway, because the run was cancelled.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepError {
    pub kind: String,
    pub message: String,
}

/// What one step did: the caps it ran under, and whether they cut anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: StepStatus,
    /// The caps it ran under.
    pub caps: Map<String, Json>,
    /// Caps that were reached, so there may be more than was returned.
    pub limits: Vec<String>,
    /// Findings it produced, counting any the run's cap then dropped.
    pub findings: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<StepError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// When it started, in Unix seconds, and how long it took. Not part of what
    /// makes two runs the same.
    pub started_at: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub template: Summary,
    pub graph: PathBuf,
    /// What the graph held when this ran.
    pub fingerprint: Fingerprint,
    /// How much of each source behind the graph its last sync covered.
    pub sources: BTreeMap<String, Field<SourceCoverage>>,
    /// The parameters as used, with defaults filled in and secrets masked.
    pub parameters: Map<String, Json>,
    /// False when a step failed or was cancelled, a cap was reached, findings
    /// were cut, or a source's last sync was not complete: a finding that is
    /// not here then means nothing.
    pub complete: bool,
    /// Why it is not complete, in words.
    pub gaps: Vec<String>,
    /// The steps that failed, by ID.
    pub failed: Vec<String>,
    pub cancelled: bool,
    pub steps: Vec<StepOutcome>,
    /// Findings by step.
    pub summary: BTreeMap<String, usize>,
    /// The most findings it was asked to keep, and how many that left out.
    pub max_findings: usize,
    pub truncated: usize,
    pub findings: Vec<Finding>,
    /// Every record a finding rests on, by evidence ID.
    pub evidence: Vec<Evidence>,
}

impl Run {
    pub fn finding_ids(&self) -> BTreeSet<&str> {
        self.findings.iter().map(|finding| finding.finding_id.as_str()).collect()
    }
}

/// Why a run did not start.
#[derive(Debug, Clone, PartialEq)]
pub struct RunError {
    /// `usage`, `not_found`, `unsupported_source`, or a query error's kind.
    pub kind: String,
    pub message: String,
    pub detail: Json,
}

impl RunError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into(), detail: Json::Null }
    }
}

impl From<QueryError> for RunError {
    fn from(error: QueryError) -> Self {
        Self { kind: error.kind.clone(), message: error.message.clone(), detail: Json::Null }
    }
}

/// A parameter's value, and the node it names when it names one.
struct Bound {
    value: Json,
    node: Option<i64>,
}

struct Runner<'a> {
    template: &'a Template,
    sources: &'a [Source],
    cancel: &'a AtomicBool,
    bound: BTreeMap<String, Bound>,
    evidence: BTreeMap<String, Evidence>,
    gaps: Vec<String>,
}

/// What a step gives back before its outcome is filled in.
#[derive(Default)]
struct Produced {
    findings: Vec<Finding>,
    limits: Vec<String>,
    warnings: Vec<String>,
    cancelled: bool,
}

fn finding_id(parts: &[&str]) -> String {
    format!("finding:{:016x}", evidence::fnv1a(&parts.join("\n")))
}

fn path_of(path: PathEvidence, graph: &GraphData, hops: &[explore::Hop]) -> FindingPath {
    let refs = hops
        .iter()
        .filter_map(|hop| graph.edges.iter().find(|edge| edge.id == hop.edge).map(|edge| HopRef { edge: evidence::edge_evidence_id(edge), along: hop.along }))
        .collect();
    FindingPath {
        evidence_id: path.evidence_id,
        derivation: path.derivation,
        confidence: path.confidence,
        claim: path.claim,
        hops: path.hops,
        supporting: path.supporting,
        refs,
    }
}

impl Runner<'_> {
    fn keep(&mut self, records: impl IntoIterator<Item = Evidence>) -> Vec<String> {
        records
            .into_iter()
            .map(|record| {
                let id = record.evidence_id.clone();
                self.evidence.entry(id.clone()).or_insert(record);
                id
            })
            .collect()
    }

    fn amount(&self, amount: &Option<Amount>, default: u64) -> u64 {
        match amount {
            None => default,
            Some(Amount::Fixed(number)) => *number,
            Some(Amount::Parameter(text)) => reference(text).and_then(|name| self.bound.get(name)).and_then(|bound| bound.value.as_u64()).unwrap_or(default),
        }
    }

    fn node(&self, text: &str) -> Option<i64> {
        reference(text).and_then(|name| self.bound.get(name)).and_then(|bound| bound.node)
    }

    fn rule(&self, step: &Step) -> String {
        step.rule_id.clone().unwrap_or_else(|| format!("{}.{}", self.template.id, step.id))
    }

    fn caps(&self, step: &Step) -> Map<String, Json> {
        let mut caps = Map::new();
        match &step.kind {
            StepKind::Walk { max_hops, max_nodes, max_edges, .. } => {
                caps.insert("max_hops".into(), json!(self.amount(max_hops, u64::from(explore::DEFAULT_MAX_HOPS))));
                caps.insert("max_nodes".into(), json!(self.amount(max_nodes, explore::DEFAULT_MAX_NODES as u64)));
                caps.insert("max_edges".into(), json!(self.amount(max_edges, explore::DEFAULT_MAX_EDGES as u64)));
            }
            StepKind::Query { max_rows, .. } => {
                caps.insert("max_rows".into(), json!(self.amount(max_rows, DEFAULT_MAX_ROWS as u64)));
            }
            StepKind::Analysis { .. } => {
                caps.insert("timeout_seconds".into(), json!(crate::plugin::ANALYZE_TIMEOUT.as_secs()));
            }
        }
        caps
    }

    fn walk(&mut self, graph: &mut Graph, step: &Step) -> Result<Produced, StepError> {
        let StepKind::Walk { from, direction, types, max_hops, max_nodes, max_edges, labels } = &step.kind else { unreachable!() };
        let request = ExploreRequest {
            starts: from.iter().filter_map(|text| self.node(text)).collect(),
            direction: *direction,
            rel_types: types.clone(),
            max_hops: self.amount(max_hops, u64::from(explore::DEFAULT_MAX_HOPS)) as u32,
            max_nodes: self.amount(max_nodes, explore::DEFAULT_MAX_NODES as u64) as usize,
            max_edges: self.amount(max_edges, explore::DEFAULT_MAX_EDGES as u64) as usize,
        };
        let walk = graph.explore(&request, self.cancel).map_err(step_error)?;
        let rule = self.rule(step);
        let mut reached: Vec<(u32, String, Finding)> = Vec::new();
        for node in walk.graph.nodes.iter().filter(|node| !walk.starts.contains(&node.id)) {
            if !labels.is_empty() && !node.labels.iter().any(|label| labels.contains(label)) {
                continue;
            }
            let Some(hops) = walk.path_to(node.id).filter(|hops| !hops.is_empty()) else { continue };
            let hops: Vec<explore::Hop> = hops.into_iter().cloned().collect();
            let Some(path) = evidence::for_path(&walk, node.id, self.sources) else { continue };
            let resource = evidence::node_evidence_id(node);
            let finding = Finding {
                finding_id: finding_id(&[&self.template.id, &step.id, &rule, &resource, &path.evidence_id]),
                step: step.id.clone(),
                rule_id: rule.clone(),
                verdict: step.verdict.clone().into(),
                class: Class::Derived,
                resource: Field::Known(resource.clone()),
                resource_name: Field::Known(node_name(node)),
                summary: path.hops.join("; "),
                values: Map::new(),
                evidence_ids: self.keep(path.evidence.clone()),
                path: Some(path_of(path, &walk.graph, &hops)),
                conditions: Vec::new(),
                unknowns: Vec::new(),
                assumptions: step.assumptions.clone(),
            };
            reached.push((hops.len() as u32, resource, finding));
        }
        // Nearest first, then by what it is: the order a walk read them in is
        // the engine's, and can differ between two graphs holding the same data.
        reached.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Ok(Produced {
            findings: reached.into_iter().map(|(_, _, finding)| finding).collect(),
            limits: walk.limits.iter().map(|limit| format!("{limit:?}").to_lowercase()).collect(),
            warnings: Vec::new(),
            cancelled: walk.cancelled,
        })
    }

    fn query(&mut self, graph: &mut Graph, step: &Step) -> Result<Produced, StepError> {
        let StepKind::Query { cypher, max_rows, resource_column } = &step.kind else { unreachable!() };
        let max_rows = self.amount(max_rows, DEFAULT_MAX_ROWS as u64) as usize;
        let params: Map<String, Json> = parameters_in(cypher).into_iter().filter_map(|name| self.bound.get(&name).map(|bound| (name, bound.value.clone()))).collect();
        let result = graph.run_read(Language::Cypher, cypher, &params, QueryOptions { max_rows }).map_err(step_error)?;
        if let Some(column) = resource_column.as_ref().filter(|column| !result.columns.contains(column)) {
            return Err(StepError { kind: "invalid".into(), message: format!("its query returns no column {column:?} to say what a row is about") });
        }
        let rule = self.rule(step);
        let mut produced = Produced::default();
        if result.truncated {
            produced.limits.push("rows".into());
        }
        let mut seen = BTreeSet::new();
        for row in &result.rows {
            let mut collector = GraphCollector::default();
            row.iter().for_each(|value| collector.visit(value));
            let records = collector.finish().unwrap_or_default();
            let mut values = Map::new();
            let mut resource = None;
            for (column, value) in result.columns.iter().zip(row) {
                // A whole record in a cell is named by its ID: what it holds is
                // in the evidence.
                let mut one = GraphCollector::default();
                let whole = value.as_object().filter(|map| map.contains_key("_id")).is_some();
                if whole {
                    one.visit(value);
                }
                let named = one.finish().and_then(|data| data.nodes.first().map(evidence::node_evidence_id).or_else(|| data.edges.first().map(evidence::edge_evidence_id)));
                let about = Some(column) == resource_column.as_ref();
                match named {
                    Some(id) => {
                        if about {
                            resource = records.nodes.iter().find(|node| evidence::node_evidence_id(node) == id).cloned();
                        }
                        values.insert(column.clone(), json!(id));
                    }
                    None => {
                        if about {
                            let stable = value.as_str().map(String::from).or_else(|| value.as_i64().map(|number| number.to_string()));
                            if let Some(stable) = stable {
                                resource = self.node_named(graph, &stable).map_err(step_error)?;
                            }
                        }
                        values.insert(column.clone(), evidence::masked(column, value).0);
                    }
                }
            }
            let mut cited: Vec<Evidence> = records.nodes.iter().map(|node| evidence::for_node(node, self.sources)).collect();
            // A relationship says where it starts and ends even when the row
            // did not return those nodes. They are named, not cited.
            let mut ends = records.nodes.clone();
            let beyond = records.dangling_endpoints();
            if !beyond.is_empty() {
                ends.extend(graph.nodes_by_id(&beyond).map_err(step_error)?.nodes);
            }
            cited.extend(records.edges.iter().map(|edge| evidence::for_edge(edge, &ends, self.sources)));
            if let Some(node) = &resource {
                cited.push(evidence::for_node(node, self.sources));
            }
            let about = resource.as_ref().map(evidence::node_evidence_id);
            let row_text = Json::Object(values.clone()).to_string();
            let id = finding_id(&[&self.template.id, &step.id, &rule, about.as_deref().unwrap_or(""), &row_text]);
            // Two rows that say the same thing are one finding.
            if !seen.insert(id.clone()) {
                continue;
            }
            let mut evidence_ids = self.keep(cited);
            evidence_ids.sort();
            evidence_ids.dedup();
            produced.findings.push(Finding {
                finding_id: id,
                step: step.id.clone(),
                rule_id: rule.clone(),
                verdict: step.verdict.clone().into(),
                class: Class::Observation,
                resource_name: resource.as_ref().map(node_name).into(),
                resource: about.into(),
                summary: values.iter().map(|(column, value)| format!("{column}: {}", plain(value))).collect::<Vec<_>>().join(", "),
                values,
                path: None,
                conditions: Vec::new(),
                unknowns: Vec::new(),
                assumptions: step.assumptions.clone(),
                evidence_ids,
            });
        }
        if resource_column.is_some() && produced.findings.iter().any(|finding| finding.resource == Field::Unknown) {
            produced.warnings.push("Some rows name a resource that is not a node in the graph: those findings have no evidence behind them.".into());
        }
        Ok(produced)
    }

    fn node_named(&mut self, graph: &mut Graph, stable: &str) -> Result<Option<crate::GraphNode>, QueryError> {
        let params = Map::from_iter([("id".to_string(), json!(stable))]);
        let found = graph.run_read(Language::Cypher, &format!("MATCH (n {{{ID_PROPERTY}: $id}}) RETURN n LIMIT 1"), &params, QueryOptions::default())?;
        Ok(found.graph.and_then(|data| data.nodes.into_iter().next()))
    }

    fn analysis(&mut self, graph: &mut Graph, step: &Step) -> Result<Produced, StepError> {
        let StepKind::Analysis { plugin, analysis, resource, only_verdict: verdict } = &step.kind else { unreachable!() };
        let plugin_error = |error: crate::plugin::PluginError| StepError { kind: "plugin".into(), message: evidence::scrub(&error.to_string()) };
        let offered = plugin_analysis::find(plugin, analysis).map_err(plugin_error)?;
        if let Some(wanted) = verdict.as_ref().filter(|wanted| !offered.info.verdicts.iter().any(|verdict| verdict.id == **wanted)) {
            let names: Vec<&str> = offered.info.verdicts.iter().map(|verdict| verdict.id.as_str()).collect();
            return Err(StepError { kind: "invalid".into(), message: format!("no verdict {wanted:?}: this analysis has {}", names.join(", ")) });
        }
        let input = graph.read_analysis_input(&offered.info).map_err(step_error)?;
        let report = plugin_analysis::run(&offered, &input, self.sources, self.cancel, |_| {}).map_err(plugin_error)?;
        let about = match resource {
            Some(text) => {
                let id = self.node(text).unwrap_or_default();
                input.data.nodes.iter().find(|node| node.id == id).map(evidence::node_evidence_id).or(Some(String::new()))
            }
            None => None,
        };
        let mut produced = Produced::default();
        if !input.complete {
            produced.limits.push("records".into());
        }
        for found in report.findings {
            if about.as_ref().is_some_and(|about| found.resource != *about) || verdict.as_ref().is_some_and(|wanted| found.verdict != *wanted) {
                continue;
            }
            for source in &found.incomplete_sources {
                let gap = format!("Step {}: a finding rests on source {source}, whose last sync was not complete.", step.id);
                if !self.gaps.contains(&gap) {
                    self.gaps.push(gap);
                }
            }
            let path = found.path.clone().map(|path| {
                let refs = found.path_refs.clone().unwrap_or_default();
                FindingPath { refs, ..path_of(path, &GraphData::default(), &[]) }
            });
            let mut evidence_ids = self.keep(found.evidence.clone());
            evidence_ids.extend(self.keep(found.path.iter().flat_map(|path| path.evidence.clone())));
            evidence_ids.sort();
            evidence_ids.dedup();
            produced.findings.push(Finding {
                finding_id: finding_id(&[&self.template.id, &step.id, &found.finding_id]),
                step: step.id.clone(),
                rule_id: found.rule_id.clone(),
                verdict: Field::Known(found.verdict.clone()),
                class: Class::Derived,
                resource: Field::Known(found.resource.clone()),
                resource_name: Field::Known(found.resource_name.clone()),
                summary: format!("{}: {} ({})", found.resource_name, found.verdict, found.rule_id),
                values: Map::new(),
                path,
                conditions: found.conditions,
                unknowns: found.unknowns,
                assumptions: found.assumptions.into_iter().chain(step.assumptions.clone()).collect(),
                evidence_ids,
            });
        }
        Ok(produced)
    }
}

fn step_error(error: QueryError) -> StepError {
    StepError { kind: error.kind, message: evidence::scrub(&error.message) }
}

/// A value as it reads in a line of text.
fn plain(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Graph {
    /// Runs `template` with `given` parameters. A step that fails or is
    /// cancelled is reported in the run, and the others still run; only what
    /// keeps every step from meaning anything is an error.
    pub fn run_template(&mut self, template: &Template, given: &Map<String, Json>, max_findings: usize, sources: &[Source], cancel: &AtomicBool) -> Result<Run, RunError> {
        if max_findings == 0 || max_findings > MAX_FINDINGS {
            return Err(RunError::new("usage", format!("findings to keep must be between 1 and {MAX_FINDINGS}, not {max_findings}")));
        }
        if let Some(unknown) = given.keys().find(|name| template.parameter(name).is_none()) {
            let names: Vec<&str> = template.parameters.iter().map(|parameter| parameter.name.as_str()).collect();
            let takes = if names.is_empty() { "takes no parameters".to_string() } else { format!("takes {}", names.join(", ")) };
            return Err(RunError::new("usage", format!("template {} has no parameter {unknown:?}: it {takes}", template.id)));
        }
        let missing = self.missing_for(template)?;
        if !missing.is_empty() {
            return Err(RunError {
                kind: "unsupported_source".into(),
                message: format!("this graph has {}, which template {} needs: its answers would mean nothing here", missing.words(), template.id),
                detail: json!({ "missing": missing }),
            });
        }

        let mut bound = BTreeMap::new();
        let mut shown = Map::new();
        for parameter in &template.parameters {
            let value = match given.get(&parameter.name).or(parameter.default.as_ref()) {
                Some(value) => coerce(parameter, value).map_err(|message| RunError::new("usage", format!("parameter {message}")))?,
                None => return Err(RunError::new("usage", format!("template {} needs parameter {}: pass -p {}=...", template.id, parameter.name, parameter.name))),
            };
            let node = match parameter.kind {
                ParameterKind::Node => {
                    let stable = value.as_str().unwrap_or_default();
                    let params = Map::from_iter([("id".to_string(), json!(stable))]);
                    let found = self.run_read(Language::Cypher, &format!("MATCH (n {{{ID_PROPERTY}: $id}}) RETURN id(n) AS id LIMIT 1"), &params, QueryOptions::default())?;
                    let id = found.rows.first().and_then(|row| row.first()).and_then(Json::as_i64);
                    Some(id.ok_or_else(|| RunError::new("not_found", format!("parameter {}: no node has the khonsu_id {stable:?}", parameter.name)))?)
                }
                _ => None,
            };
            shown.insert(parameter.name.clone(), evidence::masked(&parameter.name, &value).0);
            bound.insert(parameter.name.clone(), Bound { value, node });
        }

        let fingerprint = self.fingerprint()?;
        let here = crate::engine::normalize_path(self.path());
        let of_this_graph = sources.iter().filter(|source| crate::engine::normalize_path(&source.graph) == here).map(|source| source.id.clone());
        let named: BTreeSet<String> = fingerprint.sources.iter().cloned().chain(of_this_graph).collect();
        let coverage: BTreeMap<String, Field<SourceCoverage>> = named.into_iter().map(|source| (source.clone(), evidence::coverage_of(&source, sources))).collect();

        let mut runner = Runner { template, sources, cancel, bound, evidence: BTreeMap::new(), gaps: Vec::new() };
        for (source, coverage) in &coverage {
            if let Some(coverage) = coverage.known().filter(|coverage| coverage.status != RunStatus::Complete) {
                runner.gaps.push(format!("Source {source}'s last sync ({}) was {}: its records can be out of date or missing.", coverage.run_id, coverage.status.word()));
            }
        }
        if fingerprint.hash.is_none() {
            runner.gaps.push("The graph is too large to fingerprint: a later run can only be compared by record counts.".into());
        }

        let mut steps = Vec::with_capacity(template.steps.len());
        let mut findings: Vec<Finding> = Vec::new();
        let mut cancelled = false;
        for step in &template.steps {
            let started = std::time::Instant::now();
            let mut outcome = StepOutcome {
                id: step.id.clone(),
                kind: match step.kind {
                    StepKind::Walk { .. } => "walk",
                    StepKind::Query { .. } => "query",
                    StepKind::Analysis { .. } => "analysis",
                }
                .into(),
                title: step.title.clone(),
                status: StepStatus::Ok,
                caps: runner.caps(step),
                limits: Vec::new(),
                findings: 0,
                error: None,
                warnings: Vec::new(),
                started_at: crate::util::unix_now(),
                elapsed_ms: 0,
            };
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                outcome.status = StepStatus::Cancelled;
                steps.push(outcome);
                continue;
            }
            let produced = match &step.kind {
                StepKind::Walk { .. } => runner.walk(self, step),
                StepKind::Query { .. } => runner.query(self, step),
                StepKind::Analysis { .. } => runner.analysis(self, step),
            };
            match produced {
                Ok(produced) => {
                    if produced.cancelled {
                        cancelled = true;
                        outcome.status = StepStatus::Cancelled;
                    }
                    for limit in &produced.limits {
                        runner.gaps.push(format!("Step {} reached its cap on {limit}: there may be more than it returned.", step.id));
                    }
                    outcome.limits = produced.limits;
                    outcome.warnings = produced.warnings;
                    outcome.findings = produced.findings.len();
                    findings.extend(produced.findings);
                }
                // A plugin stopped by the cancel flag says so in its own words.
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    cancelled = true;
                    outcome.status = StepStatus::Cancelled;
                }
                Err(error) => {
                    runner.gaps.push(format!("Step {} failed ({}): its findings are missing, not none.", step.id, error.message));
                    outcome.status = StepStatus::Failed;
                    outcome.error = Some(error);
                }
            }
            outcome.elapsed_ms = started.elapsed().as_millis() as u64;
            steps.push(outcome);
        }
        if cancelled {
            runner.gaps.push("The run was cancelled before every step finished.".into());
        }

        let summary = template.steps.iter().map(|step| (step.id.clone(), findings.iter().filter(|finding| finding.step == step.id).count())).collect();
        let truncated = findings.len().saturating_sub(max_findings);
        findings.truncate(max_findings);
        if truncated > 0 {
            runner.gaps.push(format!("{truncated} findings were left out by the cap of {max_findings}."));
        }
        // Only the records the kept findings rest on.
        let cited: BTreeSet<&str> = findings.iter().flat_map(|finding| finding.evidence_ids.iter().map(String::as_str)).collect();
        let evidence = runner.evidence.iter().filter(|(id, _)| cited.contains(id.as_str())).map(|(_, record)| record.clone()).collect();
        let failed: Vec<String> = steps.iter().filter(|step| step.status == StepStatus::Failed).map(|step| step.id.clone()).collect();
        Ok(Run {
            template: template.summary(),
            graph: self.path().to_path_buf(),
            fingerprint,
            sources: coverage,
            parameters: shown,
            complete: runner.gaps.is_empty(),
            gaps: runner.gaps,
            failed,
            cancelled,
            steps,
            summary,
            max_findings,
            truncated,
            findings,
            evidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(change: impl FnOnce(&mut Json)) -> Result<Template, FileError> {
        let mut value = json!({
            "format": FORMAT,
            "version": VERSION,
            "id": "blast-radius",
            "revision": "1",
            "name": "Blast radius",
            "question": "What depends on this?",
            "claim": "What the data says depends on it.",
            "requires": { "node_labels": ["Service"], "relationship_types": ["DEPENDS_ON"] },
            "parameters": [
                { "name": "service", "kind": "node" },
                { "name": "hops", "kind": "integer", "default": 4 },
            ],
            "steps": [
                { "id": "dependents", "kind": "walk", "from": ["$service"], "direction": "in", "types": ["DEPENDS_ON"], "max_hops": "$hops" },
                { "id": "alerts", "kind": "query", "cypher": "MATCH (a:Alert)-[:FIRED_ON]->(t {khonsu_id: $service}) RETURN a", "max_rows": 50 },
            ],
        });
        change(&mut value);
        Template::parse(&value.to_string())
    }

    #[test]
    fn a_template_round_trips_and_its_digest_follows_what_it_says() {
        let first = template(|_| {}).unwrap();
        let again = Template::parse(&serde_json::to_string_pretty(&first).unwrap()).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.digest(), again.digest());
        let other = template(|value| value["steps"][0]["direction"] = json!("out")).unwrap();
        assert_ne!(first.digest(), other.digest());
        assert_eq!(first.summary().steps, ["dependents", "alerts"]);
    }

    #[test]
    fn a_template_it_cannot_use_says_why() {
        let message = |change: fn(&mut Json)| template(change).unwrap_err().message;
        assert!(message(|v| v["id"] = json!("no spaces")).contains("not a template ID"));
        assert!(message(|v| v["claim"] = json!(" ")).contains("claim"));
        assert!(message(|v| v["steps"] = json!([])).contains("between 1 and"));
        assert!(message(|v| v["steps"][1]["id"] = json!("dependents")).contains("twice"));
        assert!(message(|v| v["steps"][0]["from"] = json!(["$nobody"])).contains("declared parameter"));
        assert!(message(|v| v["steps"][0]["from"] = json!(["$hops"])).contains("a node is needed"));
        assert!(message(|v| v["steps"][0]["types"] = json!(["A|B"])).contains("not a label or type name"));
        assert!(message(|v| v["steps"][0]["max_hops"] = json!(99)).contains("max_hops must be between"));
        assert!(message(|v| v["steps"][0]["max_hops"] = json!("$service")).contains("integer parameter"));
        assert!(message(|v| v["steps"][1]["cypher"] = json!("MATCH (n {id: $other}) RETURN n")).contains("$other"));
        assert!(message(|v| v["steps"][1]["max_rows"] = json!(1_000_000)).contains("max_rows"));
        assert!(message(|v| v["parameters"][1]["default"] = json!("four")).contains("whole number"));
        assert!(message(|v| v["requires"]["node_labels"] = json!(["a b"])).contains("not a label or type name"));
        let analysis = |extra: Json| {
            template(|v| {
                let mut step = json!({ "id": "rules", "kind": "analysis", "plugin": "aws", "analysis": "exposure" });
                step.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                v["steps"][1] = step;
            })
        };
        assert!(analysis(json!({ "only_verdict": "potential_exposure", "resource": "$service" })).is_ok());
        assert!(analysis(json!({ "verdict": "potential_exposure" })).unwrap_err().message.contains("only_verdict"));
        assert!(analysis(json!({ "resource": "$hops" })).unwrap_err().message.contains("a node is needed"));
        assert_eq!(template(|v| v["version"] = json!(VERSION + 1)).unwrap_err().kind, "too_new");
        assert_eq!(template(|v| v["format"] = json!("khonsu-investigation")).unwrap_err().kind, "invalid");
    }

    #[test]
    fn a_dollar_in_a_quoted_string_is_not_a_parameter() {
        assert_eq!(parameters_in("MATCH (n {id: $a}) WHERE n.cost = '$b' AND n.x = \"it's $c\" RETURN $d_1"), BTreeSet::from(["a".to_string(), "d_1".to_string()]));
        assert_eq!(parameters_in(r"WHERE n.name = 'a\'$b' RETURN $c"), BTreeSet::from(["c".to_string()]));
    }

    #[test]
    fn a_parameter_is_taken_as_its_kind_or_refused() {
        let parameter = |kind| Parameter { name: "p".into(), kind, description: String::new(), default: None };
        assert_eq!(coerce(&parameter(ParameterKind::Node), &json!(42)).unwrap(), json!("42"));
        assert_eq!(coerce(&parameter(ParameterKind::Integer), &json!(3)).unwrap(), json!(3));
        assert!(coerce(&parameter(ParameterKind::Integer), &json!(3.5)).is_err());
        assert!(coerce(&parameter(ParameterKind::Boolean), &json!("yes")).is_err());
        assert!(coerce(&parameter(ParameterKind::Node), &json!("")).is_err());
    }
}

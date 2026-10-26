//! The only module that talks to HawDB directly.
//!
//! Keeping engine access here means a HawDB upgrade touches one file.

use crate::lock::{self, LockError, LockOwner, WriterLock};
use crate::result::{json_params, value_to_json, GraphCollector, GraphData, QueryResult};
use crate::schema::{self, GraphSchema};
use hawdb::store::{GraphMutation, KernelWriteBatch, MatchedRelationshipCreate, NodeSetAssignment, NodeSetValue, PropertyFilter};
use hawdb::{Database, DatabaseConfig, HawDBError, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Mutations per kernel write batch, well under HawDB's 100,000 limit.
const KERNEL_BATCH: usize = 1000;
/// Preserve KhonsuGraph's read ceiling across HawDB releases. Since 0.10,
/// the engine's default session policy has no row-count limit.
const MAX_READ_RESULT_ROWS: usize = 100_000;

/// A node to upsert: its label, the key it is matched on, and the properties
/// to set whether it was created or found.
pub(crate) struct NodeUpsert {
    pub label: String,
    pub key: (String, Value),
    pub set: BTreeMap<String, Value>,
}

/// A relationship to create between two nodes known by internal ID.
pub(crate) struct RelationshipCreate {
    pub source_label: String,
    pub source: i64,
    pub rel_type: String,
    pub target_label: String,
    pub target: i64,
    pub properties: BTreeMap<String, Value>,
}

/// A file every HawDB project directory contains once created.
const HAWDB_MANIFEST: &str = "manifest.hawdb";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Cypher,
    Sql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{}", locked_message(.0))]
    Locked(Option<LockOwner>),
    #[error("{0} is not a HawDB graph directory (no {HAWDB_MANIFEST})")]
    NotAGraph(PathBuf),
    #[error("{0} already exists and is not empty; choose an empty or new directory")]
    NotEmpty(PathBuf),
    #[error("{0} does not exist")]
    Missing(PathBuf),
    /// Written by HawDB 0.8–0.11, whose format 0.12 rejects and does not
    /// migrate; `rebuild` makes it again from its data sources.
    #[error("{} was written by an older HawDB (0.8–0.11), which HawDB {} cannot read. Rebuild it from its data sources with `khonsu rebuild --db {}`; the old files are kept beside it", .0.display(), crate::HAWDB_VERSION, .0.display())]
    OldFormat(PathBuf),
    #[error("cannot open graph: {message}")]
    Engine { message: String, hint: Option<String> },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn locked_message(owner: &Option<LockOwner>) -> String {
    match owner {
        Some(owner) => format!(
            "the graph is open in {} (pid {}) since {}. HawDB allows one process per graph directory, so close it there first",
            owner.program,
            owner.pid,
            crate::util::format_unix_time(owner.since)
        ),
        None => "the graph is open in another process. HawDB allows one process per graph directory, so close it there first".into(),
    }
}

/// A query failure, with the source position when HawDB reports one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    /// HawDB's error category: parse, semantic, execution, read_budget, ...
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

impl QueryError {
    pub(crate) fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            line: None,
            column: None,
        }
    }
}

impl From<HawDBError> for QueryError {
    fn from(err: HawDBError) -> Self {
        let kind = match &err {
            HawDBError::Parse(_) => "parse",
            HawDBError::Semantic(_) => "semantic",
            HawDBError::Execution(_) => "execution",
            HawDBError::ReadBudgetExceeded(_) => "read_budget",
            HawDBError::TransactionConflict { .. } => "conflict",
            HawDBError::CapabilityUnavailable { .. } => "capability",
            HawDBError::BranchBusy { .. } => "busy",
            HawDBError::Storage(_) | HawDBError::StorageIntegrity(_) | HawDBError::FileDescriptors(_) => "storage",
            _ => "engine",
        };
        let message = err.to_string();
        // A write on a read-only database is the same refusal as
        // `ensure_writable`'s, which names the kind callers act on.
        if message.contains("opened in read-only mode") {
            return Self::new("read_only", "the query writes, but the graph is open read-only");
        }
        let (line, column) = error_position(&message);
        Self {
            kind: kind.into(),
            message,
            line,
            column,
        }
    }
}

/// Extracts `line N column M` (Cypher) or `Line: N, Column: M` (SQL).
pub fn error_position(message: &str) -> (Option<u32>, Option<u32>) {
    let lower = message.to_ascii_lowercase();
    let number_after = |key: &str| -> Option<u32> {
        let start = lower.find(key)? + key.len();
        let digits: String = lower[start..]
            .trim_start_matches([':', ' '])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    };
    (number_after("line"), number_after("column"))
}

#[derive(Debug, Clone, Copy)]
pub struct QueryOptions {
    /// Rows kept in the result; the rest are dropped and `truncated` is set.
    pub max_rows: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self { max_rows: 1000 }
    }
}

/// An open HawDB graph directory.
pub struct Graph {
    db: Database,
    path: PathBuf,
    mode: AccessMode,
    // Dropped after `db`, so the lock outlives the engine handle.
    _lock: Option<WriterLock>,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph").field("path", &self.path).field("mode", &self.mode).finish()
    }
}

/// True when `path` looks like an existing HawDB graph.
pub fn is_graph_dir(path: &Path) -> bool {
    path.join(HAWDB_MANIFEST).is_file()
}

/// Whether the graph at `path` is in the project format HawDB 0.8–0.11
/// wrote, which 0.12 rejects ("unknown or incomplete project identity
/// header") with no migration: HawDB keeps no compatibility for it.
pub fn is_old_format(path: &Path) -> bool {
    std::fs::read(path.join(HAWDB_MANIFEST)).is_ok_and(|bytes| bytes.starts_with(b"HAWDB_BRANCH_PROJECT_V1\n"))
}

impl Graph {
    /// Opens an existing graph. `program` names this process in lock messages.
    pub fn open(path: impl AsRef<Path>, mode: AccessMode, program: &str) -> Result<Self, OpenError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(OpenError::Missing(path.to_path_buf()));
        }
        if !is_graph_dir(path) {
            return Err(OpenError::NotAGraph(path.to_path_buf()));
        }
        if is_old_format(path) {
            return Err(OpenError::OldFormat(normalize_path(path)));
        }
        Self::open_inner(path, mode, program)
    }

    /// Creates a new, empty graph in a new or empty directory.
    pub fn create(path: impl AsRef<Path>, program: &str) -> Result<Self, OpenError> {
        let path = path.as_ref();
        if path.exists() && fs::read_dir(path)?.next().is_some() {
            return Err(OpenError::NotEmpty(path.to_path_buf()));
        }
        fs::create_dir_all(path)?;
        Self::open_inner(path, AccessMode::ReadWrite, program)
    }

    fn open_inner(path: &Path, mode: AccessMode, program: &str) -> Result<Self, OpenError> {
        let path = normalize_path(path);
        let lock = match mode {
            AccessMode::ReadWrite => Some(WriterLock::acquire(&path, program).map_err(|err| match err {
                LockError::Held(owner) => OpenError::Locked(owner),
                LockError::Io(err) => OpenError::Io(err),
            })?),
            // HawDB leases the directory to one process, readers included.
            AccessMode::ReadOnly if lock::is_locked(&path) => return Err(OpenError::Locked(lock::read_owner(&path))),
            AccessMode::ReadOnly => None,
        };
        let config = DatabaseConfig {
            read_only: mode == AccessMode::ReadOnly,
            max_read_result_rows: Some(MAX_READ_RESULT_ROWS),
            ..DatabaseConfig::default()
        };
        let db = Database::open_with_config(&path, config).map_err(|err| open_failure(&path, err))?;
        Ok(Self {
            db,
            path,
            mode,
            _lock: lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mode(&self) -> AccessMode {
        self.mode
    }

    pub fn run(
        &mut self,
        language: Language,
        text: &str,
        params: &Map<String, Json>,
        options: QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        let text = text.trim().trim_end_matches(';').trim();
        if text.is_empty() {
            return Err(QueryError::new("parse", "the query is empty"));
        }
        if language == Language::Cypher && starts_with_keyword(text, "EXPLAIN") {
            let plan = self.explain(language, &text["EXPLAIN".len()..])?;
            return Ok(plan_result(&plan));
        }
        let params = json_params(params);
        let started = Instant::now();
        let output = match language {
            Language::Cypher => self.db.query_with_params(text, &params),
            Language::Sql => self.db.query_sql_with_params(text, &positional(params)),
        }?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let columns: Vec<String> = output.schema().columns().iter().map(|c| c.to_string()).collect();
        let row_count = output.value_rows().count();
        let mut collector = GraphCollector::default();
        let rows: Vec<Vec<Json>> = output
            .value_rows()
            .take(options.max_rows)
            .map(|row| {
                row.iter()
                    .map(|value| {
                        let json = value_to_json(value);
                        collector.visit(&json);
                        json
                    })
                    .collect()
            })
            .collect();
        Ok(QueryResult {
            columns,
            truncated: rows.len() < row_count,
            row_count,
            rows,
            elapsed_ms,
            graph: collector.finish(),
        })
    }

    /// The physical plan HawDB chose, as its EXPLAIN table.
    pub fn explain(&mut self, language: Language, text: &str) -> Result<String, QueryError> {
        let text = text.trim().trim_end_matches(';').trim();
        match language {
            Language::Cypher => Ok(self.db.explain_query(text)?.to_string()),
            Language::Sql => {
                let output = self.db.query_sql(&format!("EXPLAIN {text}"))?;
                let mut plan = String::new();
                for row in output.value_rows() {
                    let cells: Vec<String> = row
                        .iter()
                        .map(|v| match v.clone() {
                            Value::String(s) => s,
                            other => value_to_json(&other).to_string(),
                        })
                        .collect();
                    plan.push_str(&cells.join("  "));
                    plan.push('\n');
                }
                Ok(plan)
            }
        }
    }

    pub fn schema(&mut self) -> Result<GraphSchema, QueryError> {
        schema::load(self)
    }

    /// Nodes by internal ID, for edge endpoints the result did not include.
    pub fn nodes_by_id(&mut self, ids: &[i64]) -> Result<GraphData, QueryError> {
        let mut collector = GraphCollector::default();
        for id in ids {
            let output = self
                .db
                .query_with_params("MATCH (n) WHERE id(n) = $id RETURN n", &BTreeMap::from([("id".to_string(), Value::Int(*id))]))?;
            for row in output.value_rows() {
                row.iter().for_each(|v| collector.visit(&value_to_json(v)));
            }
        }
        Ok(collector.finish().unwrap_or_default())
    }

    /// A node, its relationships in both directions and their other endpoints.
    pub fn neighbors(&mut self, id: i64, limit: usize) -> Result<GraphData, QueryError> {
        let query = format!("MATCH (n)-[r]-(m) WHERE id(n) = $id RETURN n, r, m LIMIT {limit}");
        let output = self.db.query_with_params(&query, &BTreeMap::from([("id".to_string(), Value::Int(id))]))?;
        let mut collector = GraphCollector::default();
        for row in output.value_rows() {
            row.iter().for_each(|v| collector.visit(&value_to_json(v)));
        }
        let mut data = collector.finish().unwrap_or_default();
        if data.nodes.is_empty() {
            data = self.nodes_by_id(&[id])?;
        }
        Ok(data)
    }

    /// Runs a parameterized Cypher statement and returns raw rows.
    pub(crate) fn raw_cypher(&mut self, text: &str, params: &BTreeMap<String, Value>) -> Result<Vec<Vec<Value>>, QueryError> {
        let output = self.db.query_with_params(text, params)?;
        Ok(output.value_rows().map(|row| row.iter().map(|v| v.clone()).collect()).collect())
    }

    /// Runs SQL and returns rows keyed by column name.
    pub(crate) fn raw_sql(&mut self, text: &str) -> Result<Vec<BTreeMap<String, Value>>, QueryError> {
        let output = self.db.query_sql(text)?;
        let columns: Vec<String> = output.schema().columns().iter().map(|c| c.to_string()).collect();
        Ok(output
            .value_rows()
            .map(|row| columns.iter().cloned().zip(row.iter().map(|v| v.clone())).collect())
            .collect())
    }

    /// Upserts nodes by one key property, in kernel write batches: no Cypher
    /// to parse or plan per node. Every property in `set` is written on create
    /// and on match alike.
    pub(crate) fn merge_nodes(&mut self, nodes: Vec<NodeUpsert>) -> Result<(), QueryError> {
        self.ensure_writable()?;
        for chunk in nodes.chunks(KERNEL_BATCH) {
            let graph = chunk
                .iter()
                .map(|node| GraphMutation::MergeNode {
                    label: node.label.clone(),
                    match_properties: BTreeMap::from([(node.key.0.clone(), node.key.1.clone())]),
                    on_create_properties: BTreeMap::new(),
                    on_match_assignments: Vec::new(),
                    post_merge_assignments: node
                        .set
                        .iter()
                        .map(|(property, value)| NodeSetAssignment {
                            property: property.clone(),
                            value: NodeSetValue::Value(value.clone()),
                        })
                        .collect(),
                })
                .collect();
            self.db.commit_kernel_write_batch(KernelWriteBatch {
                graph,
                ..KernelWriteBatch::default()
            })?;
        }
        Ok(())
    }

    /// Creates relationships between nodes given by internal ID.
    pub(crate) fn create_relationships(&mut self, relationships: Vec<RelationshipCreate>) -> Result<(), QueryError> {
        self.ensure_writable()?;
        for chunk in relationships.chunks(KERNEL_BATCH) {
            let graph = chunk
                .iter()
                .map(|rel| {
                    GraphMutation::CreateRelationshipsBetweenMatches(MatchedRelationshipCreate {
                        source_label: rel.source_label.clone(),
                        source_filter: Some(PropertyFilter::IdEq { value: Value::Int(rel.source) }),
                        rel_type: rel.rel_type.clone(),
                        rel_properties: rel.properties.clone(),
                        target_label: rel.target_label.clone(),
                        target_filter: Some(PropertyFilter::IdEq { value: Value::Int(rel.target) }),
                    })
                })
                .collect();
            self.db.commit_kernel_write_batch(KernelWriteBatch {
                graph,
                ..KernelWriteBatch::default()
            })?;
        }
        Ok(())
    }

    fn ensure_writable(&self) -> Result<(), QueryError> {
        if self.mode == AccessMode::ReadOnly {
            return Err(QueryError::new("read_only", "the graph is open read-only"));
        }
        Ok(())
    }

    /// Runs `statements` in one transaction; nothing is written if one fails.
    pub(crate) fn write_batch(&mut self, statements: &[(String, BTreeMap<String, Value>)]) -> Result<Vec<Vec<Vec<Value>>>, QueryError> {
        if self.mode == AccessMode::ReadOnly {
            return Err(QueryError::new("read_only", "the graph is open read-only"));
        }
        let mut tx = self.db.begin_transaction()?;
        let mut results = Vec::with_capacity(statements.len());
        for (text, params) in statements {
            let output = tx.query_with_params(text, params)?;
            results.push(output.value_rows().map(|row| row.iter().map(|v| v.clone()).collect()).collect());
        }
        tx.commit()?;
        Ok(results)
    }
}

/// SQL takes `$1, $2, ...`; parameters are given as `{"1": .., "2": ..}`.
fn positional(params: BTreeMap<String, Value>) -> Vec<Value> {
    let mut numbered: Vec<(usize, Value)> = params
        .into_iter()
        .filter_map(|(k, v)| k.trim_start_matches('$').parse().ok().map(|i| (i, v)))
        .collect();
    numbered.sort_by_key(|(i, _)| *i);
    numbered.into_iter().map(|(_, v)| v).collect()
}

fn starts_with_keyword(text: &str, keyword: &str) -> bool {
    text.len() > keyword.len()
        && text[..keyword.len()].eq_ignore_ascii_case(keyword)
        && text[keyword.len()..].starts_with(char::is_whitespace)
}

fn plan_result(plan: &str) -> QueryResult {
    let rows: Vec<Vec<Json>> = plan.lines().map(|line| vec![Json::String(line.to_string())]).collect();
    QueryResult {
        columns: vec!["plan".into()],
        row_count: rows.len(),
        rows,
        truncated: false,
        elapsed_ms: 0,
        graph: None,
    }
}

/// The absolute, canonical form of a graph path, used everywhere a graph is
/// identified (settings, sources, locks). On Windows the `\\?\` prefix that
/// `canonicalize` adds is dropped; HawDB and users expect the plain form.
pub fn normalize_path(path: &Path) -> PathBuf {
    let path = path
        .canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) if !stripped.starts_with("UNC") => PathBuf::from(stripped),
        _ => path.to_path_buf(),
    }
}

fn open_failure(path: &Path, err: HawDBError) -> OpenError {
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("lease") || lower.contains("already open") || lower.contains("owner.hawdb.lock") {
        return OpenError::Locked(lock::read_owner(path));
    }
    let hint = if cfg!(windows) && (lower.contains("os error 5") || message.contains("拒绝访问")) {
        Some(
            "HawDB flushes every parent directory up to the drive root when opening a graph for writing. \
             On Windows this needs write access to those directories; a graph under a drive root you can \
             write to (e.g. D:\\graphs) works, or open read-only."
                .to_string(),
        )
    } else {
        None
    };
    OpenError::Engine { message, hint }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_read_from_cypher_and_sql_errors() {
        assert_eq!(
            error_position("parse error: unexpected trailing input at byte 30 line 1 column 31 near `x`"),
            (Some(1), Some(31))
        );
        assert_eq!(
            error_position("sql parser error: Expected: an SQL statement, found: SELEC at Line: 2, Column: 5"),
            (Some(2), Some(5))
        );
        assert_eq!(error_position("semantic error: unknown variable"), (None, None));
    }

    #[test]
    fn explain_keyword_needs_a_word_boundary() {
        assert!(starts_with_keyword("explain MATCH (n) RETURN n", "EXPLAIN"));
        assert!(!starts_with_keyword("EXPLAINED", "EXPLAIN"));
    }
}

//! A saved investigation: what someone was looking at, kept in a file of its
//! own, so that nothing but real records goes in the graph.
//!
//! The file names records by stable ID, since internal IDs can change when a
//! graph is rebuilt, and holds a fingerprint of the graph, so reopening can say
//! when the data has moved on instead of showing a new answer under an old
//! name. It holds no source configuration or credentials. See
//! `docs/investigations.md`.

use crate::engine::{Graph, QueryError};
use crate::evidence::{self, PathEvidence};
use crate::explore::{Direction, Exploration, ExploreRequest};
use crate::merge::{ID_PROPERTY, PLUGIN_PROPERTY, SOURCE_PROPERTY, SYNCED_AT_PROPERTY};
use crate::result::{value_to_json, GraphCollector, GraphData, GraphNode};
use crate::settings::Source;
use crate::util::unix_now;
use hawdb::Value;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const FORMAT: &str = "khonsu-investigation";
pub const VERSION: u32 = 1;
/// Above this many nodes or relationships a graph is counted, not hashed.
const MAX_HASHED_RECORDS: usize = 200_000;

/// A record, by its stable ID when it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    pub stable_id: Option<String>,
    /// Only trusted when there is no stable ID and the graph has not changed.
    pub internal_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    /// `cypher` or `sql`.
    pub language: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWalk {
    pub seeds: Vec<RecordRef>,
    pub direction: Direction,
    pub types: Vec<String>,
    pub max_hops: u32,
    pub max_nodes: usize,
    pub max_edges: usize,
}

/// The path someone had chosen, and the records it rested on then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPath {
    pub target: RecordRef,
    /// Evidence IDs, start to end. Empty when no path was found at saving.
    pub supporting: Vec<String>,
}

/// What a graph held, without timestamps: a re-sync that changes nothing gives
/// the same fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub nodes: u64,
    pub relationships: u64,
    /// `None` when the graph was too large to hash, so only counts compare.
    pub hash: Option<String>,
    /// Data sources with records in the graph.
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Investigation {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub graph: PathBuf,
    pub saved_at: u64,
    pub fingerprint: Fingerprint,
    #[serde(default)]
    pub query: Option<SavedQuery>,
    #[serde(default)]
    pub walk: Option<SavedWalk>,
    #[serde(default)]
    pub path: Option<SavedPath>,
    #[serde(default)]
    pub dashboard: Option<PathBuf>,
}

/// Why a file could not be used. `kind` is `not_found`, `invalid`, `too_new`,
/// `exists` or `io`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub kind: &'static str,
    pub message: String,
}

impl Investigation {
    /// Reads a file, saying what is wrong with one it cannot use. Fields it
    /// does not know are ignored, so a later release can add some.
    pub fn load(path: &Path) -> Result<Self, FileError> {
        let shown = path.display();
        let text = std::fs::read_to_string(path).map_err(|error| FileError {
            kind: if error.kind() == std::io::ErrorKind::NotFound { "not_found" } else { "io" },
            message: format!("cannot read {shown}: {error}"),
        })?;
        Self::parse(&text).map_err(|error| FileError {
            kind: error.kind,
            message: format!("{shown}: {}", error.message),
        })
    }

    pub fn parse(text: &str) -> Result<Self, FileError> {
        let invalid = |message: String| FileError { kind: "invalid", message };
        let value: Json = serde_json::from_str(text).map_err(|error| invalid(format!("not valid JSON: {error}")))?;
        if value.get("format").and_then(Json::as_str) != Some(FORMAT) {
            return Err(invalid(format!("not a saved investigation: its format is not {FORMAT:?}")));
        }
        let version = value.get("version").and_then(Json::as_u64).ok_or_else(|| invalid("it has no version number".into()))?;
        if version > u64::from(VERSION) {
            return Err(FileError {
                kind: "too_new",
                message: format!("it is version {version}, and this build reads up to version {VERSION}; update KhonsuGraph"),
            });
        }
        serde_json::from_value(value).map_err(|error| invalid(format!("its contents are not usable: {error}")))
    }

    /// Writes the file whole or not at all. An existing file is kept unless
    /// `overwrite` says otherwise.
    pub fn save(&self, path: &Path, overwrite: bool) -> Result<(), FileError> {
        let io = |error: std::io::Error| FileError {
            kind: "io",
            message: format!("cannot write {}: {error}", path.display()),
        };
        if !overwrite && path.exists() {
            return Err(FileError {
                kind: "exists",
                message: format!("{} already exists", path.display()),
            });
        }
        let text = serde_json::to_string_pretty(self).expect("an investigation serializes") + "\n";
        let mut scratch = path.as_os_str().to_owned();
        scratch.push(".tmp");
        let scratch = PathBuf::from(scratch);
        std::fs::write(&scratch, text).map_err(io)?;
        std::fs::rename(&scratch, path).map_err(|error| {
            let _ = std::fs::remove_file(&scratch);
            io(error)
        })
    }
}

/// What to save, by internal IDs as the caller has them.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    pub name: String,
    pub query: Option<SavedQuery>,
    pub walk: Option<ExploreRequest>,
    /// Needs `walk`: the node whose path was chosen.
    pub target: Option<i64>,
    pub dashboard: Option<PathBuf>,
}

/// Whether the graph still holds what it did at saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphState {
    Unchanged,
    Changed,
    /// The counts agree, and the graph is too large to hash.
    NotCompared,
}

/// What came back on reopening, with everything that differs from the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reopened {
    pub graph_state: GraphState,
    /// What changed, in words. Not exhaustive: a hash says only that something did.
    pub differences: Vec<String>,
    pub now: Fingerprint,
    /// Saved starting points that are not in the graph now.
    pub missing_seeds: Vec<RecordRef>,
    /// The saved path's end is not in the graph now.
    pub target_missing: bool,
    /// Records found by an internal ID on a changed graph, which may now be
    /// something else.
    pub unsure: Vec<RecordRef>,
    pub walk: Option<Exploration>,
    pub target: Option<i64>,
    pub path: Option<PathEvidence>,
    /// Whether the path rests on the saved records. `None` when none was saved.
    pub path_same: Option<bool>,
}

impl Graph {
    /// The fingerprint of everything in the graph.
    pub fn fingerprint(&mut self) -> Result<Fingerprint, QueryError> {
        let params = BTreeMap::new();
        let count = |rows: Vec<Vec<Value>>| rows.first().and_then(|row| row.first()).map(value_to_json).and_then(|value| value.as_u64()).unwrap_or(0);
        let nodes = count(self.raw_cypher("MATCH (n) RETURN count(n) AS c", &params)?);
        let relationships = count(self.raw_cypher("MATCH ()-[r]->() RETURN count(r) AS c", &params)?);
        if nodes as usize > MAX_HASHED_RECORDS || relationships as usize > MAX_HASHED_RECORDS {
            return Ok(Fingerprint {
                nodes,
                relationships,
                hash: None,
                sources: Vec::new(),
            });
        }
        let mut collector = GraphCollector::default();
        for text in ["MATCH (n) RETURN n", "MATCH ()-[r]->() RETURN r"] {
            for row in self.raw_cypher(text, &params)? {
                row.iter().for_each(|value| collector.visit(&value_to_json(value)));
            }
        }
        let data = collector.finish().unwrap_or_default();
        let (hash, sources) = content_hash(&data);
        Ok(Fingerprint {
            nodes,
            relationships,
            hash: Some(hash),
            sources,
        })
    }

    /// Records what `capture` describes, as it stands now.
    pub fn capture_investigation(&mut self, graph: &Path, capture: Capture, sources: &[Source], cancel: &AtomicBool) -> Result<Investigation, QueryError> {
        let refuse = |message: String| QueryError::new("semantic", message);
        if capture.target.is_some() && capture.walk.is_none() {
            return Err(refuse("a path to a node needs a walk to find it: name where to start".into()));
        }
        let mut walk = None;
        let mut path = None;
        if let Some(request) = &capture.walk {
            let found = self.explore(request, cancel)?;
            if let Some(start) = found.missing_starts.first() {
                return Err(refuse(format!("no node has the ID {start}")));
            }
            let seeds = self.references(&request.starts)?;
            if let Some(target) = capture.target {
                let refs = self.references(&[target])?;
                let target_ref = refs.into_iter().next().ok_or_else(|| refuse(format!("no node has the ID {target}")))?;
                let supporting = evidence::for_path(&found, target, sources).map(|record| record.supporting).unwrap_or_default();
                path = Some(SavedPath { target: target_ref, supporting });
            }
            walk = Some(SavedWalk {
                seeds,
                direction: request.direction,
                types: request.rel_types.clone(),
                max_hops: request.max_hops,
                max_nodes: request.max_nodes,
                max_edges: request.max_edges,
            });
        }
        Ok(Investigation {
            format: FORMAT.into(),
            version: VERSION,
            name: capture.name,
            graph: graph.to_path_buf(),
            saved_at: unix_now(),
            fingerprint: self.fingerprint()?,
            query: capture.query,
            walk,
            path,
            dashboard: capture.dashboard,
        })
    }

    /// Restores what can be restored, and says what could not. A record that
    /// has gone, or a graph that has changed, is reported and not an error.
    pub fn reopen_investigation(&mut self, saved: &Investigation, sources: &[Source], cancel: &AtomicBool) -> Result<Reopened, QueryError> {
        let now = self.fingerprint()?;
        let (graph_state, differences) = compare(&saved.fingerprint, &now);

        let mut missing_seeds = Vec::new();
        let mut unsure = Vec::new();
        let mut starts = Vec::new();
        for seed in saved.walk.iter().flat_map(|walk| &walk.seeds) {
            match self.resolve(seed)? {
                Some(id) => {
                    starts.push(id);
                    if seed.stable_id.is_none() && graph_state != GraphState::Unchanged {
                        unsure.push(seed.clone());
                    }
                }
                None => missing_seeds.push(seed.clone()),
            }
        }
        let target = match &saved.path {
            Some(path) => {
                let found = self.resolve(&path.target)?;
                if found.is_some() && path.target.stable_id.is_none() && graph_state != GraphState::Unchanged {
                    unsure.push(path.target.clone());
                }
                found
            }
            None => None,
        };
        let target_missing = saved.path.is_some() && target.is_none();

        let mut walk = None;
        if let Some(plan) = saved.walk.as_ref().filter(|_| !starts.is_empty()) {
            let request = ExploreRequest {
                starts,
                direction: plan.direction,
                rel_types: plan.types.clone(),
                max_hops: plan.max_hops,
                max_nodes: plan.max_nodes,
                max_edges: plan.max_edges,
            };
            walk = Some(self.explore(&request, cancel)?);
        }
        let path = walk.as_ref().zip(target).and_then(|(walk, target)| evidence::for_path(walk, target, sources));
        let path_same = saved.path.as_ref().map(|saved_path| {
            let now: &[String] = path.as_ref().map_or(&[], |path| path.supporting.as_slice());
            saved_path.supporting == now
        });
        Ok(Reopened {
            graph_state,
            differences,
            now,
            missing_seeds,
            target_missing,
            unsure,
            walk,
            target,
            path,
            path_same,
        })
    }

    fn references(&mut self, ids: &[i64]) -> Result<Vec<RecordRef>, QueryError> {
        let found = self.nodes_by_id(ids)?;
        Ok(ids
            .iter()
            .filter_map(|id| found.nodes.iter().find(|node| node.id == *id))
            .map(|node| RecordRef {
                stable_id: node.properties.get(ID_PROPERTY).and_then(Json::as_str).map(String::from),
                internal_id: node.id,
            })
            .collect())
    }

    /// The node a reference names now, if any.
    fn resolve(&mut self, record: &RecordRef) -> Result<Option<i64>, QueryError> {
        match &record.stable_id {
            Some(stable) => {
                let params = BTreeMap::from([("id".to_string(), Value::String(stable.clone()))]);
                let rows = self.raw_cypher(&format!("MATCH (n {{{ID_PROPERTY}: $id}}) RETURN id(n) AS id LIMIT 1"), &params)?;
                Ok(rows.first().and_then(|row| row.first()).map(value_to_json).and_then(|value| value.as_i64()))
            }
            None => {
                let found = self.nodes_by_id(&[record.internal_id])?;
                Ok(found.nodes.first().map(|node| node.id))
            }
        }
    }
}

fn compare(saved: &Fingerprint, now: &Fingerprint) -> (GraphState, Vec<String>) {
    let mut differences = Vec::new();
    if saved.nodes != now.nodes {
        differences.push(format!("Nodes: {} when saved, {} now.", saved.nodes, now.nodes));
    }
    if saved.relationships != now.relationships {
        differences.push(format!("Relationships: {} when saved, {} now.", saved.relationships, now.relationships));
    }
    if let (Some(saved_hash), Some(now_hash)) = (&saved.hash, &now.hash) {
        let was: BTreeSet<&String> = saved.sources.iter().collect();
        let is: BTreeSet<&String> = now.sources.iter().collect();
        differences.extend(was.difference(&is).map(|source| format!("Source {source} has no records now.")));
        differences.extend(is.difference(&was).map(|source| format!("Source {source} was not in the graph when saved.")));
        if saved_hash != now_hash && differences.is_empty() {
            differences.push("The same number of records, with different contents.".into());
        }
    }
    let state = if !differences.is_empty() {
        GraphState::Changed
    } else if saved.hash.is_some() && now.hash.is_some() {
        GraphState::Unchanged
    } else {
        GraphState::NotCompared
    };
    (state, differences)
}

/// A hash of what the records say, whatever order they are read in. Internal
/// IDs, sync times and plugin versions are left out: they differ between
/// graphs holding the same data.
fn content_hash(data: &GraphData) -> (String, Vec<String>) {
    let mut sources = BTreeSet::new();
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    let mut lines = Vec::with_capacity(data.nodes.len() + data.edges.len());
    for node in &data.nodes {
        if let Some(source) = node.properties.get(SOURCE_PROPERTY).and_then(Json::as_str) {
            sources.insert(source.to_string());
        }
        let line = node_line(node);
        names.insert(node.id, line.clone());
        lines.push(line);
    }
    for edge in &data.edges {
        if let Some(source) = edge.properties.get(SOURCE_PROPERTY).and_then(Json::as_str) {
            sources.insert(source.to_string());
        }
        let end = |id: i64| names.get(&id).map_or("?", String::as_str);
        lines.push(format!("{}|{}|{}|{}", end(edge.source), edge.rel_type, end(edge.target), canonical(&edge.properties)));
    }
    lines.sort_unstable();
    (format!("{:016x}", evidence::fnv1a(&lines.join("\n"))), sources.into_iter().collect())
}

fn node_line(node: &GraphNode) -> String {
    let mut labels = node.labels.clone();
    labels.sort_unstable();
    format!("{}|{}", labels.join(":"), canonical(&node.properties))
}

/// Properties as text with keys in order, without what changes on a re-sync.
fn canonical(properties: &Map<String, Json>) -> String {
    fn sorted(value: &Json) -> Json {
        match value {
            Json::Object(map) => {
                let ordered: BTreeMap<&String, Json> = map.iter().map(|(key, item)| (key, sorted(item))).collect();
                Json::Object(ordered.into_iter().map(|(key, item)| (key.clone(), item)).collect())
            }
            Json::Array(items) => Json::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let kept: Map<String, Json> = properties
        .iter()
        .filter(|(key, _)| key.as_str() != SYNCED_AT_PROPERTY && key.as_str() != PLUGIN_PROPERTY)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    sorted(&Json::Object(kept)).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::GraphEdge;
    use serde_json::json;

    fn saved() -> Investigation {
        Investigation {
            format: FORMAT.into(),
            version: VERSION,
            name: "web tier".into(),
            graph: PathBuf::from("graphs/shop"),
            saved_at: 1000,
            fingerprint: Fingerprint { nodes: 1, relationships: 0, hash: Some("abc".into()), sources: vec!["shop".into()] },
            query: None,
            walk: None,
            path: None,
            dashboard: None,
        }
    }

    fn with(field: &str, value: Json) -> String {
        let mut text = serde_json::to_value(saved()).unwrap();
        text[field] = value;
        text.to_string()
    }

    #[test]
    fn a_file_round_trips_and_ignores_fields_it_does_not_know() {
        let text = serde_json::to_string(&saved()).unwrap();
        assert_eq!(Investigation::parse(&text).unwrap(), saved());
        assert_eq!(Investigation::parse(&with("from_a_later_release", json!(1))).unwrap(), saved());
    }

    #[test]
    fn a_file_it_cannot_use_says_why() {
        let kind = |text: &str| Investigation::parse(text).unwrap_err().kind;
        assert_eq!(kind("{not json"), "invalid");
        assert_eq!(kind("{}"), "invalid");
        assert_eq!(kind(&with("format", json!("something-else"))), "invalid");
        assert_eq!(kind(&with("version", json!("one"))), "invalid");
        assert_eq!(kind(&with("fingerprint", json!(7))), "invalid");
        let newer = Investigation::parse(&with("version", json!(VERSION + 1))).unwrap_err();
        assert_eq!(newer.kind, "too_new");
        assert!(newer.message.contains("update"), "{}", newer.message);
    }

    fn data(extra: Json) -> GraphData {
        let node = |id: i64, stable: &str, extra: Json| {
            let mut properties = Map::from_iter([
                ("khonsu_id".to_string(), json!(stable)),
                ("khonsu_source".to_string(), json!("shop")),
                ("khonsu_synced_at".to_string(), json!(id * 10)),
                ("khonsu_plugin".to_string(), json!("aws@0.1.0")),
            ]);
            properties.extend(extra.as_object().unwrap().clone());
            GraphNode { id, labels: vec!["Host".into()], properties }
        };
        GraphData {
            nodes: vec![node(1, "a", extra), node(2, "b", json!({}))],
            edges: vec![GraphEdge {
                id: 9,
                source: 1,
                target: 2,
                rel_type: "NEXT".into(),
                properties: Map::from_iter([("khonsu_id".to_string(), json!("a|b"))]),
            }],
        }
    }

    #[test]
    fn the_hash_follows_what_records_say_not_how_they_were_stored() {
        let base = content_hash(&data(json!({"name": "web"}))).0;

        // Other internal IDs, times, plugin versions, property order and read order.
        let mut moved = data(json!({"name": "web"}));
        moved.nodes.reverse();
        for node in &mut moved.nodes {
            node.id += 100;
            node.properties.insert("khonsu_synced_at".into(), json!(99_999));
            node.properties.insert("khonsu_plugin".into(), json!("aws@9.9.9"));
        }
        moved.edges[0].source += 100;
        moved.edges[0].target += 100;
        assert_eq!(content_hash(&moved).0, base);

        // A changed value, a new property, a changed label and a reversed relationship.
        assert_ne!(content_hash(&data(json!({"name": "db"}))).0, base);
        assert_ne!(content_hash(&data(json!({"name": "web", "port": 80}))).0, base);
        let mut relabelled = data(json!({"name": "web"}));
        relabelled.nodes[0].labels = vec!["Other".into()];
        assert_ne!(content_hash(&relabelled).0, base);
        let mut reversed = data(json!({"name": "web"}));
        let edge = &mut reversed.edges[0];
        std::mem::swap(&mut edge.source, &mut edge.target);
        assert_ne!(content_hash(&reversed).0, base);

        assert_eq!(content_hash(&data(json!({}))).1, ["shop"]);
    }

    fn print(nodes: u64, hash: Option<&str>, sources: &[&str]) -> Fingerprint {
        Fingerprint {
            nodes,
            relationships: 2,
            hash: hash.map(String::from),
            sources: sources.iter().map(|source| source.to_string()).collect(),
        }
    }

    #[test]
    fn comparing_says_what_it_can_and_never_calls_an_uncompared_graph_unchanged() {
        let a = print(5, Some("x"), &["shop"]);
        assert_eq!(compare(&a, &a), (GraphState::Unchanged, vec![]));

        let (state, differences) = compare(&a, &print(5, Some("y"), &["shop"]));
        assert_eq!(state, GraphState::Changed);
        assert_eq!(differences, ["The same number of records, with different contents."]);

        let (state, differences) = compare(&a, &print(6, Some("y"), &["other"]));
        assert_eq!(state, GraphState::Changed);
        assert_eq!(differences.len(), 3, "{differences:?}");

        // Too large to hash: equal counts prove nothing, different ones do.
        assert_eq!(compare(&print(5, None, &[]), &print(5, None, &[])).0, GraphState::NotCompared);
        assert_eq!(compare(&a, &print(5, None, &[])).0, GraphState::NotCompared);
        assert_eq!(compare(&print(5, None, &[]), &print(6, None, &[])).0, GraphState::Changed);
    }

    #[test]
    fn saving_keeps_an_existing_file_unless_told_otherwise() {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let path = dir.path().join("web.investigation");
        saved().save(&path, false).unwrap();
        assert_eq!(Investigation::load(&path).unwrap(), saved());

        let mut other = saved();
        other.name = "another".into();
        assert_eq!(other.save(&path, false).unwrap_err().kind, "exists");
        assert_eq!(Investigation::load(&path).unwrap().name, "web tier");
        other.save(&path, true).unwrap();
        assert_eq!(Investigation::load(&path).unwrap().name, "another");
        assert!(!dir.path().join("web.investigation.tmp").exists());

        assert_eq!(Investigation::load(&dir.path().join("none")).unwrap_err().kind, "not_found");
    }
}

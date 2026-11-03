//! A snapshot: the records of a graph at one moment, kept in a file by stable
//! IDs, so that two can be compared long after the graph has moved on. HawDB has
//! no time travel, and this does not add any.
//!
//! A record gone from a later snapshot counts as removed only when its source
//! was read completely for that snapshot. Otherwise it may simply not have been
//! seen, and is listed apart. See `docs/snapshots.md`.

use crate::engine::{Graph, QueryError};
use crate::evidence::{self, Field, SourceCoverage};
use crate::investigation::{check_header, compare, content_hash, write_whole, FileError, Fingerprint, GraphState};
use crate::merge::{ID_PROPERTY, SOURCE_PROPERTY};
use crate::result::GraphData;
use crate::settings::{RunStatus, Source};
use crate::util::unix_now;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const FORMAT: &str = "khonsu-snapshot";
pub const VERSION: u32 = 1;
/// Above this many nodes or relationships a graph is refused, not cut short: a
/// snapshot missing records would read as removals later.
pub const MAX_RECORDS: usize = 200_000;
/// Changes listed unless asked otherwise; the rest are counted.
pub const DEFAULT_MAX_CHANGES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: String,
    pub labels: Vec<String>,
    pub source: Option<String>,
    /// Its own properties, with anything secret-looking masked.
    pub properties: Map<String, Json>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipRecord {
    pub id: String,
    pub rel_type: String,
    pub from: String,
    pub to: String,
    pub source: Option<String>,
    pub properties: Map<String, Json>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted: Vec<String>,
}

/// What is known about a source at the moment of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceState {
    pub coverage: Field<SourceCoverage>,
    /// `<plugin id>@<plugin version>`: what mapped the source to these records.
    pub plugin: Field<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: String,
    pub version: u32,
    /// `snapshot:<digest>`: the same records give the same ID.
    pub snapshot_id: String,
    pub captured_at: u64,
    pub fingerprint: Fingerprint,
    pub sources: BTreeMap<String, SourceState>,
    /// Sorted by ID.
    pub nodes: Vec<NodeRecord>,
    pub relationships: Vec<RelationshipRecord>,
    /// Records with no stable ID, or with an end that has none. They cannot be
    /// matched between snapshots, so they are counted and not kept.
    pub unstable_nodes: u64,
    pub unstable_relationships: u64,
}

impl Snapshot {
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
        let value = check_header(text, FORMAT, VERSION)?;
        serde_json::from_value(value).map_err(|error| FileError {
            kind: "invalid",
            message: format!("its contents are not usable: {error}"),
        })
    }

    pub fn save(&self, path: &Path, overwrite: bool) -> Result<(), FileError> {
        let text = serde_json::to_string_pretty(self).expect("a snapshot serializes") + "\n";
        write_whole(path, &text, overwrite)
    }

    /// Whether `graph` still holds what this snapshot does.
    pub fn check(&self, graph: &mut Graph) -> Result<(GraphState, Vec<String>), QueryError> {
        Ok(compare(&self.fingerprint, &graph.fingerprint()?))
    }

    /// Sources whose last sync at this moment was known not to be complete.
    fn incomplete_sources(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|(_, state)| state.coverage.known().is_some_and(|coverage| coverage.status != RunStatus::Complete))
            .map(|(source, _)| source.clone())
            .collect()
    }

    /// Whether a record of `source` missing from this snapshot has really gone.
    fn confirms_removal(&self, source: Option<&str>) -> bool {
        let state = source.and_then(|source| self.sources.get(source));
        state.and_then(|state| state.coverage.known()).is_some_and(|coverage| coverage.status == RunStatus::Complete)
    }
}

impl Graph {
    /// The graph's records as they stand. A graph too large to hold is refused.
    pub fn snapshot(&mut self, sources: &[Source]) -> Result<Snapshot, QueryError> {
        self.snapshot_capped(sources, MAX_RECORDS)
    }

    pub(crate) fn snapshot_capped(&mut self, sources: &[Source], cap: usize) -> Result<Snapshot, QueryError> {
        let fingerprint = self.fingerprint()?;
        let too_large = fingerprint.nodes as usize > cap || fingerprint.relationships as usize > cap;
        if too_large {
            return Err(QueryError::new(
                "too_large",
                format!("the graph has {} nodes and {} relationships, and a snapshot holds at most {cap} of each", fingerprint.nodes, fingerprint.relationships),
            ));
        }
        let data = self.read_all_records()?;
        Ok(from_records(&data, fingerprint, sources))
    }
}

fn from_records(data: &GraphData, fingerprint: Fingerprint, sources: &[Source]) -> Snapshot {
    let text = |properties: &Map<String, Json>, key: &str| properties.get(key).and_then(Json::as_str).filter(|value| !value.is_empty()).map(String::from);
    let stable: BTreeMap<i64, String> = data.nodes.iter().filter_map(|node| Some((node.id, text(&node.properties, ID_PROPERTY)?))).collect();

    let mut nodes = Vec::new();
    let mut unstable_nodes = 0;
    for node in &data.nodes {
        let Some(id) = stable.get(&node.id) else {
            unstable_nodes += 1;
            continue;
        };
        let (properties, mut redacted) = evidence::own_properties(&node.properties);
        redacted.sort_unstable();
        let mut labels = node.labels.clone();
        labels.sort_unstable();
        nodes.push(NodeRecord {
            id: id.clone(),
            labels,
            source: text(&node.properties, SOURCE_PROPERTY),
            properties: sorted(properties),
            redacted,
        });
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut relationships = Vec::new();
    let mut unstable_relationships = 0;
    for edge in &data.edges {
        let (Some(id), Some(from), Some(to)) = (text(&edge.properties, ID_PROPERTY), stable.get(&edge.source), stable.get(&edge.target)) else {
            unstable_relationships += 1;
            continue;
        };
        let (properties, mut redacted) = evidence::own_properties(&edge.properties);
        redacted.sort_unstable();
        relationships.push(RelationshipRecord {
            id,
            rel_type: edge.rel_type.clone(),
            from: from.clone(),
            to: to.clone(),
            source: text(&edge.properties, SOURCE_PROPERTY),
            properties: sorted(properties),
            redacted,
        });
    }
    relationships.sort_by(|a, b| a.id.cmp(&b.id));

    let states = fingerprint
        .sources
        .iter()
        .map(|source| {
            let run = sources.iter().find(|configured| configured.id == *source).and_then(Source::last_run);
            let state = SourceState {
                coverage: evidence::coverage_of(source, sources),
                plugin: run.map(|run| run.plugin.clone()).into(),
            };
            (source.clone(), state)
        })
        .collect();
    let digest = fingerprint.hash.clone().unwrap_or_else(|| content_hash(data).0);
    Snapshot {
        format: FORMAT.into(),
        version: VERSION,
        snapshot_id: format!("snapshot:{digest}"),
        captured_at: unix_now(),
        fingerprint,
        sources: states,
        nodes,
        relationships,
        unstable_nodes,
        unstable_relationships,
    }
}

fn sorted(properties: Map<String, Json>) -> Map<String, Json> {
    let ordered: BTreeMap<String, Json> = properties.into_iter().collect();
    ordered.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyChange {
    pub property: String,
    pub before: Option<Json>,
    pub after: Option<Json>,
}

/// One node or relationship in a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// A `khonsu_id`.
    pub id: String,
    /// A node's labels, or a relationship's type.
    pub types: Vec<String>,
    pub source: Option<String>,
    /// For a relationship: its ends, as they were last seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// For a changed record: what changed. Masked values compare as masked.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub properties: Vec<PropertyChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    pub added: Vec<Change>,
    /// Gone, and the source was read completely, so they really are.
    pub removed: Vec<Change>,
    /// Not seen, and the source was not known to be read completely. They may
    /// still exist.
    pub not_seen: Vec<Change>,
    pub changed: Vec<Change>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub added: usize,
    pub removed: usize,
    pub not_seen: usize,
    pub changed: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRef {
    pub snapshot_id: String,
    pub captured_at: u64,
    pub sources: BTreeMap<String, SourceState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    pub before: SnapshotRef,
    pub after: SnapshotRef,
    /// Counts every change, including any cut from the lists below.
    pub node_counts: Counts,
    pub relationship_counts: Counts,
    pub nodes: Changes,
    pub relationships: Changes,
    /// Changes left out of the lists by the cap.
    pub truncated: usize,
    /// What to bear in mind when reading this.
    pub warnings: Vec<String>,
}

fn property_changes(before: &Map<String, Json>, after: &Map<String, Json>) -> Vec<PropertyChange> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| PropertyChange {
            property: key.clone(),
            before: before.get(key).cloned(),
            after: after.get(key).cloned(),
        })
        .collect()
}

fn text_change(property: &str, before: &str, after: &str) -> Option<PropertyChange> {
    (before != after).then(|| PropertyChange {
        property: property.into(),
        before: Some(Json::String(before.into())),
        after: Some(Json::String(after.into())),
    })
}

/// What differs from `before` to `after`, by stable IDs, in ID order, listing
/// at most `max_changes` in all. The same two snapshots give the same result.
pub fn diff(before: &Snapshot, after: &Snapshot, max_changes: usize) -> Diff {
    let mut nodes = Changes::default();
    let was: BTreeMap<&str, &NodeRecord> = before.nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let is: BTreeMap<&str, &NodeRecord> = after.nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let node_change = |node: &NodeRecord, properties: Vec<PropertyChange>| Change {
        id: node.id.clone(),
        types: node.labels.clone(),
        source: node.source.clone(),
        from: None,
        to: None,
        properties,
    };
    for (id, old) in &was {
        match is.get(id) {
            None if after.confirms_removal(old.source.as_deref()) => nodes.removed.push(node_change(old, Vec::new())),
            None => nodes.not_seen.push(node_change(old, Vec::new())),
            Some(new) => {
                let mut changes = property_changes(&old.properties, &new.properties);
                changes.extend(text_change("labels", &old.labels.join(":"), &new.labels.join(":")));
                if !changes.is_empty() {
                    nodes.changed.push(node_change(new, changes));
                }
            }
        }
    }
    nodes.added = is.iter().filter(|(id, _)| !was.contains_key(*id)).map(|(_, node)| node_change(node, Vec::new())).collect();

    let mut relationships = Changes::default();
    let was: BTreeMap<&str, &RelationshipRecord> = before.relationships.iter().map(|edge| (edge.id.as_str(), edge)).collect();
    let is: BTreeMap<&str, &RelationshipRecord> = after.relationships.iter().map(|edge| (edge.id.as_str(), edge)).collect();
    let edge_change = |edge: &RelationshipRecord, properties: Vec<PropertyChange>| Change {
        id: edge.id.clone(),
        types: vec![edge.rel_type.clone()],
        source: edge.source.clone(),
        from: Some(edge.from.clone()),
        to: Some(edge.to.clone()),
        properties,
    };
    for (id, old) in &was {
        match is.get(id) {
            None if after.confirms_removal(old.source.as_deref()) => relationships.removed.push(edge_change(old, Vec::new())),
            None => relationships.not_seen.push(edge_change(old, Vec::new())),
            Some(new) => {
                let mut changes = property_changes(&old.properties, &new.properties);
                changes.extend(text_change("from", &old.from, &new.from));
                changes.extend(text_change("to", &old.to, &new.to));
                changes.extend(text_change("rel_type", &old.rel_type, &new.rel_type));
                if !changes.is_empty() {
                    relationships.changed.push(edge_change(new, changes));
                }
            }
        }
    }
    relationships.added = is.iter().filter(|(id, _)| !was.contains_key(*id)).map(|(_, edge)| edge_change(edge, Vec::new())).collect();

    let count = |changes: &Changes| Counts {
        added: changes.added.len(),
        removed: changes.removed.len(),
        not_seen: changes.not_seen.len(),
        changed: changes.changed.len(),
    };
    let (node_counts, relationship_counts) = (count(&nodes), count(&relationships));

    let mut left = max_changes;
    let mut truncated = 0;
    for list in [
        &mut nodes.added,
        &mut nodes.removed,
        &mut nodes.not_seen,
        &mut nodes.changed,
        &mut relationships.added,
        &mut relationships.removed,
        &mut relationships.not_seen,
        &mut relationships.changed,
    ] {
        let keep = list.len().min(left);
        truncated += list.len() - keep;
        list.truncate(keep);
        left -= keep;
    }

    let mut warnings = Vec::new();
    for source in after.incomplete_sources() {
        warnings.push(format!("Source {source}'s last sync was not complete in the later snapshot, so what it did not report is listed as not seen, not removed."));
    }
    for source in before.incomplete_sources() {
        warnings.push(format!("Source {source}'s last sync was not complete in the earlier snapshot, so some additions may have existed already."));
    }
    let unknown: Vec<&String> = after.sources.iter().filter(|(_, state)| state.coverage.known().is_none()).map(|(source, _)| source).collect();
    for source in unknown {
        warnings.push(format!("Nothing is known of how complete source {source}'s last sync was in the later snapshot, so what it did not report is listed as not seen."));
    }
    let gone = before.sources.keys().filter(|source| !after.sources.contains_key(*source));
    for source in gone {
        warnings.push(format!("Source {source} has no records in the later snapshot, so its records are listed as not seen."));
    }
    if before.captured_at > after.captured_at {
        warnings.push("The earlier snapshot was captured after the later one, so additions and removals read backwards.".into());
    }
    let unstable = before.unstable_nodes + before.unstable_relationships + after.unstable_nodes + after.unstable_relationships;
    if unstable > 0 {
        warnings.push(format!("{unstable} records have no stable ID and are not compared."));
    }
    let reference = |snapshot: &Snapshot| SnapshotRef {
        snapshot_id: snapshot.snapshot_id.clone(),
        captured_at: snapshot.captured_at,
        sources: snapshot.sources.clone(),
    };
    Diff {
        before: reference(before),
        after: reference(after),
        node_counts,
        relationship_counts,
        nodes,
        relationships,
        truncated,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{GraphEdge, GraphNode};
    use serde_json::json;

    fn node(id: i64, stable: &str, extra: Json) -> GraphNode {
        let mut properties = Map::from_iter([("khonsu_id".to_string(), json!(stable)), ("khonsu_source".to_string(), json!("shop"))]);
        properties.extend(extra.as_object().unwrap().clone());
        GraphNode { id, labels: vec!["Host".into()], properties }
    }

    fn edge(id: i64, source: i64, target: i64, stable: &str) -> GraphEdge {
        GraphEdge {
            id,
            source,
            target,
            rel_type: "LINKS".into(),
            properties: Map::from_iter([("khonsu_id".to_string(), json!(stable)), ("khonsu_source".to_string(), json!("shop"))]),
        }
    }

    fn source_with(status: Option<RunStatus>) -> Vec<Source> {
        let runs = status.map_or(json!([]), |status| {
            json!([{
                "run_id": "shop#1", "number": 1, "mode": "full", "started_at": 1, "ended_at": 2,
                "status": status, "plugin": "aws@0", "received_nodes": 0, "received_edges": 0,
                "removals_applied": false, "cursor_advanced": false,
            }])
        });
        vec![serde_json::from_value(json!({"id": "shop", "name": "shop", "plugin": "aws", "graph": "g", "runs": runs})).unwrap()]
    }

    fn snap(nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, status: Option<RunStatus>, at: u64) -> Snapshot {
        let data = GraphData { nodes, edges };
        let (hash, sources) = content_hash(&data);
        let fingerprint = Fingerprint {
            nodes: data.nodes.len() as u64,
            relationships: data.edges.len() as u64,
            hash: Some(hash),
            sources,
        };
        let mut snapshot = from_records(&data, fingerprint, &source_with(status));
        snapshot.captured_at = at;
        snapshot
    }

    fn before() -> Snapshot {
        snap(
            vec![node(1, "s/a", json!({"port": 80})), node(2, "s/b", json!({})), node(3, "s/c", json!({}))],
            vec![edge(10, 1, 2, "s/ab"), edge(11, 2, 3, "s/bc")],
            Some(RunStatus::Complete),
            100,
        )
    }

    fn after(status: Option<RunStatus>) -> Snapshot {
        // Other internal IDs: a is changed, c has gone, d is new.
        snap(
            vec![node(7, "s/a", json!({"port": 443})), node(8, "s/b", json!({})), node(9, "s/d", json!({}))],
            vec![edge(20, 7, 8, "s/ab"), edge(21, 8, 9, "s/bd")],
            status,
            200,
        )
    }

    fn ids(changes: &[Change]) -> Vec<&str> {
        changes.iter().map(|change| change.id.as_str()).collect()
    }

    #[test]
    fn a_diff_goes_by_stable_ids_and_reports_each_kind_of_change() {
        let result = diff(&before(), &after(Some(RunStatus::Complete)), DEFAULT_MAX_CHANGES);
        assert_eq!(ids(&result.nodes.added), ["s/d"]);
        assert_eq!(ids(&result.nodes.removed), ["s/c"]);
        assert!(result.nodes.not_seen.is_empty());
        assert_eq!(ids(&result.nodes.changed), ["s/a"]);
        assert_eq!(
            result.nodes.changed[0].properties,
            [PropertyChange { property: "port".into(), before: Some(json!(80)), after: Some(json!(443)) }]
        );
        assert_eq!(ids(&result.relationships.added), ["s/bd"]);
        assert_eq!(ids(&result.relationships.removed), ["s/bc"]);
        assert!(result.relationships.changed.is_empty(), "s/ab has other internal IDs, and is the same relationship");
        assert_eq!(result.relationships.removed[0].from.as_deref(), Some("s/b"));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(diff(&before(), &after(Some(RunStatus::Complete)), DEFAULT_MAX_CHANGES), result, "the same snapshots give the same diff");
    }

    #[test]
    fn nothing_changed_gives_an_empty_diff_whatever_the_internal_ids() {
        let result = diff(&before(), &before(), DEFAULT_MAX_CHANGES);
        assert_eq!(result.node_counts, Counts::default());
        assert_eq!(result.relationship_counts, Counts::default());
    }

    #[test]
    fn only_a_complete_later_sync_can_confirm_a_removal() {
        for status in [Some(RunStatus::Partial), Some(RunStatus::Failed), Some(RunStatus::Cancelled), None] {
            let result = diff(&before(), &after(status), DEFAULT_MAX_CHANGES);
            assert!(result.nodes.removed.is_empty(), "{status:?}");
            assert!(result.relationships.removed.is_empty(), "{status:?}");
            assert_eq!(ids(&result.nodes.not_seen), ["s/c"], "{status:?}");
            assert_eq!(ids(&result.relationships.not_seen), ["s/bc"], "{status:?}");
            assert!(result.warnings.iter().any(|text| text.contains("shop")), "{status:?}: {:?}", result.warnings);
            // Additions and changes are what the source did report.
            assert_eq!(ids(&result.nodes.added), ["s/d"], "{status:?}");
            assert_eq!(ids(&result.nodes.changed), ["s/a"], "{status:?}");
        }
    }

    #[test]
    fn a_source_gone_from_the_later_snapshot_confirms_nothing() {
        let empty = snap(vec![], vec![], None, 200);
        let result = diff(&before(), &empty, DEFAULT_MAX_CHANGES);
        assert!(result.nodes.removed.is_empty());
        assert_eq!(result.node_counts.not_seen, 3);
        assert!(result.warnings.iter().any(|text| text.contains("no records")), "{:?}", result.warnings);
    }

    #[test]
    fn the_lists_are_capped_and_the_counts_are_not() {
        let result = diff(&before(), &after(Some(RunStatus::Complete)), 2);
        let listed = result.nodes.added.len() + result.nodes.removed.len() + result.nodes.changed.len() + result.relationships.added.len() + result.relationships.removed.len();
        assert_eq!(listed, 2);
        assert_eq!(result.truncated, 3);
        assert_eq!(result.node_counts.added, 1);
        assert_eq!(result.relationship_counts.removed, 1);
    }

    #[test]
    fn records_without_stable_ids_are_counted_not_compared() {
        let mut bare = node(1, "x", json!({}));
        bare.properties.remove("khonsu_id");
        let snapshot = snap(vec![bare, node(2, "s/b", json!({}))], vec![edge(10, 1, 2, "s/ab")], Some(RunStatus::Complete), 1);
        assert_eq!((snapshot.nodes.len(), snapshot.unstable_nodes), (1, 1));
        assert_eq!((snapshot.relationships.len(), snapshot.unstable_relationships), (0, 1), "its end has no stable ID");
        let result = diff(&snapshot, &snapshot, DEFAULT_MAX_CHANGES);
        assert!(result.warnings.iter().any(|text| text.contains("no stable ID")), "{:?}", result.warnings);
    }

    #[test]
    fn secrets_stay_out_of_a_snapshot_and_a_bookkeeping_change_is_not_a_change() {
        let snapshot = snap(vec![node(1, "s/a", json!({"api_token": "abc123", "khonsu_synced_at": 5}))], vec![], Some(RunStatus::Complete), 1);
        let text = serde_json::to_string(&snapshot).unwrap();
        assert!(!text.contains("abc123"), "{text}");
        assert_eq!(snapshot.nodes[0].redacted, ["api_token"]);
        let later = snap(vec![node(1, "s/a", json!({"api_token": "abc123", "khonsu_synced_at": 9}))], vec![], Some(RunStatus::Complete), 2);
        assert_eq!(diff(&snapshot, &later, 10).node_counts, Counts::default());
    }

    #[test]
    fn the_id_follows_the_records_not_the_moment() {
        assert_eq!(before().snapshot_id, snap(before_nodes(), before_edges(), Some(RunStatus::Partial), 999).snapshot_id);
        assert_ne!(before().snapshot_id, after(Some(RunStatus::Complete)).snapshot_id);
    }

    fn before_nodes() -> Vec<GraphNode> {
        vec![node(5, "s/a", json!({"port": 80})), node(6, "s/b", json!({})), node(7, "s/c", json!({}))]
    }

    fn before_edges() -> Vec<GraphEdge> {
        vec![edge(30, 5, 6, "s/ab"), edge(31, 6, 7, "s/bc")]
    }

    #[test]
    fn a_graph_over_the_cap_is_refused_rather_than_cut_short() {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let mut graph = Graph::create(dir.path().join("graph"), "test").unwrap();
        for name in ["a", "b", "c"] {
            let text = format!("CREATE (:Host {{khonsu_id: '{name}'}})");
            graph.run(crate::Language::Cypher, &text, &Map::new(), crate::QueryOptions::default()).unwrap();
        }
        let refused = graph.snapshot_capped(&[], 2).unwrap_err();
        assert_eq!(refused.kind, "too_large", "{refused:?}");
        assert!(refused.message.contains('3'), "{refused:?}");

        let taken = graph.snapshot_capped(&[], 3).unwrap();
        assert_eq!(taken.nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        // Nothing in it says the graph changed.
        assert_eq!(taken.check(&mut graph).unwrap().0, GraphState::Unchanged);
    }

    #[test]
    fn a_file_it_cannot_use_says_why() {
        let kind = |text: &str| Snapshot::parse(text).unwrap_err().kind;
        assert_eq!(kind("{not json"), "invalid");
        assert_eq!(kind(r#"{"format": "khonsu-investigation", "version": 1}"#), "invalid");
        assert_eq!(kind(r#"{"format": "khonsu-snapshot", "version": 1}"#), "invalid");
        assert_eq!(kind(r#"{"format": "khonsu-snapshot", "version": 99}"#), "too_new");
        let text = serde_json::to_string(&before()).unwrap();
        assert_eq!(Snapshot::parse(&text).unwrap(), before());
    }
}

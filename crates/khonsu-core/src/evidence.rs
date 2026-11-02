//! What supports a node, a relationship or a path: where it came from, when,
//! and whether the source said it or KhonsuGraph worked it out.
//!
//! A value nobody supplied is [`Field::Unknown`], never a guess. Records are
//! meant to be copied out of the app, so they hold no source configuration, and
//! anything that looks like a secret is left out and named in `redacted`.

use crate::explore::{explain_hop, Exploration, Hop};
use crate::merge::{ID_PROPERTY, PLUGIN_PROPERTY, SOURCE_PROPERTY, SYNCED_AT_PROPERTY};
use crate::result::{GraphData, GraphEdge, GraphNode};
use crate::settings::{RunStatus, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

const REDACTED: &str = "[redacted]";
/// Property names that hold secrets, compared in lower case without `_` or `-`.
const SECRET_NAMES: &[&str] = &[
    "password", "passwd", "secret", "token", "credential", "apikey", "accesskey", "privatekey", "authorization", "cookie",
];

/// How much of a source its last sync covered, and so how far its records can
/// be taken as the whole picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCoverage {
    pub run_id: String,
    pub status: RunStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// The coverage of a source. Unknown when it is not configured here or was last
/// synced before runs were kept.
pub fn coverage_of(source: &str, sources: &[Source]) -> Field<SourceCoverage> {
    let run = sources.iter().find(|configured| configured.id == source).and_then(Source::last_run);
    run.map(|run| SourceCoverage {
        run_id: run.run_id.clone(),
        status: run.status,
        gaps: run.gaps.clone(),
    })
    .into()
}

/// A value that was supplied, or a plain statement that it was not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum Field<T> {
    Known(T),
    Unknown,
}

impl<T> From<Option<T>> for Field<T> {
    fn from(value: Option<T>) -> Self {
        value.map_or(Field::Unknown, Field::Known)
    }
}

impl<T> Field<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Field::Known(value) => Some(value),
            Field::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assertion {
    /// A data source reported this record.
    Explicit,
    /// KhonsuGraph worked this out from other records.
    Derived,
    /// Not written by a data source, for example by hand.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Node,
    Relationship,
    Path,
}

/// What supports one node or relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// `node:<khonsu_id>`, `rel:<khonsu_id>`. Without a `khonsu_id` it falls
    /// back on the internal ID, and `stable` is false: it may not survive a
    /// rebuild of the graph.
    pub evidence_id: String,
    pub stable: bool,
    pub kind: Kind,
    pub internal_id: i64,
    /// A node's labels, or a relationship's type.
    pub types: Vec<String>,
    /// A relationship's ends, by their evidence IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Field<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Field<String>>,
    /// The data source instance that wrote it.
    pub source: Field<String>,
    /// `<plugin id>@<plugin version>`.
    pub plugin: Field<String>,
    /// When this record last changed in the graph, in Unix seconds. An
    /// unchanged relationship is not rewritten by a sync, so this can be older
    /// than the last sync that saw it.
    pub record_written_at: Field<u64>,
    /// When the source last synced completely, in Unix seconds.
    pub source_last_successful_sync: Field<u64>,
    /// How much of the source its last sync covered.
    pub source_coverage: Field<SourceCoverage>,
    /// Which snapshot of the source this came from. Not recorded yet.
    pub source_snapshot: Field<String>,
    pub assertion: Assertion,
    /// The rule that produced it, for a derived record.
    pub rule_id: Field<String>,
    /// The record's own properties, without bookkeeping or secrets.
    pub properties: Map<String, Json>,
    /// Property names left out or masked because they look like secrets.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redacted: Vec<String>,
}

/// What supports a path a walk found. The path is derived, and the records it
/// rests on are explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathEvidence {
    /// `path:<hash of supporting>`: the same hops give the same ID.
    pub evidence_id: String,
    pub stable: bool,
    pub kind: Kind,
    pub assertion: Assertion,
    /// How it was worked out.
    pub derivation: String,
    pub rule_id: Field<String>,
    /// What the path does and does not show.
    pub claim: String,
    /// Each hop in words, in the order walked.
    pub hops: Vec<String>,
    /// The evidence IDs of the records below, start to end.
    pub supporting: Vec<String>,
    pub evidence: Vec<Evidence>,
}

pub const PATH_CLAIM: &str = "These records connect in the data. This is not proof that anything can reach anything.";

pub(crate) fn node_evidence_id(node: &GraphNode) -> String {
    text(&node.properties, ID_PROPERTY).map_or_else(|| format!("node-internal:{}", node.id), |id| format!("node:{id}"))
}

pub(crate) fn edge_evidence_id(edge: &GraphEdge) -> String {
    text(&edge.properties, ID_PROPERTY).map_or_else(|| format!("rel-internal:{}", edge.id), |id| format!("rel:{id}"))
}

pub fn for_node(node: &GraphNode, sources: &[Source]) -> Evidence {
    let (properties, redacted) = own_properties(&node.properties);
    let source = text(&node.properties, SOURCE_PROPERTY);
    Evidence {
        stable: text(&node.properties, ID_PROPERTY).is_some(),
        evidence_id: node_evidence_id(node),
        kind: Kind::Node,
        internal_id: node.id,
        types: node.labels.clone(),
        from: None,
        to: None,
        ..provenance(&node.properties, source, sources, properties, redacted)
    }
}

/// `nodes` are looked in for the relationship's ends; an end not among them
/// is unknown.
pub fn for_edge(edge: &GraphEdge, nodes: &[GraphNode], sources: &[Source]) -> Evidence {
    let (properties, redacted) = own_properties(&edge.properties);
    let source = text(&edge.properties, SOURCE_PROPERTY);
    let end = |id: i64| Field::from(nodes.iter().find(|node| node.id == id).map(node_evidence_id));
    Evidence {
        stable: text(&edge.properties, ID_PROPERTY).is_some(),
        evidence_id: edge_evidence_id(edge),
        kind: Kind::Relationship,
        internal_id: edge.id,
        types: vec![edge.rel_type.clone()],
        from: Some(end(edge.source)),
        to: Some(end(edge.target)),
        ..provenance(&edge.properties, source, sources, properties, redacted)
    }
}

/// The evidence for the path a walk took to `target`, unless it reached it by
/// no hops at all.
pub fn for_path(walk: &Exploration, target: i64, sources: &[Source]) -> Option<PathEvidence> {
    let path = walk.path_to(target).filter(|path| !path.is_empty())?;
    let hops: Vec<Hop> = path.into_iter().cloned().collect();
    Some(for_hops(&hops, &walk.graph, sources, "fewest hops in a bounded walk", Field::Unknown))
}

/// The evidence for `hops`, which must not be empty, read from `graph`. A hop
/// that crosses a record twice lists that record once, where it first appears.
pub(crate) fn for_hops(hops: &[Hop], graph: &GraphData, sources: &[Source], derivation: &str, rule_id: Field<String>) -> PathEvidence {
    let mut evidence: Vec<Evidence> = Vec::new();
    let mut add = |record: Evidence| {
        if !evidence.iter().any(|seen| seen.evidence_id == record.evidence_id) {
            evidence.push(record);
        }
    };
    if let Some(start) = graph.nodes.iter().find(|node| node.id == hops[0].from) {
        add(for_node(start, sources));
    }
    for hop in hops {
        if let Some(edge) = graph.edges.iter().find(|edge| edge.id == hop.edge) {
            add(for_edge(edge, &graph.nodes, sources));
        }
        if let Some(node) = graph.nodes.iter().find(|node| node.id == hop.to) {
            add(for_node(node, sources));
        }
    }
    let supporting: Vec<String> = evidence.iter().map(|record| record.evidence_id.clone()).collect();
    PathEvidence {
        evidence_id: format!("path:{:016x}", fnv1a(&supporting.join("\n"))),
        stable: evidence.iter().all(|record| record.stable),
        kind: Kind::Path,
        assertion: Assertion::Derived,
        derivation: derivation.into(),
        rule_id,
        claim: PATH_CLAIM.into(),
        hops: hops.iter().map(|hop| explain_hop(&graph.nodes, hop)).collect(),
        supporting,
        evidence,
    }
}

/// The fields a node and a relationship share.
fn provenance(properties: &Map<String, Json>, source: Option<String>, sources: &[Source], own: Map<String, Json>, redacted: Vec<String>) -> Evidence {
    let configured = source.as_deref().and_then(|id| sources.iter().find(|configured| configured.id == id));
    let last_ok = configured.and_then(|configured| match configured.last_complete_run() {
        Some(run) => Some(run.started_at),
        // A source last synced before runs were kept.
        None if configured.runs.is_empty() => configured.last_sync.as_ref().filter(|sync| sync.ok).map(|sync| sync.at),
        None => None,
    });
    let coverage = source.as_deref().map_or(Field::Unknown, |id| coverage_of(id, sources));
    Evidence {
        evidence_id: String::new(),
        stable: false,
        kind: Kind::Node,
        internal_id: 0,
        types: Vec::new(),
        from: None,
        to: None,
        assertion: if source.is_some() { Assertion::Explicit } else { Assertion::Unknown },
        source: source.into(),
        plugin: text(properties, PLUGIN_PROPERTY).into(),
        record_written_at: properties.get(SYNCED_AT_PROPERTY).and_then(Json::as_u64).into(),
        source_last_successful_sync: last_ok.into(),
        source_coverage: coverage,
        source_snapshot: Field::Unknown,
        rule_id: Field::Unknown,
        properties: own,
        redacted,
    }
}

fn text(properties: &Map<String, Json>, key: &str) -> Option<String> {
    properties.get(key).and_then(Json::as_str).filter(|value| !value.is_empty()).map(String::from)
}

/// `properties` without KhonsuGraph's bookkeeping, and with secrets masked,
/// along with the names of those masked.
fn own_properties(properties: &Map<String, Json>) -> (Map<String, Json>, Vec<String>) {
    let mut redacted = Vec::new();
    let own = properties
        .iter()
        .filter(|(key, _)| !key.starts_with("khonsu_"))
        .map(|(key, value)| {
            if is_secret_name(key) {
                redacted.push(key.clone());
                return (key.clone(), Json::String(REDACTED.into()));
            }
            let (clean, masked) = mask_values(value);
            if masked {
                redacted.push(key.clone());
            }
            (key.clone(), clean)
        })
        .collect();
    (own, redacted)
}

fn is_secret_name(name: &str) -> bool {
    let plain = name.to_lowercase().replace(['_', '-'], "");
    SECRET_NAMES.iter().any(|secret| plain.contains(secret))
}

fn mask_values(value: &Json) -> (Json, bool) {
    match value {
        Json::String(text) if looks_like_a_secret(text) => (Json::String(REDACTED.into()), true),
        Json::Array(items) => {
            let cleaned: Vec<(Json, bool)> = items.iter().map(mask_values).collect();
            let masked = cleaned.iter().any(|(_, masked)| *masked);
            (Json::Array(cleaned.into_iter().map(|(item, _)| item).collect()), masked)
        }
        Json::Object(map) => {
            let mut masked = false;
            let cleaned = map
                .iter()
                .map(|(key, item)| {
                    if is_secret_name(key) {
                        masked = true;
                        return (key.clone(), Json::String(REDACTED.into()));
                    }
                    let (item, inner) = mask_values(item);
                    masked |= inner;
                    (key.clone(), item)
                })
                .collect();
            (Json::Object(cleaned), masked)
        }
        other => (other.clone(), false),
    }
}

/// Key material by its shape: AWS access key IDs and PEM blocks.
fn looks_like_a_secret(text: &str) -> bool {
    let aws_key = (text.starts_with("AKIA") || text.starts_with("ASIA"))
        && text.len() == 20
        && text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    aws_key || text.contains("-----BEGIN")
}

/// A hash that stays the same across platforms and Rust versions.
pub(crate) fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LastSync;
    use serde_json::json;

    fn map(value: Json) -> Map<String, Json> {
        value.as_object().unwrap().clone()
    }

    fn synced(extra: Json) -> Map<String, Json> {
        let mut properties = map(json!({
            "khonsu_source": "shop",
            "khonsu_plugin": "aws@0.2.0",
            "khonsu_synced_at": 1000,
            "khonsu_hash": "abc",
        }));
        properties.extend(map(extra));
        properties
    }

    fn source(ok: bool) -> Source {
        serde_json::from_value(json!({
            "id": "shop", "name": "shop", "plugin": "aws", "graph": "g",
            "config": {"fixture_dir": "/home/someone/private", "profile": "prod"},
            "secret_keys": ["secret_access_key"],
            "last_sync": serde_json::to_value(LastSync { at: 2000, ok, nodes: 1, edges: 1, message: String::new() }).unwrap(),
        }))
        .unwrap()
    }

    fn node(id: i64, extra: Json) -> GraphNode {
        GraphNode {
            id,
            labels: vec!["Host".into()],
            properties: synced(extra),
        }
    }

    #[test]
    fn a_synced_node_says_where_it_came_from() {
        let record = for_node(&node(1, json!({"khonsu_id": "shop/h1", "name": "web"})), &[source(true)]);
        assert_eq!(record.evidence_id, "node:shop/h1");
        assert!(record.stable);
        assert_eq!(record.source, Field::Known("shop".to_string()));
        assert_eq!(record.plugin, Field::Known("aws@0.2.0".to_string()));
        assert_eq!(record.record_written_at, Field::Known(1000));
        assert_eq!(record.source_last_successful_sync, Field::Known(2000));
        assert_eq!(record.assertion, Assertion::Explicit);
        assert_eq!(record.properties, map(json!({"name": "web"})), "bookkeeping stays out of the properties");
    }

    #[test]
    fn what_was_not_supplied_is_unknown_not_made_up() {
        let bare = GraphNode {
            id: 7,
            labels: vec![],
            properties: map(json!({"name": "by hand"})),
        };
        let record = for_node(&bare, &[source(true)]);
        assert_eq!(record.evidence_id, "node-internal:7");
        assert!(!record.stable);
        assert_eq!(record.source, Field::Unknown);
        assert_eq!(record.plugin, Field::Unknown);
        assert_eq!(record.record_written_at, Field::Unknown);
        assert_eq!(record.source_last_successful_sync, Field::Unknown);
        assert_eq!(record.source_snapshot, Field::Unknown);
        assert_eq!(record.rule_id, Field::Unknown);
        assert_eq!(record.assertion, Assertion::Unknown);
    }

    #[test]
    fn a_failed_or_removed_source_gives_no_last_successful_sync() {
        let host = node(1, json!({"khonsu_id": "shop/h1"}));
        assert_eq!(for_node(&host, &[source(false)]).source_last_successful_sync, Field::Unknown);
        assert_eq!(for_node(&host, &[]).source_last_successful_sync, Field::Unknown);
        assert_eq!(for_node(&host, &[]).source, Field::Known("shop".to_string()), "the record still names it");
    }

    fn run(number: u64, status: RunStatus) -> crate::settings::SyncRun {
        crate::settings::SyncRun {
            run_id: format!("shop#{number}"),
            number,
            mode: crate::protocol::SyncMode::Full,
            started_at: number * 100,
            ended_at: number * 100 + 1,
            status,
            plugin: "aws@0.2.0".into(),
            received_nodes: 0,
            received_edges: 0,
            removals_applied: false,
            cursor_advanced: false,
            gaps: vec!["no route tables".into()],
            message: String::new(),
        }
    }

    #[test]
    fn a_record_says_how_much_of_its_source_the_last_sync_covered() {
        let host = node(1, json!({"khonsu_id": "shop/h1"}));
        let mut configured = source(false);
        configured.runs = vec![run(1, RunStatus::Complete), run(2, RunStatus::Partial)];
        let record = for_node(&host, std::slice::from_ref(&configured));
        let coverage = record.source_coverage.known().unwrap();
        assert_eq!((coverage.run_id.as_str(), coverage.status), ("shop#2", RunStatus::Partial));
        assert_eq!(coverage.gaps, ["no route tables"]);
        assert_eq!(record.source_last_successful_sync, Field::Known(100), "the last complete one, not the last one");

        configured.runs = vec![run(1, RunStatus::Failed)];
        let record = for_node(&host, std::slice::from_ref(&configured));
        assert_eq!(record.source_last_successful_sync, Field::Unknown, "no complete run, whatever last_sync says");

        // Without the source configured, or synced before runs were kept.
        assert_eq!(for_node(&host, &[]).source_coverage, Field::Unknown);
        assert_eq!(for_node(&host, &[source(true)]).source_coverage, Field::Unknown);
    }

    #[test]
    fn a_field_serializes_with_its_status() {
        assert_eq!(serde_json::to_value(Field::Known(5)).unwrap(), json!({"status": "known", "value": 5}));
        assert_eq!(serde_json::to_value(Field::<i64>::Unknown).unwrap(), json!({"status": "unknown"}));
    }

    #[test]
    fn secrets_are_masked_and_named_and_source_config_stays_out() {
        let host = node(
            1,
            json!({
                "khonsu_id": "shop/h1",
                "name": "web",
                "api_token": "abc123",
                "DB-Password": "hunter2",
                "key_id": "AKIAIOSFODNN7EXAMPLE",
                "pem": "-----BEGIN PRIVATE KEY-----",
                "nested": {"client_secret": "x", "fine": "y"},
                "list": ["ok", "AKIAIOSFODNN7EXAMPLE"],
            }),
        );
        let record = for_node(&host, &[source(true)]);
        let mut named = record.redacted.clone();
        named.sort();
        assert_eq!(named, ["DB-Password", "api_token", "key_id", "list", "nested", "pem"]);
        assert_eq!(record.properties["name"], json!("web"));
        assert_eq!(record.properties["nested"]["fine"], json!("y"));

        let text = serde_json::to_string(&record).unwrap();
        for secret in ["abc123", "hunter2", "AKIAIOSFODNN7EXAMPLE", "BEGIN PRIVATE", "\"x\""] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        for config in ["/home/someone/private", "prod", "secret_access_key", "fixture_dir"] {
            assert!(!text.contains(config), "{config} in {text}");
        }
    }

    fn walk() -> Exploration {
        let edge = |id: i64, source: i64, target: i64, stable: &str| GraphEdge {
            id,
            source,
            target,
            rel_type: "NEXT".into(),
            properties: synced(json!({"khonsu_id": stable})),
        };
        let hop = |depth: u32, from: i64, to: i64, edge: i64| Hop {
            depth,
            from,
            to,
            edge,
            rel_type: "NEXT".into(),
            along: true,
        };
        Exploration {
            starts: vec![1],
            graph: GraphData {
                nodes: vec![
                    node(1, json!({"khonsu_id": "s/a", "name": "a"})),
                    node(2, json!({"khonsu_id": "s/b", "name": "b"})),
                    node(3, json!({"khonsu_id": "s/c", "name": "c"})),
                ],
                edges: vec![edge(10, 1, 2, "s/ab"), edge(11, 2, 3, "s/bc")],
            },
            hops: vec![hop(1, 1, 2, 10), hop(2, 2, 3, 11)],
            depth_reached: 2,
            ..Exploration::default()
        }
    }

    #[test]
    fn a_path_is_derived_and_links_to_the_records_it_rests_on() {
        let path = for_path(&walk(), 3, &[]).unwrap();
        assert_eq!(path.assertion, Assertion::Derived);
        assert_eq!(path.kind, Kind::Path);
        assert_eq!(path.rule_id, Field::Unknown);
        assert_eq!(path.supporting, ["node:s/a", "rel:s/ab", "node:s/b", "rel:s/bc", "node:s/c"]);
        assert_eq!(path.hops, ["a -[NEXT]-> b", "b -[NEXT]-> c"]);
        assert_eq!(path.claim, PATH_CLAIM);
        assert!(path.stable);
        assert!(path.evidence.iter().all(|record| record.assertion == Assertion::Explicit));
        assert_eq!(path.evidence[1].from, Some(Field::Known("node:s/a".to_string())));
        assert_eq!(path.evidence[1].to, Some(Field::Known("node:s/b".to_string())));
    }

    #[test]
    fn the_same_hops_give_the_same_path_id_and_other_hops_another() {
        let to_c = for_path(&walk(), 3, &[]).unwrap().evidence_id;
        assert_eq!(to_c, for_path(&walk(), 3, &[]).unwrap().evidence_id);
        assert_ne!(to_c, for_path(&walk(), 2, &[]).unwrap().evidence_id);
        // A hash of known text, so that changing the function shows up here.
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_start_or_an_unreached_node_has_no_path() {
        assert!(for_path(&walk(), 1, &[]).is_none());
        assert!(for_path(&walk(), 99, &[]).is_none());
    }

    #[test]
    fn an_end_outside_the_given_nodes_is_unknown() {
        let edge = GraphEdge {
            id: 10,
            source: 1,
            target: 2,
            rel_type: "NEXT".into(),
            properties: synced(json!({"khonsu_id": "s/ab"})),
        };
        let record = for_edge(&edge, &[node(1, json!({"khonsu_id": "s/a"}))], &[]);
        assert_eq!(record.from, Some(Field::Known("node:s/a".to_string())));
        assert_eq!(record.to, Some(Field::Unknown));
    }
}

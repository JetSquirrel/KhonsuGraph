//! Writes plugin records into a graph, deduplicated by stable ID.
//!
//! Every node and relationship written for a data source carries:
//!
//! | property            | value                                   |
//! | ------------------- | --------------------------------------- |
//! | `khonsu_id`         | `<source id>/<record id>`               |
//! | `khonsu_source`     | the data source instance ID             |
//! | `khonsu_plugin`     | `<plugin id>@<plugin version>`          |
//! | `khonsu_synced_at`  | sync start, Unix seconds                |
//!
//! Nodes are upserted with `MERGE` on `khonsu_id`. Changed relationships are
//! deleted and created again; unchanged ones are left alone. This strategy
//! predates relationship-property `SET` support in HawDB 0.11.
//! Relationship patterns need labeled endpoints in HawDB, so the writer tracks
//! the label of every node it has seen.

use crate::engine::{Graph, NodeUpsert, QueryError, RelationshipCreate};
use crate::result::json_to_value;
use crate::schema::quote_ident;
use hawdb::Value;
use khonsu_plugin_protocol::{EdgeRecord, NodeRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const ID_PROPERTY: &str = "khonsu_id";
pub const SOURCE_PROPERTY: &str = "khonsu_source";
pub const PLUGIN_PROPERTY: &str = "khonsu_plugin";
pub const SYNCED_AT_PROPERTY: &str = "khonsu_synced_at";

const BATCH_SIZE: usize = 256;

/// A relationship shape `(source label)-[type]->(target label)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EdgePattern {
    pub source_label: String,
    pub rel_type: String,
    pub target_label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergeStats {
    pub nodes_written: u64,
    pub edges_written: u64,
    /// Records whose stable ID repeated an earlier record of this sync.
    pub duplicate_nodes: u64,
    pub duplicate_edges: u64,
    /// Edges whose endpoints were never seen; they are not written.
    pub skipped_edges: u64,
    pub stale_nodes_removed: u64,
    pub stale_edges_removed: u64,
    /// Relationships already in the graph as the source reports them; left alone.
    #[serde(default)]
    pub unchanged_edges: u64,
    pub warnings: Vec<String>,
}

/// One of the source's relationships as the graph holds it.
struct ExistingEdge {
    pattern: EdgePattern,
    source: String,
    target: String,
    properties: BTreeMap<String, Value>,
}

/// A record's properties as they are stored: identifier keys, nested
/// values as JSON text.
fn user_properties(properties: &serde_json::Map<String, Json>) -> BTreeMap<String, Value> {
    properties
        .iter()
        .filter_map(|(key, value)| Some((property_key(key)?, property_value(value))))
        .collect()
}

pub struct Merger<'g> {
    graph: &'g mut Graph,
    source: String,
    plugin: String,
    synced_at: i64,
    labels: HashMap<String, String>,
    indexed_labels: BTreeSet<String>,
    pending_nodes: Vec<NodeRecord>,
    pending_edges: Vec<EdgeRecord>,
    seen_nodes: BTreeSet<String>,
    seen_edges: BTreeSet<String>,
    patterns: BTreeSet<EdgePattern>,
    /// This source's nodes in the graph: stable ID to internal ID and label.
    node_index: Option<HashMap<String, (i64, String)>>,
    stats: MergeStats,
}

impl<'g> Merger<'g> {
    pub fn new(graph: &'g mut Graph, source: &str, plugin: &str, synced_at: i64) -> Self {
        Self {
            graph,
            source: source.to_string(),
            plugin: plugin.to_string(),
            synced_at,
            labels: HashMap::new(),
            indexed_labels: BTreeSet::new(),
            pending_nodes: Vec::new(),
            pending_edges: Vec::new(),
            seen_nodes: BTreeSet::new(),
            seen_edges: BTreeSet::new(),
            patterns: BTreeSet::new(),
            node_index: None,
            stats: MergeStats::default(),
        }
    }

    pub fn add_node(&mut self, node: NodeRecord) -> Result<(), QueryError> {
        if !self.seen_nodes.insert(node.id.clone()) {
            self.stats.duplicate_nodes += 1;
        }
        self.pending_nodes.push(node);
        if self.pending_nodes.len() >= BATCH_SIZE {
            self.flush_nodes()?;
        }
        Ok(())
    }

    /// Edges are held until the end so their endpoints' labels are known.
    pub fn add_edge(&mut self, edge: EdgeRecord) {
        if !self.seen_edges.insert(edge.id.clone()) {
            self.stats.duplicate_edges += 1;
        }
        self.pending_edges.push(edge);
    }

    /// Writes everything pending. With `remove_stale`, nodes and relationships
    /// of this source not seen in this sync are deleted (a full sync).
    ///
    /// Relationships are compared with the source's relationships already in
    /// the graph, read once: a new one is created, an unchanged one is left
    /// alone, and a changed one is deleted and created again (the strategy
    /// retained from HawDB 0.8). Deletes go in batches by
    /// pattern, `WHERE r.khonsu_id IN [...]`: matching one relationship at a
    /// time scans its type, which made a sync quadratic in its edges.
    pub fn finish(mut self, remove_stale: bool, _known_patterns: &[EdgePattern]) -> Result<(MergeStats, Vec<EdgePattern>), QueryError> {
        self.flush_nodes()?;
        let edges = std::mem::take(&mut self.pending_edges);
        // Keep the last occurrence of a repeated edge ID, like nodes.
        let mut last: BTreeMap<String, EdgeRecord> = BTreeMap::new();
        for edge in edges {
            last.insert(edge.id.clone(), edge);
        }
        let existing = if last.is_empty() && !remove_stale {
            HashMap::new()
        } else {
            self.existing_edges()?
        };
        let mut deletes: BTreeMap<EdgePattern, Vec<String>> = BTreeMap::new();
        let mut creates: Vec<(EdgeRecord, EdgePattern)> = Vec::new();
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for edge in last.into_values() {
            let (Some(source_label), Some(target_label)) = (self.label_of(&edge.source)?, self.label_of(&edge.target)?) else {
                self.stats.skipped_edges += 1;
                if self.stats.warnings.len() < 20 {
                    self.stats.warnings.push(format!(
                        "edge {} skipped: endpoint {} or {} not found",
                        edge.id, edge.source, edge.target
                    ));
                }
                continue;
            };
            let pattern = EdgePattern {
                source_label,
                rel_type: edge.rel_type.clone(),
                target_label,
            };
            let kid = self.scoped(&edge.id);
            kept.insert(kid.clone());
            self.patterns.insert(pattern.clone());
            match existing.get(&kid) {
                Some(old)
                    if old.pattern == pattern
                        && old.source == self.scoped(&edge.source)
                        && old.target == self.scoped(&edge.target)
                        && old.properties == user_properties(&edge.properties) =>
                {
                    self.stats.unchanged_edges += 1;
                }
                Some(old) => {
                    deletes.entry(old.pattern.clone()).or_default().push(kid);
                    creates.push((edge, pattern));
                }
                None => creates.push((edge, pattern)),
            }
        }
        if remove_stale {
            for (kid, old) in &existing {
                if !kept.contains(kid) {
                    deletes.entry(old.pattern.clone()).or_default().push(kid.clone());
                    self.stats.stale_edges_removed += 1;
                }
            }
        }
        for (pattern, kids) in deletes {
            for chunk in kids.chunks(500) {
                let list = Value::List(chunk.iter().map(|k| Value::String(k.clone())).collect());
                self.graph.write_batch(&[(
                    format!(
                        "MATCH (a:{})-[r:{}]->(b:{}) WHERE r.{ID_PROPERTY} IN $ids DELETE r",
                        quote_ident(&pattern.source_label),
                        quote_ident(&pattern.rel_type),
                        quote_ident(&pattern.target_label)
                    ),
                    BTreeMap::from([("ids".to_string(), list)]),
                )])?;
            }
        }
        let mut batch = Vec::with_capacity(creates.len());
        for (edge, pattern) in &creates {
            if let Some(create) = self.relationship_create(edge, pattern)? {
                batch.push(create);
                self.stats.edges_written += 1;
            }
        }
        self.graph.create_relationships(batch)?;
        if remove_stale {
            self.remove_stale_nodes()?;
        }
        let patterns = self.patterns.into_iter().collect();
        Ok((self.stats, patterns))
    }

    /// This source's relationships in the graph, by `khonsu_id`.
    fn existing_edges(&mut self) -> Result<HashMap<String, ExistingEdge>, QueryError> {
        let rows = self.graph.raw_cypher(
            &format!("MATCH (a)-[r]->(b) WHERE r.{SOURCE_PROPERTY} = $source RETURN a, r, b"),
            &BTreeMap::from([("source".to_string(), Value::String(self.source.clone()))]),
        )?;
        let mut found = HashMap::new();
        for row in rows {
            let maps: Vec<&BTreeMap<String, Value>> = row
                .iter()
                .filter_map(|v| match v {
                    Value::Map(map) => Some(map),
                    _ => None,
                })
                .collect();
            let Some(rel) = maps.iter().find(|m| m.contains_key("source_id") && m.contains_key("type")) else {
                continue;
            };
            let node = |key: &str| maps.iter().find(|m| m.contains_key("labels") && m.get("_id") == rel.get(key));
            let (Some(a), Some(b)) = (node("source_id"), node("target_id")) else {
                continue;
            };
            let first_label = |m: &BTreeMap<String, Value>| match m.get("labels") {
                Some(Value::List(labels)) => match labels.first() {
                    Some(Value::String(l)) => l.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            let text = |m: &BTreeMap<String, Value>, key: &str| match m.get(key) {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let kid = text(rel, ID_PROPERTY);
            let properties = rel
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "_id" | "type" | "source_id" | "target_id") && !k.starts_with("khonsu_"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            found.insert(
                kid,
                ExistingEdge {
                    pattern: EdgePattern {
                        source_label: first_label(a),
                        rel_type: text(rel, "type"),
                        target_label: first_label(b),
                    },
                    source: text(a, ID_PROPERTY),
                    target: text(b, ID_PROPERTY),
                    properties,
                },
            );
        }
        Ok(found)
    }

    fn scoped(&self, id: &str) -> String {
        format!("{}/{}", self.source, id)
    }

    /// The internal ID and label of every node this source has in the graph,
    /// by stable ID, read once after the nodes are written: relationships are
    /// then created between internal IDs.
    fn node_index(&mut self) -> Result<(), QueryError> {
        if self.node_index.is_some() {
            return Ok(());
        }
        let rows = self.graph.raw_cypher(
            &format!("MATCH (n {{{SOURCE_PROPERTY}: $source}}) RETURN n"),
            &BTreeMap::from([("source".to_string(), Value::String(self.source.clone()))]),
        )?;
        let mut index = HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(Value::Map(node)) = row.into_iter().next() else {
                continue;
            };
            let (Some(Value::String(kid)), Some(Value::Int(id))) = (node.get(ID_PROPERTY), node.get("_id")) else {
                continue;
            };
            let label = match node.get("labels") {
                Some(Value::List(labels)) => match labels.first() {
                    Some(Value::String(label)) => label.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            index.insert(kid.clone(), (*id, label));
        }
        self.node_index = Some(index);
        Ok(())
    }

    /// The label of a node of this source, by record ID.
    fn label_of(&mut self, id: &str) -> Result<Option<String>, QueryError> {
        if let Some(label) = self.labels.get(id) {
            return Ok(Some(label.clone()));
        }
        // An endpoint written by an earlier (e.g. incremental) sync.
        self.node_index()?;
        let scoped = self.scoped(id);
        Ok(self.node_index.as_ref().and_then(|index| index.get(&scoped)).map(|(_, label)| label.clone()))
    }

    fn node_id(&mut self, id: &str) -> Result<Option<i64>, QueryError> {
        self.node_index()?;
        let scoped = self.scoped(id);
        Ok(self.node_index.as_ref().and_then(|index| index.get(&scoped)).map(|(id, _)| *id))
    }

    fn flush_nodes(&mut self) -> Result<(), QueryError> {
        if self.pending_nodes.is_empty() {
            return Ok(());
        }
        let nodes = std::mem::take(&mut self.pending_nodes);
        let mut indexes = Vec::new();
        for label in nodes.iter().map(|n| n.label.clone()).collect::<BTreeSet<_>>() {
            if self.indexed_labels.insert(label.clone()) {
                // Ignored when it exists; makes the merge on khonsu_id a lookup.
                indexes.push((format!("CREATE INDEX ON :{}({ID_PROPERTY})", quote_ident(&label)), BTreeMap::new()));
            }
        }
        if !indexes.is_empty() {
            self.graph.write_batch(&indexes)?;
        }
        let mut upserts = Vec::with_capacity(nodes.len());
        for node in &nodes {
            if let Some(previous) = self.labels.insert(node.id.clone(), node.label.clone()) {
                if previous != node.label && self.stats.warnings.len() < 20 {
                    self.stats
                        .warnings
                        .push(format!("node {} changed label {previous} -> {}", node.id, node.label));
                }
            }
            let mut set = user_properties(&node.properties);
            set.insert(SOURCE_PROPERTY.into(), Value::String(self.source.clone()));
            set.insert(PLUGIN_PROPERTY.into(), Value::String(self.plugin.clone()));
            set.insert(SYNCED_AT_PROPERTY.into(), Value::Int(self.synced_at));
            upserts.push(NodeUpsert {
                label: node.label.clone(),
                key: (ID_PROPERTY.into(), Value::String(self.scoped(&node.id))),
                set,
            });
        }
        self.graph.merge_nodes(upserts)?;
        // New nodes have IDs the index does not know yet.
        self.node_index = None;
        self.stats.nodes_written += nodes.len() as u64;
        Ok(())
    }

    fn relationship_create(&mut self, edge: &EdgeRecord, pattern: &EdgePattern) -> Result<Option<RelationshipCreate>, QueryError> {
        let (Some(source), Some(target)) = (self.node_id(&edge.source)?, self.node_id(&edge.target)?) else {
            return Ok(None);
        };
        let mut properties = user_properties(&edge.properties);
        properties.insert(ID_PROPERTY.into(), Value::String(self.scoped(&edge.id)));
        properties.insert(SOURCE_PROPERTY.into(), Value::String(self.source.clone()));
        properties.insert(PLUGIN_PROPERTY.into(), Value::String(self.plugin.clone()));
        properties.insert(SYNCED_AT_PROPERTY.into(), Value::Int(self.synced_at));
        Ok(Some(RelationshipCreate {
            source_label: pattern.source_label.clone(),
            source,
            rel_type: pattern.rel_type.clone(),
            target_label: pattern.target_label.clone(),
            target,
            properties,
        }))
    }

    /// Nodes of this source a full sync did not see. Their relationships go
    /// with them (`DETACH DELETE`).
    fn remove_stale_nodes(&mut self) -> Result<(), QueryError> {
        let params = BTreeMap::from([
            ("source".to_string(), Value::String(self.source.clone())),
            ("synced_at".to_string(), Value::Int(self.synced_at)),
        ]);
        // DELETE inside a transaction returns no rows in HawDB 0.8, so stale
        // nodes are counted with the same pattern before they are deleted.
        let matched = format!("MATCH (n {{{SOURCE_PROPERTY}: $source}}) WHERE n.{SYNCED_AT_PROPERTY} < $synced_at");
        let stale = self.count(&format!("{matched} RETURN count(n) AS c"), &params)?;
        if stale > 0 {
            self.graph.write_batch(&[(format!("{matched} DETACH DELETE n"), params)])?;
            self.stats.stale_nodes_removed += stale;
        }
        Ok(())
    }

    fn count(&mut self, query: &str, params: &BTreeMap<String, Value>) -> Result<u64, QueryError> {
        let rows = self.graph.raw_cypher(query, params)?;
        Ok(match rows.first().and_then(|r| r.first()) {
            Some(Value::Int(n)) => *n as u64,
            _ => 0,
        })
    }
}

/// HawDB 0.8 accepts only plain identifiers as property names in `SET`.
/// Other characters become `_`; provenance names are reserved.
pub fn property_key(key: &str) -> Option<String> {
    let mut out: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.is_empty() {
        return None;
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.starts_with("khonsu_") || out == "_id" || out == "labels" {
        out.insert_str(0, "src_");
    }
    Some(out)
}

/// Nested objects are stored as JSON text; lists of scalars stay lists.
fn property_value(value: &Json) -> Value {
    match value {
        Json::Object(_) => Value::String(value.to_string()),
        Json::Array(items) if items.iter().any(|i| i.is_object() || i.is_array()) => Value::String(value.to_string()),
        other => json_to_value(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_keys_become_identifiers() {
        assert_eq!(property_key("instance type").as_deref(), Some("instance_type"));
        assert_eq!(property_key("2fa").as_deref(), Some("_2fa"));
        assert_eq!(property_key("khonsu_id").as_deref(), Some("src_khonsu_id"));
        assert_eq!(property_key(""), None);
    }
}

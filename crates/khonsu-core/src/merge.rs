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
//! Nodes are upserted with `MERGE` on `khonsu_id`. HawDB 0.8 cannot `SET`
//! relationship properties after creation, so a relationship is refreshed by
//! deleting the previous one with the same `khonsu_id` and creating it again.
//! Relationship patterns need labeled endpoints in HawDB, so the writer tracks
//! the label of every node it has seen.

use crate::engine::{Graph, QueryError};
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
    pub warnings: Vec<String>,
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
    pub fn finish(mut self, remove_stale: bool, known_patterns: &[EdgePattern]) -> Result<(MergeStats, Vec<EdgePattern>), QueryError> {
        self.flush_nodes()?;
        let edges = std::mem::take(&mut self.pending_edges);
        // Keep the last occurrence of a repeated edge ID, like nodes.
        let mut last: BTreeMap<String, EdgeRecord> = BTreeMap::new();
        for edge in edges {
            last.insert(edge.id.clone(), edge);
        }
        let mut batch = Vec::new();
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
            batch.extend(self.edge_statements(&edge, &pattern));
            self.patterns.insert(pattern);
            self.stats.edges_written += 1;
            if batch.len() >= BATCH_SIZE {
                self.graph.write_batch(&batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.graph.write_batch(&batch)?;
        }
        if remove_stale {
            self.remove_stale(known_patterns)?;
        }
        let patterns = self.patterns.into_iter().collect();
        Ok((self.stats, patterns))
    }

    fn scoped(&self, id: &str) -> String {
        format!("{}/{}", self.source, id)
    }

    fn label_of(&mut self, id: &str) -> Result<Option<String>, QueryError> {
        if let Some(label) = self.labels.get(id) {
            return Ok(Some(label.clone()));
        }
        // An endpoint written by an earlier (e.g. incremental) sync.
        let rows = self.graph.raw_cypher(
            &format!("MATCH (n) WHERE n.{ID_PROPERTY} = $id RETURN n"),
            &BTreeMap::from([("id".to_string(), Value::String(self.scoped(id)))]),
        )?;
        let label = rows.first().and_then(|row| match row.first() {
            Some(Value::Map(map)) => match map.get("labels") {
                Some(Value::List(labels)) => match labels.first() {
                    Some(Value::String(label)) => Some(label.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        if let Some(label) = &label {
            self.labels.insert(id.to_string(), label.clone());
        }
        Ok(label)
    }

    fn flush_nodes(&mut self) -> Result<(), QueryError> {
        if self.pending_nodes.is_empty() {
            return Ok(());
        }
        let nodes = std::mem::take(&mut self.pending_nodes);
        let mut statements = Vec::with_capacity(nodes.len() + 4);
        for label in nodes.iter().map(|n| n.label.clone()).collect::<BTreeSet<_>>() {
            if self.indexed_labels.insert(label.clone()) {
                // Ignored when it exists; makes MERGE on khonsu_id an index lookup.
                statements.push((format!("CREATE INDEX ON :{}({ID_PROPERTY})", quote_ident(&label)), BTreeMap::new()));
            }
        }
        for node in &nodes {
            if let Some(previous) = self.labels.insert(node.id.clone(), node.label.clone()) {
                if previous != node.label && self.stats.warnings.len() < 20 {
                    self.stats
                        .warnings
                        .push(format!("node {} changed label {previous} -> {}", node.id, node.label));
                }
            }
            let mut params = self.provenance(&node.id);
            let mut sets = provenance_sets("n");
            for (i, (key, value)) in node.properties.iter().enumerate() {
                let Some(key) = property_key(key) else { continue };
                let name = format!("p{i}");
                sets.push(format!("n.{key} = ${name}"));
                params.insert(name, property_value(value));
            }
            statements.push((
                format!(
                    "MERGE (n:{} {{{ID_PROPERTY}: $kid}}) SET {}",
                    quote_ident(&node.label),
                    sets.join(", ")
                ),
                params,
            ));
        }
        self.graph.write_batch(&statements)?;
        self.stats.nodes_written += nodes.len() as u64;
        Ok(())
    }

    fn edge_statements(&self, edge: &EdgeRecord, pattern: &EdgePattern) -> Vec<(String, BTreeMap<String, Value>)> {
        let source = quote_ident(&pattern.source_label);
        let target = quote_ident(&pattern.target_label);
        let rel_type = quote_ident(&pattern.rel_type);
        let mut params = self.provenance(&edge.id);
        params.insert("a".into(), Value::String(self.scoped(&edge.source)));
        params.insert("b".into(), Value::String(self.scoped(&edge.target)));
        let delete = (
            format!(
                "MATCH (a:{source})-[r:{rel_type}]->(b:{target}) WHERE a.{ID_PROPERTY} = $a AND b.{ID_PROPERTY} = $b AND r.{ID_PROPERTY} = $kid DELETE r"
            ),
            params.clone(),
        );
        let mut assignments = vec![
            format!("{ID_PROPERTY}: $kid"),
            format!("{SOURCE_PROPERTY}: $source"),
            format!("{PLUGIN_PROPERTY}: $plugin"),
            format!("{SYNCED_AT_PROPERTY}: $synced_at"),
        ];
        for (i, (key, value)) in edge.properties.iter().enumerate() {
            let Some(key) = property_key(key) else { continue };
            let name = format!("p{i}");
            assignments.push(format!("{key}: ${name}"));
            params.insert(name, property_value(value));
        }
        let create = (
            format!(
                "MATCH (a:{source} {{{ID_PROPERTY}: $a}}) MATCH (b:{target} {{{ID_PROPERTY}: $b}}) CREATE (a)-[r:{rel_type} {{{}}}]->(b)",
                assignments.join(", ")
            ),
            params,
        );
        vec![delete, create]
    }

    fn provenance(&self, id: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("kid".to_string(), Value::String(self.scoped(id))),
            ("source".to_string(), Value::String(self.source.clone())),
            ("plugin".to_string(), Value::String(self.plugin.clone())),
            ("synced_at".to_string(), Value::Int(self.synced_at)),
        ])
    }

    fn remove_stale(&mut self, known_patterns: &[EdgePattern]) -> Result<(), QueryError> {
        let params = BTreeMap::from([
            ("source".to_string(), Value::String(self.source.clone())),
            ("synced_at".to_string(), Value::Int(self.synced_at)),
        ]);
        let patterns: BTreeSet<EdgePattern> = known_patterns.iter().cloned().chain(self.patterns.iter().cloned()).collect();
        // DELETE inside a transaction returns no rows in HawDB 0.8, so stale
        // elements are counted with the same pattern before they are deleted.
        for pattern in patterns {
            let matched = format!(
                "MATCH (a:{})-[r:{} {{{SOURCE_PROPERTY}: $source}}]->(b:{}) WHERE r.{SYNCED_AT_PROPERTY} < $synced_at",
                quote_ident(&pattern.source_label),
                quote_ident(&pattern.rel_type),
                quote_ident(&pattern.target_label)
            );
            let stale = self.count(&format!("{matched} RETURN count(r) AS c"), &params)?;
            if stale > 0 {
                self.graph.write_batch(&[(format!("{matched} DELETE r"), params.clone())])?;
                self.stats.stale_edges_removed += stale;
            }
        }
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

fn provenance_sets(var: &str) -> Vec<String> {
    vec![
        format!("{var}.{SOURCE_PROPERTY} = $source"),
        format!("{var}.{PLUGIN_PROPERTY} = $plugin"),
        format!("{var}.{SYNCED_AT_PROPERTY} = $synced_at"),
    ]
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

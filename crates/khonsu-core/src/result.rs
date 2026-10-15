//! Query results as plain JSON, with graph elements pulled out for the canvas.
//!
//! HawDB returns a node as a map carrying `_id` and `labels`, and a
//! relationship as a map carrying `_id`, `type`, `source_id` and `target_id`.
//! Results keep those maps verbatim in `rows`; `graph` collects every node and
//! relationship found anywhere in the cells, deduplicated by ID.

use hawdb::Value;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Json>>,
    /// Rows returned by HawDB before KhonsuGraph applied `max_rows`.
    pub row_count: usize,
    /// True when `rows` holds fewer rows than HawDB returned.
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// Present when any cell holds a node or relationship.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphData>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub id: i64,
    pub labels: Vec<String>,
    pub properties: Map<String, Json>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub id: i64,
    pub source: i64,
    pub target: i64,
    pub rel_type: String,
    pub properties: Map<String, Json>,
}

impl GraphData {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Node IDs referenced by edges but missing from `nodes`.
    pub fn dangling_endpoints(&self) -> Vec<i64> {
        let known: BTreeSet<i64> = self.nodes.iter().map(|n| n.id).collect();
        let mut missing = BTreeSet::new();
        for edge in &self.edges {
            for id in [edge.source, edge.target] {
                if !known.contains(&id) {
                    missing.insert(id);
                }
            }
        }
        missing.into_iter().collect()
    }
}

pub fn value_to_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            // NaN and infinities have no JSON number form.
            .unwrap_or_else(|| Json::String(f.to_string())),
        Value::String(s) => Json::String(s.clone()),
        Value::Binary(bytes) => {
            let mut hex = String::with_capacity(2 + bytes.len() * 2);
            hex.push_str("\\x");
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            Json::String(hex)
        }
        Value::Uuid(uuid) => Json::String(uuid.to_string()),
        Value::List(items) => Json::Array(items.iter().map(value_to_json).collect()),
        Value::Map(map) => Json::Object(map.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect()),
    }
}

/// Converts a JSON parameter into a HawDB value. Integral numbers become
/// `Int`, other numbers `Float`.
pub fn json_to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(s) => Value::String(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
        Json::Object(map) => Value::Map(map.iter().map(|(k, v)| (k.clone(), json_to_value(v))).collect()),
    }
}

pub fn json_params(params: &Map<String, Json>) -> BTreeMap<String, Value> {
    params.iter().map(|(k, v)| (k.clone(), json_to_value(v))).collect()
}

#[derive(Default)]
pub(crate) struct GraphCollector {
    nodes: BTreeMap<i64, GraphNode>,
    edges: BTreeMap<i64, GraphEdge>,
}

impl GraphCollector {
    pub(crate) fn visit(&mut self, value: &Json) {
        match value {
            Json::Object(map) => {
                if let Some(node) = as_node(map) {
                    self.nodes.entry(node.id).or_insert(node);
                } else if let Some(edge) = as_edge(map) {
                    self.edges.entry(edge.id).or_insert(edge);
                } else {
                    map.values().for_each(|v| self.visit(v));
                }
            }
            Json::Array(items) => items.iter().for_each(|v| self.visit(v)),
            _ => {}
        }
    }

    pub(crate) fn finish(self) -> Option<GraphData> {
        let data = GraphData {
            nodes: self.nodes.into_values().collect(),
            edges: self.edges.into_values().collect(),
        };
        (!data.is_empty()).then_some(data)
    }
}

fn as_node(map: &Map<String, Json>) -> Option<GraphNode> {
    let id = map.get("_id")?.as_i64()?;
    let labels = map
        .get("labels")?
        .as_array()?
        .iter()
        .map(|l| l.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let properties = map
        .iter()
        .filter(|(k, _)| *k != "_id" && *k != "labels")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some(GraphNode { id, labels, properties })
}

fn as_edge(map: &Map<String, Json>) -> Option<GraphEdge> {
    let id = map.get("_id")?.as_i64()?;
    let rel_type = map.get("type")?.as_str()?.to_string();
    let source = map.get("source_id")?.as_i64()?;
    let target = map.get("target_id")?.as_i64()?;
    let properties = map
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "_id" | "type" | "source_id" | "target_id"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some(GraphEdge {
        id,
        source,
        target,
        rel_type,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nodes_and_relationships_are_collected_from_nested_cells() {
        let mut collector = GraphCollector::default();
        collector.visit(&json!({"_id": 0, "labels": ["Host"], "name": "web"}));
        collector.visit(&json!([{"_id": 0, "type": "RUNS", "source_id": 0, "target_id": 1, "since": 1}]));
        collector.visit(&json!({"_id": 0, "labels": ["Host"], "name": "web"}));
        collector.visit(&json!({"plain": 1}));
        let graph = collector.finish().unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].properties["name"], "web");
        assert_eq!(graph.edges[0].rel_type, "RUNS");
        assert_eq!(graph.edges[0].properties["since"], 1);
        assert_eq!(graph.dangling_endpoints(), vec![1]);
    }

    #[test]
    fn scalar_results_have_no_graph() {
        let mut collector = GraphCollector::default();
        collector.visit(&json!(42));
        assert!(collector.finish().is_none());
    }

    #[test]
    fn values_round_trip_through_json() {
        let value = Value::Map(BTreeMap::from([
            ("n".to_string(), Value::Int(3)),
            ("f".to_string(), Value::Float(1.5)),
            ("l".to_string(), Value::List(vec![Value::Bool(true), Value::Null])),
        ]));
        assert_eq!(json_to_value(&value_to_json(&value)), value);
        assert_eq!(value_to_json(&Value::Binary(vec![0xab, 1])), json!("\\xab01"));
    }
}

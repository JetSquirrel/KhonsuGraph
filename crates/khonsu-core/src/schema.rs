//! Labels, relationship types, properties and relational tables of a graph.
//!
//! Read from HawDB's `system.graph_statistics` and `information_schema`, so it
//! reflects what the directory actually holds.

use crate::engine::{Graph, QueryError};
use hawdb::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GraphSchema {
    pub node_count: u64,
    pub relationship_count: u64,
    pub labels: Vec<LabelInfo>,
    pub relationship_types: Vec<RelationshipTypeInfo>,
    pub tables: Vec<TableInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelInfo {
    pub name: String,
    pub count: u64,
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationshipTypeInfo {
    pub name: String,
    pub count: u64,
    pub properties: Vec<String>,
    /// `(source label, target label)` pairs seen for this type.
    pub patterns: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<String>,
}

impl GraphSchema {
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.relationship_types.is_empty() && self.tables.is_empty()
    }
}

#[derive(Default)]
struct Accumulator {
    count: u64,
    properties: BTreeSet<String>,
    patterns: BTreeSet<(String, String)>,
}

pub(crate) fn load(graph: &mut Graph) -> Result<GraphSchema, QueryError> {
    let stats = graph.raw_sql("SELECT * FROM system.graph_statistics")?;
    let mut schema = GraphSchema::default();
    let mut labels: BTreeMap<String, Accumulator> = BTreeMap::new();
    let mut types: BTreeMap<String, Accumulator> = BTreeMap::new();

    for row in &stats {
        let kind = text(row, "statistic_kind").unwrap_or_default();
        let count = int(row, "count").unwrap_or(0);
        let label = text(row, "label_name");
        let rel_type = text(row, "relationship_type_name");
        let property = text(row, "property_name");
        match kind.as_str() {
            "node_count" => schema.node_count = count,
            "relationship_count" => schema.relationship_count = count,
            _ => {}
        }
        if let Some(rel_type) = rel_type {
            let entry = types.entry(rel_type).or_default();
            if kind.ends_with("_count") && property.is_none() && !kind.contains("pattern") {
                entry.count = entry.count.max(count);
            }
            if let Some(property) = property {
                entry.properties.insert(property);
            }
            if let (Some(source), Some(target)) = (text(row, "source_label_name"), text(row, "target_label_name")) {
                entry.patterns.insert((source, target));
            }
        } else if let Some(label) = label {
            let entry = labels.entry(label).or_default();
            if kind == "label_count" {
                entry.count = count;
            }
            if let Some(property) = property {
                entry.properties.insert(property);
            }
        }
    }

    // Statistics are maintained by HawDB and may lag; counts are exact.
    for (name, entry) in labels.iter_mut() {
        if let Some(count) = exact_count(graph, &format!("MATCH (n:{}) RETURN count(n) AS c", quote_ident(name))) {
            entry.count = count;
        }
    }
    for (name, entry) in types.iter_mut() {
        if let Some(count) = exact_count(graph, &format!("MATCH ()-[r:{}]->() RETURN count(r) AS c", quote_ident(name))) {
            entry.count = count;
        }
    }

    schema.labels = labels
        .into_iter()
        .filter(|(_, e)| e.count > 0)
        .map(|(name, e)| LabelInfo {
            name,
            count: e.count,
            properties: e.properties.into_iter().collect(),
        })
        .collect();
    schema.relationship_types = types
        .into_iter()
        .filter(|(_, e)| e.count > 0)
        .map(|(name, e)| RelationshipTypeInfo {
            name,
            count: e.count,
            properties: e.properties.into_iter().collect(),
            patterns: e.patterns.into_iter().collect(),
        })
        .collect();
    schema.tables = load_tables(graph)?;
    Ok(schema)
}

fn load_tables(graph: &mut Graph) -> Result<Vec<TableInfo>, QueryError> {
    let mut tables: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for row in graph.raw_sql("SELECT * FROM information_schema.tables")? {
        let (Some(name), Some(schema)) = (text(&row, "table_name"), text(&row, "table_schema")) else {
            continue;
        };
        if schema == "public" && !name.starts_with("hawdb_") {
            tables.insert(name, Vec::new());
        }
    }
    for row in graph.raw_sql("SELECT * FROM information_schema.columns")? {
        let (Some(table), Some(column)) = (text(&row, "table_name"), text(&row, "column_name")) else {
            continue;
        };
        if let Some(columns) = tables.get_mut(&table) {
            columns.push((int(&row, "ordinal_position").unwrap_or(0) as i64, column));
        }
    }
    Ok(tables
        .into_iter()
        .map(|(name, mut columns)| {
            columns.sort();
            TableInfo {
                name,
                columns: columns.into_iter().map(|(_, c)| c).collect(),
            }
        })
        .collect())
}

fn exact_count(graph: &mut Graph, query: &str) -> Option<u64> {
    let rows = graph.raw_cypher(query, &BTreeMap::new()).ok()?;
    match rows.first()?.first()? {
        Value::Int(count) => Some(*count as u64),
        _ => None,
    }
}

/// Backquotes an identifier when it is not a plain Cypher name.
pub fn quote_ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

fn text(row: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match row.get(key)? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn int(row: &BTreeMap<String, Value>, key: &str) -> Option<u64> {
    match row.get(key)? {
        Value::Int(i) => Some(*i as u64),
        _ => None,
    }
}

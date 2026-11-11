use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value as Json};

use super::Network;
use crate::{Graph, Language, QueryError, QueryOptions, QueryResult};

/// Runs a read-only Cypher statement: the graph itself, or — while the
/// window holds the graph — the window on the CLI's behalf.
pub trait CypherReader {
    fn cypher(&mut self, text: &str) -> Result<QueryResult, QueryError>;
}

impl CypherReader for Graph {
    fn cypher(&mut self, text: &str) -> Result<QueryResult, QueryError> {
        self.run_read(Language::Cypher, text, &Map::new(), QueryOptions { max_rows: usize::MAX })
    }
}

/// Which graph to analyze.
#[derive(Debug, Clone, Default)]
pub struct LoadSpec {
    /// A Cypher query returning nodes and relationships whole: the
    /// subgraph is what it returns. Overrides `labels` and `types`.
    pub cypher: Option<String>,
    /// Only nodes with one of these labels, and relationships between them.
    pub labels: Vec<String>,
    /// Only relationships of these types; without `labels`, only the nodes
    /// they join.
    pub types: Vec<String>,
    /// A numeric relationship property read as each relationship's weight
    /// (or length, for paths); 1 where missing.
    pub weight: Option<String>,
}

/// Rows per page when reading a whole graph: under KhonsuGraph's 100,000-row
/// read ceiling, so a graph of any size reads in pages.
const PAGE: usize = 50_000;

pub fn load(reader: &mut dyn CypherReader, spec: &LoadSpec) -> Result<Network, QueryError> {
    let mut net = Network::new();
    if let Some(text) = &spec.cypher {
        let result = reader.cypher(text)?;
        let graph = result.graph.unwrap_or_default();
        for node in &graph.nodes {
            net.add_node(node.id);
        }
        for edge in &graph.edges {
            let weight = match spec.weight.as_deref() {
                Some(w) => weight(edge.properties.get(w), w, edge.id)?,
                None => 1.0,
            };
            net.add_edge(edge.source, edge.target, weight);
        }
        return Ok(net);
    }

    let mut kept: Option<HashSet<i64>> = None;
    if spec.labels.is_empty() {
        // Relationship types alone make a graph of what they join, as
        // networkx builds one from an edge list; otherwise every node counts,
        // isolated ones included.
        if spec.types.is_empty() {
            for id in paged_ids(reader, "MATCH (n) RETURN id(n) AS id")? {
                net.add_node(id);
            }
        }
    } else {
        let mut set = HashSet::new();
        for label in &spec.labels {
            for id in paged_ids(reader, &format!("MATCH (n:{}) RETURN id(n) AS id", ident(label)))? {
                net.add_node(id);
                set.insert(id);
            }
        }
        kept = Some(set);
    }

    let weight_column = spec.weight.as_deref().map(|w| format!(", r.{} AS w", ident(w))).unwrap_or_default();
    let patterns: Vec<String> = if spec.types.is_empty() {
        vec!["(a)-[r]->(b)".into()]
    } else {
        spec.types.iter().map(|t| format!("(a)-[r:{}]->(b)", ident(t))).collect()
    };
    for pattern in patterns {
        let mut skip = 0;
        loop {
            let text = format!(
                "MATCH {pattern} RETURN id(r) AS rid, id(a) AS s, id(b) AS t{weight_column} ORDER BY rid SKIP {skip} LIMIT {PAGE}"
            );
            let result = reader.cypher(&text)?;
            let col = |name: &str| result.columns.iter().position(|c| c == name);
            let (s, t, w, rid) = (col("s"), col("t"), col("w"), col("rid"));
            for row in &result.rows {
                let (Some(source), Some(target)) = (s.and_then(|i| row[i].as_i64()), t.and_then(|i| row[i].as_i64())) else {
                    continue;
                };
                if kept.as_ref().is_some_and(|k| !k.contains(&source) || !k.contains(&target)) {
                    continue;
                }
                let weighed = match (w, spec.weight.as_deref()) {
                    (Some(i), Some(name)) => weight(row.get(i), name, rid.and_then(|i| row[i].as_i64()).unwrap_or(-1))?,
                    _ => 1.0,
                };
                net.add_edge(source, target, weighed);
            }
            if result.rows.len() < PAGE {
                break;
            }
            skip += PAGE;
        }
    }
    Ok(net)
}

fn paged_ids(reader: &mut dyn CypherReader, base: &str) -> Result<Vec<i64>, QueryError> {
    let mut ids = Vec::new();
    let mut skip = 0;
    loop {
        let result = reader.cypher(&format!("{base} ORDER BY id SKIP {skip} LIMIT {PAGE}"))?;
        let before = ids.len();
        ids.extend(result.rows.iter().filter_map(|row| row.first().and_then(Json::as_i64)));
        if ids.len() - before < PAGE {
            return Ok(ids);
        }
        skip += PAGE;
    }
}

/// A weight from a property: numbers as they are, numeric text parsed, 1
/// for anything else, so a missing weight counts as an unweighted hop.
fn number(value: Option<&Json>) -> f64 {
    match value {
        Some(Json::Number(n)) => n.as_f64().unwrap_or(1.0),
        Some(Json::String(s)) => s.trim().parse().unwrap_or(1.0),
        _ => 1.0,
    }
}

/// A relationship's weight, which the algorithms need finite and not
/// negative: a shortest path through a negative cycle has no end, and the
/// search for one never returned. Refused here, before any of them runs.
fn weight(value: Option<&Json>, property: &str, relationship: i64) -> Result<f64, QueryError> {
    let weight = number(value);
    if weight.is_finite() && weight >= 0.0 {
        return Ok(weight);
    }
    Err(QueryError::new(
        "weight",
        format!(
            "relationship #{relationship} has {property} = {weight}: a weight must be a finite number, zero or more. \
             Leave such relationships out with --cypher, or use another property"
        ),
    ))
}

/// A label, type or property name as Cypher reads it: backquoted unless a
/// plain identifier.
fn ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// What a result row says of a node besides its ID.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NodeInfo {
    pub labels: Vec<String>,
    /// The first of `name`, `title`, `label`, `external_id`, `id` the node has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Labels and a display name for `ids`, in batches.
pub fn describe_nodes(reader: &mut dyn CypherReader, ids: &[i64]) -> Result<HashMap<i64, NodeInfo>, QueryError> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(500) {
        let list = chunk.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        let result = reader.cypher(&format!("MATCH (n) WHERE id(n) IN [{list}] RETURN n"))?;
        for node in result.graph.unwrap_or_default().nodes {
            let name = ["name", "title", "label", "external_id", "id"].iter().find_map(|key| match node.properties.get(*key) {
                Some(Json::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(Json::Number(n)) => Some(n.to_string()),
                _ => None,
            });
            out.insert(node.id, NodeInfo { labels: node.labels, name });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_quoted_only_when_they_must_be() {
        assert_eq!(ident("Process"), "Process");
        assert_eq!(ident("IN_TACTIC"), "IN_TACTIC");
        assert_eq!(ident("price hkd"), "`price hkd`");
        assert_eq!(ident("a`b"), "`a``b`");
    }

    #[test]
    fn weights_read_numbers_and_numeric_text() {
        assert_eq!(number(Some(&Json::from(2.5))), 2.5);
        assert_eq!(number(Some(&Json::from("3"))), 3.0);
        assert_eq!(number(Some(&Json::from("n/a"))), 1.0);
        assert_eq!(number(None), 1.0);
    }

    #[test]
    fn a_weight_that_is_negative_or_not_finite_is_refused() {
        assert_eq!(weight(Some(&Json::from(0)), "score", 1).unwrap(), 0.0);
        assert_eq!(weight(Some(&Json::from("2.5")), "score", 1).unwrap(), 2.5);
        assert_eq!(weight(None, "score", 1).unwrap(), 1.0);
        for bad in [Json::from(-1), Json::from(-0.5), Json::from("-3"), Json::from("NaN"), Json::from("inf"), Json::from("-inf")] {
            let error = weight(Some(&bad), "score", 7).unwrap_err();
            assert_eq!(error.kind, "weight", "{bad}");
            assert!(error.message.contains("#7") && error.message.contains("score"), "{}", error.message);
        }
    }

    /// Answers every statement with the same rows.
    struct Rows(crate::QueryResult);

    impl CypherReader for Rows {
        fn cypher(&mut self, _: &str) -> Result<crate::QueryResult, QueryError> {
            Ok(self.0.clone())
        }
    }

    /// Two nodes joined both ways with weight -1 made closeness and weighted
    /// paths run without end.
    #[test]
    fn a_negative_cycle_is_refused_before_any_algorithm_runs() {
        let rows = |weights: [f64; 2]| {
            Rows(crate::QueryResult {
                columns: ["rid", "s", "t", "w"].map(String::from).to_vec(),
                rows: vec![
                    vec![Json::from(10), Json::from(1), Json::from(2), Json::from(weights[0])],
                    vec![Json::from(11), Json::from(2), Json::from(1), Json::from(weights[1])],
                ],
                row_count: 2,
                truncated: false,
                elapsed_ms: 0,
                graph: None,
            })
        };
        let spec = LoadSpec { types: vec!["LINKS".into()], weight: Some("score".into()), ..LoadSpec::default() };
        let error = load(&mut rows([-1.0, -1.0]), &spec).unwrap_err();
        assert_eq!(error.kind, "weight");
        assert!(error.message.contains("#10"), "{}", error.message);

        let net = load(&mut rows([1.0, 2.0]), &spec).unwrap();
        assert_eq!(net.edge_count(), 2);
        let close = crate::analysis::algorithms::closeness(&net, true, true);
        assert!(close.iter().all(|c| c.is_finite()), "{close:?}");
        // Without --weight the property is not read at all.
        let unweighted = LoadSpec { types: vec!["LINKS".into()], ..LoadSpec::default() };
        assert_eq!(load(&mut rows([-1.0, -1.0]), &unweighted).unwrap().edge_count(), 2);
    }
}

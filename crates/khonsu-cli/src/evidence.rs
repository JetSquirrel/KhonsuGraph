//! `khonsu evidence`: what supports a node or relationship. See
//! `khonsu_core::evidence`.

use clap::Args;
use khonsu_core::evidence::{self, Evidence};
use khonsu_core::settings::{self, Source};
use khonsu_core::{AccessMode, Graph, GraphData, Language, QueryOptions};
use serde_json::{json, Map, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Args)]
pub struct EvidenceArgs {
    #[command(flatten)]
    db: DbArg,
    /// A node: its `khonsu_id`, or its ID (`id(n)`, `_id` in results). Repeat for several.
    #[arg(long)]
    node: Vec<String>,
    /// A relationship: its `khonsu_id`, or its ID (`id(r)`, `_id` in results). Repeat for several.
    #[arg(long)]
    rel: Vec<String>,
}

pub fn evidence(args: EvidenceArgs) -> Result<Value, CliError> {
    if args.node.is_empty() && args.rel.is_empty() {
        return Err(CliError::usage("name a record with --node or --rel"));
    }
    let path = resolve_db(&args.db)?;
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let sources = settings::load_sources();
    let mut records = Vec::new();
    for node in &args.node {
        records.push(node_evidence(&mut graph, node, &sources)?);
    }
    for rel in &args.rel {
        records.push(rel_evidence(&mut graph, rel, &sources)?);
    }
    Ok(json!({ "evidence": records }))
}

fn node_evidence(graph: &mut Graph, node: &str, sources: &[Source]) -> Result<Evidence, CliError> {
    let found = lookup(graph, node, "MATCH (n) WHERE id(n) = $id RETURN n LIMIT 1", "MATCH (n {khonsu_id: $id}) RETURN n LIMIT 1")?;
    let record = found.nodes.first().ok_or_else(|| CliError::new("not_found", format!("no node {node:?}")))?;
    Ok(evidence::for_node(record, sources))
}

fn rel_evidence(graph: &mut Graph, rel: &str, sources: &[Source]) -> Result<Evidence, CliError> {
    let found = lookup(
        graph,
        rel,
        "MATCH (a)-[r]->(b) WHERE id(r) = $id RETURN a, r, b LIMIT 1",
        "MATCH (a)-[r]->(b) WHERE r.khonsu_id = $id RETURN a, r, b LIMIT 1",
    )?;
    let record = found.edges.first().ok_or_else(|| CliError::new("not_found", format!("no relationship {rel:?}")))?;
    Ok(evidence::for_edge(record, &found.nodes, sources))
}

/// Runs `by_number` when `id` is a number, `by_stable_id` otherwise.
fn lookup(graph: &mut Graph, id: &str, by_number: &str, by_stable_id: &str) -> Result<GraphData, CliError> {
    let (text, param) = match id.parse::<i64>() {
        Ok(number) => (by_number, json!(number)),
        Err(_) => (by_stable_id, json!(id)),
    };
    let params = Map::from_iter([("id".to_string(), param)]);
    let result = graph.run(Language::Cypher, text, &params, QueryOptions::default())?;
    Ok(result.graph.unwrap_or_default())
}

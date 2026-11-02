//! `khonsu explore`: a bounded walk from chosen nodes. See
//! `khonsu_core::explore`.

use std::sync::atomic::AtomicBool;

use clap::{Args, ValueEnum};
use khonsu_core::explore::{self, Direction, ExploreRequest};
use khonsu_core::{AccessMode, Graph, Language, QueryOptions};
use serde_json::{json, Map, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Clone, Copy, ValueEnum)]
enum DirectionArg {
    /// Along the arrows.
    Out,
    /// Against the arrows.
    In,
    Both,
}

#[derive(Args)]
pub struct ExploreArgs {
    #[command(flatten)]
    db: DbArg,
    /// A node to start from: its stable `khonsu_id`, or its ID (`id(n)`, `_id` in results). Repeat for several.
    #[arg(long, required = true)]
    from: Vec<String>,
    #[arg(long, value_enum, default_value = "out")]
    direction: DirectionArg,
    /// Only follow relationships of this type. Repeat or separate with commas.
    #[arg(long = "type", value_delimiter = ',')]
    rel_type: Vec<String>,
    /// Steps to walk from a start, at most 16.
    #[arg(long, default_value_t = explore::DEFAULT_MAX_HOPS)]
    max_hops: u32,
    /// Nodes to return, starts included, at most 2,000.
    #[arg(long, default_value_t = explore::DEFAULT_MAX_NODES)]
    max_nodes: usize,
    /// Relationships to return, at most 5,000.
    #[arg(long, default_value_t = explore::DEFAULT_MAX_EDGES)]
    max_edges: usize,
    /// Also report a fewest-hops path to this node among what the walk found.
    #[arg(long)]
    to: Option<String>,
}

pub fn explore(args: ExploreArgs) -> Result<Value, CliError> {
    let path = resolve_db(&args.db)?;
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let starts = args.from.iter().map(|node| node_id(&mut graph, node)).collect::<Result<Vec<_>, _>>()?;
    let target = args.to.as_deref().map(|node| node_id(&mut graph, node)).transpose()?;
    let request = ExploreRequest {
        starts,
        direction: match args.direction {
            DirectionArg::Out => Direction::Out,
            DirectionArg::In => Direction::In,
            DirectionArg::Both => Direction::Both,
        },
        rel_types: args.rel_type,
        max_hops: args.max_hops,
        max_nodes: args.max_nodes,
        max_edges: args.max_edges,
    };
    // Nothing cancels a command from outside but ending it.
    let result = graph.explore(&request, &AtomicBool::new(false))?;

    let explained = |hop: &explore::Hop| {
        let mut value = serde_json::to_value(hop).expect("a hop serializes");
        value["text"] = json!(result.explain(hop));
        value
    };
    let mut out = json!({
        "complete": result.is_complete(),
        "limits": result.limits,
        "depth_reached": result.depth_reached,
        "missing_starts": result.missing_starts,
        "starts": result.starts,
        "hops": result.hops.iter().map(explained).collect::<Vec<_>>(),
        "graph": result.graph,
    });
    if let Some(target) = target {
        let found = result.path_to(target);
        out["path"] = json!({
            "to": target,
            "found": found.is_some(),
            "hops": found.unwrap_or_default().into_iter().map(explained).collect::<Vec<_>>(),
        });
    }
    Ok(out)
}

/// `node` as an internal ID: a number is one already, anything else a stable ID.
fn node_id(graph: &mut Graph, node: &str) -> Result<i64, CliError> {
    if let Ok(id) = node.parse() {
        return Ok(id);
    }
    let params = Map::from_iter([("id".to_string(), json!(node))]);
    let found = graph.run(Language::Cypher, "MATCH (n {khonsu_id: $id}) RETURN id(n) AS id LIMIT 1", &params, QueryOptions::default())?;
    found.rows.first().and_then(|row| row[0].as_i64()).ok_or_else(|| CliError::new("not_found", format!("no node has the khonsu_id {node:?}")))
}

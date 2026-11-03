//! `khonsu investigation`: save what is being looked at, and come back to it.
//! See `khonsu_core::investigation`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Args, Subcommand, ValueEnum};
use khonsu_core::explore::{self, Direction, ExploreRequest};
use khonsu_core::investigation::{Capture, FileError, Investigation, SavedQuery};
use khonsu_core::{normalize_path, settings, AccessMode, Graph, Language, QueryOptions};
use serde_json::{json, Map, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Clone, Copy, ValueEnum)]
enum DirectionArg {
    Out,
    In,
    Both,
}

#[derive(Subcommand)]
pub enum InvestigationCommand {
    /// Save a walk, a path, a query and a dashboard to a file of its own. The graph is not changed.
    Save(SaveArgs),
    /// Run a saved walk again, and say what differs from when it was saved.
    Reopen(ReopenArgs),
}

#[derive(Args)]
pub struct SaveArgs {
    /// The file to write, e.g. `web-tier.investigation`.
    file: PathBuf,
    #[command(flatten)]
    db: DbArg,
    /// What to call it. Defaults to the file's name.
    #[arg(long)]
    name: Option<String>,
    /// A node to start from: its `khonsu_id`, or its ID. Repeat for several.
    #[arg(long)]
    from: Vec<String>,
    #[arg(long, value_enum, default_value = "out")]
    direction: DirectionArg,
    /// Only follow relationships of this type. Repeat or separate with commas.
    #[arg(long = "type", value_delimiter = ',')]
    rel_type: Vec<String>,
    #[arg(long, default_value_t = explore::DEFAULT_MAX_HOPS)]
    max_hops: u32,
    #[arg(long, default_value_t = explore::DEFAULT_MAX_NODES)]
    max_nodes: usize,
    #[arg(long, default_value_t = explore::DEFAULT_MAX_EDGES)]
    max_edges: usize,
    /// The node whose path to keep. Needs --from.
    #[arg(long)]
    to: Option<String>,
    /// An exact path to keep, as JSON: a finding's `path_refs`. Reopening looks up these
    /// records rather than walking, so it keeps a path that isn't the fewest hops.
    #[arg(long, conflicts_with_all = ["from", "to"])]
    path_refs: Option<String>,
    /// A Cypher query to keep with it. Saved as typed, so leave out anything secret.
    #[arg(long, conflicts_with = "sql")]
    cypher: Option<String>,
    /// A SQL query to keep with it. Saved as typed, so leave out anything secret.
    #[arg(long)]
    sql: Option<String>,
    /// A .dash dashboard to keep with it.
    #[arg(long)]
    dashboard: Option<PathBuf>,
    /// Replace the file if it exists.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct ReopenArgs {
    /// The saved file.
    file: PathBuf,
    /// Use this graph instead of the one the file names.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
}

pub fn investigation(command: InvestigationCommand) -> Result<Value, CliError> {
    match command {
        InvestigationCommand::Save(args) => save(args),
        InvestigationCommand::Reopen(args) => reopen(args),
    }
}

impl From<FileError> for CliError {
    fn from(error: FileError) -> Self {
        CliError::new(error.kind, error.message)
    }
}

fn save(args: SaveArgs) -> Result<Value, CliError> {
    if args.to.is_some() && args.from.is_empty() {
        return Err(CliError::usage("--to needs --from: a path is found by walking from somewhere"));
    }
    let path = normalize_path(&resolve_db(&args.db)?);
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let starts = args.from.iter().map(|node| node_id(&mut graph, node)).collect::<Result<Vec<_>, _>>()?;
    let target = args.to.as_deref().map(|node| node_id(&mut graph, node)).transpose()?;
    let walk = (!starts.is_empty()).then(|| ExploreRequest {
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
    });
    let query = match (args.cypher, args.sql) {
        (Some(text), _) => Some(SavedQuery { language: "cypher".into(), text }),
        (_, Some(text)) => Some(SavedQuery { language: "sql".into(), text }),
        _ => None,
    };
    let name = args.name.unwrap_or_else(|| args.file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default());
    let path_refs = match &args.path_refs {
        Some(text) => serde_json::from_str(text).map_err(|error| CliError::usage(format!("--path-refs is not a list of hops: {error}")))?,
        None => Vec::new(),
    };
    let capture = Capture {
        name,
        query,
        walk,
        target,
        path_refs,
        dashboard: args.dashboard.as_deref().map(normalize_path),
    };
    let saved = graph.capture_investigation(&path, capture, &settings::load_sources(), &AtomicBool::new(false))?;
    saved.save(&args.file, args.force)?;
    Ok(json!({
        "saved": normalize_path(&args.file),
        "path_found": saved.path.as_ref().map(|path| !path.supporting.is_empty()),
        "investigation": saved,
    }))
}

fn reopen(args: ReopenArgs) -> Result<Value, CliError> {
    let saved = Investigation::load(&args.file)?;
    let path = args.db.clone().unwrap_or_else(|| saved.graph.clone());
    if !path.is_dir() {
        return Err(CliError::new(
            "graph_missing",
            format!("the graph {} is not there; pass --db to use another", path.display()),
        ));
    }
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let result = graph.reopen_investigation(&saved, &settings::load_sources(), &AtomicBool::new(false))?;

    let walk = result.walk.as_ref().map(|walk| {
        let explained = |hop: &explore::Hop| {
            let mut value = serde_json::to_value(hop).expect("a hop serializes");
            value["text"] = json!(walk.explain(hop));
            value
        };
        json!({
            "complete": walk.is_complete(),
            "limits": walk.limits,
            "depth_reached": walk.depth_reached,
            "starts": walk.starts,
            "hops": walk.hops.iter().map(explained).collect::<Vec<_>>(),
            "graph": walk.graph,
        })
    });
    Ok(json!({
        "name": saved.name,
        "saved_at": saved.saved_at,
        "graph": path,
        "graph_state": result.graph_state,
        "differences": result.differences,
        "saved_fingerprint": saved.fingerprint,
        "fingerprint": result.now,
        "missing_seeds": result.missing_seeds,
        "target_missing": result.target_missing,
        "unsure": result.unsure,
        "saved_walk": saved.walk,
        "walk": walk,
        "path": result.path,
        "path_same": result.path_same,
        "query": saved.query,
        "dashboard": saved.dashboard,
    }))
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

//! `khonsu analyze`: graph algorithms HawDB's Cypher lacks, after networkx.
//! See `khonsu_core::analysis`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Args, ValueEnum};
use khonsu_core::analysis::{self, CypherReader, LoadSpec, Network, NodeInfo, Partition};
use khonsu_core::{AccessMode, Graph, Language, OpenError, QueryError, QueryResult};
use serde_json::{json, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Algorithm {
    /// Size, density, components, clustering, cores and diameter.
    Summary,
    /// Most-connected nodes: in-, out- and total degree.
    Degree,
    /// PageRank: nodes that well-connected nodes point to.
    Pagerank,
    /// Betweenness centrality: nodes many shortest paths pass through.
    Betweenness,
    /// Closeness centrality: nodes near everything that reaches them.
    Closeness,
    /// Connected components (weak; --strong for strongly connected).
    Components,
    /// Louvain communities, with their modularity.
    Communities,
    /// Clustering coefficient and triangles per node.
    Clustering,
    /// k-core number per node.
    Kcore,
    /// A shortest path between --from and --to.
    Path,
}

#[derive(Args)]
pub struct AnalyzeArgs {
    algorithm: Algorithm,
    #[command(flatten)]
    db: DbArg,
    /// Analyze what this Cypher query returns (nodes and relationships, returned whole) instead of the whole graph.
    #[arg(long, conflicts_with_all = ["label", "rel_type"])]
    cypher: Option<String>,
    /// Only nodes with this label, and relationships between them. Repeat or separate with commas.
    #[arg(long, value_delimiter = ',')]
    label: Vec<String>,
    /// Only relationships of this type. Repeat or separate with commas.
    #[arg(long = "type", value_delimiter = ',')]
    rel_type: Vec<String>,
    /// Numeric relationship property used as weight (PageRank) or length (paths, betweenness, closeness).
    #[arg(long)]
    weight: Option<String>,
    /// Read every relationship both ways.
    #[arg(long)]
    undirected: bool,
    /// Rows to return, best first.
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// Betweenness: walk only this many source nodes and scale up (networkx's k). Default: all up to 5,000 nodes, else 500.
    #[arg(long)]
    sample: Option<usize>,
    /// PageRank damping factor.
    #[arg(long, default_value_t = 0.85)]
    alpha: f64,
    /// Communities: above 1 favors smaller communities, below 1 larger ones.
    #[arg(long, default_value_t = 1.0)]
    resolution: f64,
    /// Communities: fixes the order nodes are visited in, so a result repeats.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Components: strongly connected (following direction) instead of weakly.
    #[arg(long)]
    strong: bool,
    /// Path: the start node's ID (`id(n)`, `_id` in results).
    #[arg(long)]
    from: Option<i64>,
    /// Path: the end node's ID.
    #[arg(long)]
    to: Option<i64>,
    /// Store each analyzed node's result as this node property (the community or component number, the score, the degree or core), for dashboards and the canvas to sort, color and group by. Needs the graph free of the window.
    #[arg(long, value_name = "PROPERTY")]
    write: Option<String>,
}

/// Graphs above this many nodes get sampled betweenness unless --sample says otherwise.
const EXACT_BETWEENNESS: usize = 5_000;
const DEFAULT_SAMPLE: usize = 500;
/// Members listed per component or community.
const MEMBERS: usize = 10;

pub fn analyze(args: AnalyzeArgs) -> Result<Value, CliError> {
    if args.algorithm == Algorithm::Path && (args.from.is_none() || args.to.is_none()) {
        return Err(CliError::usage("path needs --from ID and --to ID (node IDs: `RETURN id(n)`, or `_id` in a result)"));
    }
    if args.write.is_some() && matches!(args.algorithm, Algorithm::Summary | Algorithm::Path) {
        return Err(CliError::usage("--write stores a result per node; summary and path have none"));
    }
    if let Some(property) = &args.write {
        if property.is_empty() || !property.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(CliError::usage("--write takes a plain property name: letters, digits and _"));
        }
    }
    let path = resolve_db(&args.db)?;
    let mode = if args.write.is_some() { AccessMode::ReadWrite } else { AccessMode::ReadOnly };
    let mut reader = match Graph::open(&path, mode, PROGRAM) {
        Ok(graph) => Reader::Graph(graph),
        // The window holds the graph: read through it, as `query` does.
        Err(OpenError::Locked(_)) if args.write.is_none() => Reader::Window(WindowReader(path.clone())),
        Err(e) => return Err(e.into()),
    };
    let spec = LoadSpec {
        cypher: args.cypher.clone(),
        labels: args.label.clone(),
        types: args.rel_type.clone(),
        weight: args.weight.clone(),
    };
    let started = Instant::now();
    let net = analysis::load(&mut reader, &spec)?;
    let loaded_ms = started.elapsed().as_millis() as u64;
    let directed = !args.undirected;
    let weighted = args.weight.is_some();
    let (mut out, values) = run(&args, &net, directed, weighted, &mut reader)?;
    if let (Some(property), Some(values), Reader::Graph(graph)) = (&args.write, values, &mut reader) {
        let written = analysis::write_back(graph, &net, property, &values)?;
        out["written"] = json!({ "property": property, "nodes": written });
    }
    out["algorithm"] = json!(args.algorithm.to_possible_value().map(|v| v.get_name().to_string()));
    out["graph"] = json!({
        "nodes": net.node_count(),
        "relationships": net.edge_count(),
        "directed": directed,
        "weighted": weighted,
    });
    out["load_ms"] = json!(loaded_ms);
    out["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    Ok(out)
}

/// The output, and each node's value for `--write` (by node index).
fn run(args: &AnalyzeArgs, net: &Network, directed: bool, weighted: bool, reader: &mut dyn CypherReader) -> Result<(Value, Option<Vec<Value>>), CliError> {
    let n = net.node_count();
    let floats = |values: &[f64]| Some(values.iter().map(|&v| json!(v)).collect::<Vec<_>>());
    Ok(match args.algorithm {
        Algorithm::Summary => (json!({ "summary": analysis::summary(net) }), None),
        Algorithm::Degree => {
            let degrees = analysis::degrees(net);
            let centrality = analysis::degree_centrality(net);
            let top = top_by(n, args.top, |v| (degrees[v].0 + degrees[v].1) as f64);
            let info = describe(reader, net, &top)?;
            let rows = top
                .iter()
                .map(|&v| {
                    let mut row = node_row(net, &info, v);
                    row["degree"] = json!(degrees[v].0 + degrees[v].1);
                    row["in"] = json!(degrees[v].0);
                    row["out"] = json!(degrees[v].1);
                    row["centrality"] = json!(centrality[v]);
                    row
                })
                .collect::<Vec<_>>();
            (json!({ "rows": rows }), Some(degrees.iter().map(|&(i, o)| json!(i + o)).collect()))
        }
        Algorithm::Pagerank => {
            let (scores, converged) = analysis::pagerank(net, args.alpha, directed, 100, 1e-6);
            let mut out = scored(reader, net, &scores, args.top, "pagerank")?;
            out["converged"] = json!(converged);
            (out, floats(&scores))
        }
        Algorithm::Betweenness => {
            let sample = args.sample.or((n > EXACT_BETWEENNESS).then_some(DEFAULT_SAMPLE));
            let scores = analysis::betweenness(net, directed, weighted, sample);
            let mut out = scored(reader, net, &scores, args.top, "betweenness")?;
            out["sampled_sources"] = json!(sample.filter(|&k| k < n));
            (out, floats(&scores))
        }
        Algorithm::Closeness => {
            let scores = analysis::closeness(net, directed, weighted);
            (scored(reader, net, &scores, args.top, "closeness")?, floats(&scores))
        }
        Algorithm::Kcore => {
            let core = analysis::core_number(net);
            let scores: Vec<f64> = core.iter().map(|&c| c as f64).collect();
            (scored(reader, net, &scores, args.top, "core")?, Some(core.iter().map(|&c| json!(c)).collect()))
        }
        Algorithm::Clustering => {
            let (triangles, coefficient) = analysis::clustering(net);
            // Most triangles first: a coefficient of 1 on two neighbors says little.
            let top = top_by(n, args.top, |v| triangles[v] as f64);
            let info = describe(reader, net, &top)?;
            let rows = top
                .iter()
                .map(|&v| {
                    let mut row = node_row(net, &info, v);
                    row["triangles"] = json!(triangles[v]);
                    row["clustering"] = json!(coefficient[v]);
                    row
                })
                .collect::<Vec<_>>();
            let average = if n > 0 { coefficient.iter().sum::<f64>() / n as f64 } else { 0.0 };
            let out = json!({ "rows": rows, "average_clustering": average, "triangles": triangles.iter().sum::<usize>() / 3 });
            (out, floats(&coefficient))
        }
        Algorithm::Components => {
            let partition = if args.strong { analysis::strong_components(net) } else { analysis::weak_components(net) };
            let mut out = groups(reader, net, &partition, args.top, "component")?;
            out["kind"] = json!(if args.strong { "strong" } else { "weak" });
            (out, Some(partition.of.iter().map(|&g| json!(g)).collect()))
        }
        Algorithm::Communities => {
            let (partition, modularity) = analysis::louvain(net, args.resolution, args.seed);
            let mut out = groups(reader, net, &partition, args.top, "community")?;
            out["modularity"] = json!(modularity);
            (out, Some(partition.of.iter().map(|&g| json!(g)).collect()))
        }
        Algorithm::Path => {
            let (from, to) = (args.from.unwrap_or_default(), args.to.unwrap_or_default());
            let index = |id: i64| {
                net.index_of(id)
                    .ok_or_else(|| CliError::new("not_found", format!("node {id} is not in the analyzed graph")))
            };
            let (s, t) = (index(from)?, index(to)?);
            let out = match analysis::shortest_path(net, s, t, directed, weighted) {
                None => json!({ "found": false, "rows": [] }),
                Some((path, length)) => {
                    let info = describe(reader, net, &path)?;
                    let rows = path.iter().map(|&v| node_row(net, &info, v)).collect::<Vec<_>>();
                    json!({ "found": true, "hops": path.len() - 1, "length": length, "rows": rows })
                }
            };
            (out, None)
        }
    })
}

/// Node indexes with the highest `score`, best first, ties by ID.
fn top_by(n: usize, top: usize, score: impl Fn(usize) -> f64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    order.truncate(top);
    order
}

fn scored(reader: &mut dyn CypherReader, net: &Network, scores: &[f64], top: usize, key: &str) -> Result<Value, CliError> {
    let order = top_by(net.node_count(), top, |v| scores[v]);
    let info = describe(reader, net, &order)?;
    let rows = order
        .iter()
        .map(|&v| {
            let mut row = node_row(net, &info, v);
            row[key] = json!(scores[v]);
            row
        })
        .collect::<Vec<_>>();
    Ok(json!({ "rows": rows }))
}

/// Components or communities, largest first, each with its first members.
fn groups(reader: &mut dyn CypherReader, net: &Network, partition: &Partition, top: usize, key: &str) -> Result<Value, CliError> {
    let shown: Vec<&Vec<usize>> = partition.groups.iter().take(top).collect();
    let members: Vec<usize> = shown.iter().flat_map(|g| g.iter().take(MEMBERS).copied()).collect();
    let info = describe(reader, net, &members)?;
    let rows = shown
        .iter()
        .enumerate()
        .map(|(i, group)| {
            json!({
                key: i,
                "size": group.len(),
                "members": group.iter().take(MEMBERS).map(|&v| node_row(net, &info, v)).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let singletons = partition.groups.iter().filter(|g| g.len() == 1).count();
    Ok(json!({ "count": partition.groups.len(), "singletons": singletons, "rows": rows }))
}

fn describe(reader: &mut dyn CypherReader, net: &Network, nodes: &[usize]) -> Result<HashMap<i64, NodeInfo>, CliError> {
    let ids: Vec<i64> = nodes.iter().map(|&v| net.id(v)).collect();
    Ok(analysis::describe_nodes(reader, &ids)?)
}

fn node_row(net: &Network, info: &HashMap<i64, NodeInfo>, v: usize) -> Value {
    let id = net.id(v);
    let node = info.get(&id).cloned().unwrap_or_default();
    json!({ "id": id, "labels": node.labels, "name": node.name })
}

/// Where the analysis reads: the graph, open here, or the window holding it.
enum Reader {
    Graph(Graph),
    Window(WindowReader),
}

impl CypherReader for Reader {
    fn cypher(&mut self, text: &str) -> Result<QueryResult, QueryError> {
        match self {
            Self::Graph(graph) => graph.cypher(text),
            Self::Window(window) => window.cypher(text),
        }
    }
}

/// Reads through the running window, which holds the graph.
struct WindowReader(PathBuf);

impl CypherReader for WindowReader {
    fn cypher(&mut self, text: &str) -> Result<QueryResult, QueryError> {
        let failed = |message: String| QueryError {
            kind: "window".into(),
            message,
            line: None,
            column: None,
        };
        let answer = crate::dash::query_via_window(Path::new(&self.0), Language::Cypher, text, usize::MAX)
            .ok_or_else(|| failed("the window stopped answering".into()))?
            .map_err(|e| QueryError {
                kind: e.kind,
                message: e.message,
                line: None,
                column: None,
            })?;
        serde_json::from_value(answer).map_err(|e| failed(e.to_string()))
    }
}

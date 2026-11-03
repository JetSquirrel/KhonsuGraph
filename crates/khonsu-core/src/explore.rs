//! Following a suspected chain from chosen nodes without loading the graph.
//!
//! A breadth-first walk with one parameterized query per step, so every cap is
//! enforced here and cancellation is checked between queries. HawDB takes one
//! relationship type on a variable-length pattern and no `type(r)` in a
//! `WHERE`, so types go in the pattern, one query each.
//!
//! What a walk finds is a path in the data, not proof that anything can reach
//! anything: see `docs/investigation-gates.md`.

use crate::engine::{Graph, QueryError};
use crate::result::{value_to_json, GraphCollector, GraphData, GraphEdge, GraphNode};
use hawdb::Value;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_MAX_HOPS: u32 = 3;
pub const DEFAULT_MAX_NODES: usize = 200;
pub const DEFAULT_MAX_EDGES: usize = 500;
/// The most a request may ask for, whatever the person types.
pub const MAX_HOPS: u32 = 16;
pub const MAX_NODES: usize = 2000;
pub const MAX_EDGES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Along the arrows.
    Out,
    /// Against the arrows.
    In,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreRequest {
    pub starts: Vec<i64>,
    pub direction: Direction,
    /// Relationship types to follow; empty follows every type.
    pub rel_types: Vec<String>,
    pub max_hops: u32,
    pub max_nodes: usize,
    pub max_edges: usize,
}

impl ExploreRequest {
    pub fn new(starts: Vec<i64>) -> Self {
        Self {
            starts,
            direction: Direction::Out,
            rel_types: Vec::new(),
            max_hops: DEFAULT_MAX_HOPS,
            max_nodes: DEFAULT_MAX_NODES,
            max_edges: DEFAULT_MAX_EDGES,
        }
    }

    fn validate(&self) -> Result<(), QueryError> {
        let refuse = |message: String| Err(QueryError::new("semantic", message));
        if self.starts.is_empty() {
            return refuse("choose at least one node to start from".into());
        }
        if self.max_hops == 0 || self.max_hops > MAX_HOPS {
            return refuse(format!("hops must be between 1 and {MAX_HOPS}, not {}", self.max_hops));
        }
        if self.max_nodes < self.starts.len() || self.max_nodes > MAX_NODES {
            return refuse(format!("the node cap must be between {} and {MAX_NODES}, not {}", self.starts.len(), self.max_nodes));
        }
        if self.max_edges == 0 || self.max_edges > MAX_EDGES {
            return refuse(format!("the relationship cap must be between 1 and {MAX_EDGES}, not {}", self.max_edges));
        }
        // A type goes into the query text, since HawDB cannot bind one.
        if let Some(bad) = self.rel_types.iter().find(|name| !is_identifier(name)) {
            return refuse(format!("{bad:?} is not a relationship type name"));
        }
        Ok(())
    }
}

pub(crate) fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One relationship the walk followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// Steps from the nearest start: 1 is a start's own relationship.
    pub depth: u32,
    pub from: i64,
    pub to: i64,
    pub edge: i64,
    pub rel_type: String,
    /// True when the walk went along the arrow, false when against it.
    pub along: bool,
}

/// A cap that stopped the walk from showing everything it could reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Limit {
    Hops,
    Nodes,
    Edges,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Exploration {
    /// The requested starts that are in the graph.
    pub starts: Vec<i64>,
    pub graph: GraphData,
    /// In the order they were found, nearest first.
    pub hops: Vec<Hop>,
    /// How far the walk found anything.
    pub depth_reached: u32,
    /// Empty means the walk saw everything its filters allow.
    pub limits: Vec<Limit>,
    /// Stopped early on request; `graph` holds what was found until then.
    pub cancelled: bool,
    /// Requested starts that are not in the graph.
    pub missing_starts: Vec<i64>,
}

impl Exploration {
    /// Whether the result is everything the filters allow. An incomplete walk
    /// that found nothing has not shown that nothing is there.
    pub fn is_complete(&self) -> bool {
        self.limits.is_empty() && !self.cancelled
    }

    /// The hops that first reached `target`, from a start: a fewest-hops path
    /// among what the walk found. `None` when it did not reach it, and empty
    /// when `target` is a start.
    pub fn path_to(&self, target: i64) -> Option<Vec<&Hop>> {
        if !self.graph.nodes.iter().any(|node| node.id == target) {
            return None;
        }
        // The hop that first reached each node. A walk can arrive back at a
        // start around a cycle, which does not count as reaching it.
        let mut first: BTreeMap<i64, &Hop> = BTreeMap::new();
        for hop in self.hops.iter().filter(|hop| !self.starts.contains(&hop.to)) {
            first.entry(hop.to).or_insert(hop);
        }
        // Each hop's `from` was reached a step earlier, so this ends at a start.
        let mut path = Vec::new();
        let mut at = target;
        while let Some(hop) = first.get(&at) {
            path.push(*hop);
            at = hop.from;
        }
        path.reverse();
        Some(path)
    }

    /// One hop in words, in the direction it was walked:
    /// `Internet -[ROUTES_TO]-> shop`, `web-1 <-[ROUTES_TO]- web`.
    pub fn explain(&self, hop: &Hop) -> String {
        explain_hop(&self.graph.nodes, hop)
    }
}

/// One hop in words, with names from `nodes`.
pub(crate) fn explain_hop(nodes: &[GraphNode], hop: &Hop) -> String {
    let name = |id: i64| nodes.iter().find(|node| node.id == id).map(node_name).unwrap_or_else(|| format!("#{id}"));
    let (from, to, rel) = (name(hop.from), name(hop.to), &hop.rel_type);
    if hop.along {
        format!("{from} -[{rel}]-> {to}")
    } else {
        format!("{from} <-[{rel}]- {to}")
    }
}

pub(crate) fn node_name(node: &GraphNode) -> String {
    for key in ["name", "title", "khonsu_id", "id"] {
        match node.properties.get(key) {
            Some(Json::String(text)) if !text.is_empty() => return text.clone(),
            Some(value @ Json::Number(_)) => return value.to_string(),
            _ => {}
        }
    }
    format!("{} #{}", node.labels.first().map(String::as_str).unwrap_or("node"), node.id)
}

impl Graph {
    /// Walks from `request.starts`. `cancel` is checked before each query; a
    /// query already running finishes first.
    pub fn explore(&mut self, request: &ExploreRequest, cancel: &AtomicBool) -> Result<Exploration, QueryError> {
        request.validate()?;
        let found = self.nodes_by_id(&request.starts)?;
        let mut result = Exploration::default();
        let known: BTreeSet<i64> = found.nodes.iter().map(|node| node.id).collect();
        result.missing_starts = request.starts.iter().copied().filter(|id| !known.contains(id)).collect();
        result.missing_starts.sort_unstable();
        result.missing_starts.dedup();

        let mut nodes: BTreeMap<i64, GraphNode> = found.nodes.into_iter().map(|node| (node.id, node)).collect();
        let mut edges: BTreeMap<i64, GraphEdge> = BTreeMap::new();
        let mut frontier: Vec<i64> = known.into_iter().collect();
        result.starts = frontier.clone();
        let mut limits = BTreeSet::new();

        for depth in 1..=request.max_hops {
            if frontier.is_empty() {
                break;
            }
            let Some(walks) = self.step(request, &frontier, cancel)? else {
                result.cancelled = true;
                break;
            };
            let mut next = Vec::new();
            for walk in walks {
                if edges.contains_key(&walk.edge.id) {
                    continue;
                }
                if edges.len() == request.max_edges {
                    limits.insert(Limit::Edges);
                    break;
                }
                let fresh = !nodes.contains_key(&walk.to.id);
                if fresh && nodes.len() == request.max_nodes {
                    limits.insert(Limit::Nodes);
                    continue;
                }
                result.hops.push(Hop {
                    depth,
                    from: walk.from,
                    to: walk.to.id,
                    edge: walk.edge.id,
                    rel_type: walk.edge.rel_type.clone(),
                    along: walk.along,
                });
                result.depth_reached = depth;
                if fresh {
                    next.push(walk.to.id);
                    nodes.insert(walk.to.id, walk.to);
                }
                edges.insert(walk.edge.id, walk.edge);
            }
            next.sort_unstable();
            frontier = next;
            if !limits.is_empty() {
                frontier.clear();
            }
        }

        // One step past the last hop, only to say whether the cap cut anything.
        if !result.cancelled && !frontier.is_empty() {
            match self.step(request, &frontier, cancel)? {
                Some(walks) if walks.iter().any(|walk| !edges.contains_key(&walk.edge.id)) => {
                    limits.insert(Limit::Hops);
                }
                Some(_) => {}
                None => result.cancelled = true,
            }
        }

        result.limits = limits.into_iter().collect();
        result.graph = GraphData {
            nodes: nodes.into_values().collect(),
            edges: edges.into_values().collect(),
        };
        Ok(result)
    }

    /// Every relationship the filters allow out of `frontier`, by edge ID so
    /// that a repeat of the same walk gives the same result. `None` when
    /// cancelled. Each query can return one row more than the edge cap, which
    /// is enough to show the cap was exceeded even when all the others have
    /// been seen already.
    fn step(&mut self, request: &ExploreRequest, frontier: &[i64], cancel: &AtomicBool) -> Result<Option<Vec<Walk>>, QueryError> {
        let types: Vec<Option<&str>> = if request.rel_types.is_empty() {
            vec![None]
        } else {
            request.rel_types.iter().map(|name| Some(name.as_str())).collect()
        };
        let ways: &[bool] = match request.direction {
            Direction::Out => &[true],
            Direction::In => &[false],
            Direction::Both => &[true, false],
        };
        let ids = Value::List(frontier.iter().map(|id| Value::Int(*id)).collect());
        let params = BTreeMap::from([("ids".to_string(), ids)]);
        let rows = request.max_edges + 1;

        let mut walks = Vec::new();
        for along in ways.iter().copied() {
            for rel_type in &types {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let rel = rel_type.map(|name| format!("r:{name}")).unwrap_or_else(|| "r".into());
                let pattern = if along { format!("(n)-[{rel}]->(m)") } else { format!("(n)<-[{rel}]-(m)") };
                let text = format!("MATCH {pattern} WHERE id(n) IN $ids RETURN n, r, m ORDER BY id(r) LIMIT {rows}");
                let mut collector = GraphCollector::default();
                for row in self.raw_cypher(&text, &params)? {
                    row.iter().for_each(|value| collector.visit(&value_to_json(value)));
                }
                let data = collector.finish().unwrap_or_default();
                let by_id: BTreeMap<i64, GraphNode> = data.nodes.into_iter().map(|node| (node.id, node)).collect();
                for edge in data.edges {
                    let (from, to) = if along { (edge.source, edge.target) } else { (edge.target, edge.source) };
                    if let Some(node) = by_id.get(&to) {
                        walks.push(Walk {
                            from,
                            to: node.clone(),
                            edge,
                            along,
                        });
                    }
                }
            }
        }
        walks.sort_by_key(|walk| (walk.edge.id, !walk.along));
        Ok(Some(walks))
    }
}

struct Walk {
    from: i64,
    to: GraphNode,
    edge: GraphEdge,
    along: bool,
}

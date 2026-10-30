//! Graph analysis HawDB's Cypher does not do, after networkx: centrality,
//! components, communities, shortest paths, clustering and cores.
//!
//! A graph — the whole of it, the labels and relationship types asked for,
//! or the nodes and relationships a Cypher query returns — is read into a
//! [`Network`] of node IDs and weighted adjacency, and the algorithms run on
//! that in memory. Each returns one score (or group) per node, so its
//! result can be listed best first, or written back as a node property for
//! dashboards and the canvas to sort, color and group by.
//!
//! The algorithms follow networkx's definitions and defaults, so a result
//! can be checked against it: a directed graph stays directed unless asked
//! otherwise, and an algorithm networkx only defines for undirected graphs
//! (clustering, cores, communities) reads every relationship both ways.

mod algorithms;
mod load;
mod network;

pub use algorithms::*;
pub use load::{describe_nodes, load, CypherReader, LoadSpec, NodeInfo};
pub use network::Network;

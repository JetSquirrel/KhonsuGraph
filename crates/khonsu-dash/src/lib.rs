//! `.dash` dashboards for KhonsuGraph: a file of query, plot and filter
//! blocks, ported from DuckLocal. The queries are Cypher or SQL over the open
//! HawDB graph; a `graph` plot draws nodes and relationships.
//!
//! ```text
//! src/syntax.rs    the text as a tree: blocks, attributes, heredocs
//! src/model.rs     what the tree means, checked without a graph
//! src/filter.rs    `filter` blocks: a click's pick, as `$name` predicates
//! src/check.rs     `khonsu check`: run every query read-only, check columns
//! src/comments.rs  review threads on plots, kept beside the spec
//! src/presence.rs  an agent at work on a spec, for the window to show
//! src/remote.rs    the line between khonsu commands and the running window
//! ```

pub mod check;
pub mod comments;
pub mod filter;
pub mod geo;
pub mod model;
pub mod presence;
pub mod remote;
pub mod syntax;

pub use model::{parse, validate, validate_partial, Diagnostic, Filter, Plot, Query, Spec};

//! Shared core of KhonsuGraph: HawDB access, results, schema, the data-source
//! plugin host and per-user settings. The desktop app and the `khonsu` CLI are
//! thin layers over this crate.

pub mod engine;
pub mod lock;
pub mod merge;
pub mod plugin;
pub mod result;
pub mod schema;
pub mod secrets;
pub mod settings;
pub mod util;

pub use engine::{AccessMode, Graph, Language, OpenError, QueryError, QueryOptions};
pub use khonsu_plugin_protocol as protocol;
pub use result::{GraphData, GraphEdge, GraphNode, QueryResult};
pub use schema::GraphSchema;

//! The window's one open graph, like DuckLocal's one connection. Every call
//! blocks on the engine; UI code reaches it through `smol::unblock`.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use khonsu_core::{AccessMode, Graph, GraphData, GraphSchema, Language, OpenError, QueryError, QueryOptions, QueryResult};
use serde_json::Map;

/// The name this process records in the graph's lock, shown to a CLI that
/// finds the graph busy.
pub const PROGRAM: &str = "KhonsuGraph";

/// Rows a query keeps for the grid; HawDB's own budget still applies.
pub const MAX_ROWS: usize = 5000;

static GRAPH: LazyLock<Mutex<Option<Graph>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, PartialEq)]
pub struct GraphInfo {
    pub path: PathBuf,
    pub read_only: bool,
}

impl GraphInfo {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

fn info(graph: &Graph) -> GraphInfo {
    GraphInfo {
        path: graph.path().to_path_buf(),
        read_only: graph.mode() == AccessMode::ReadOnly,
    }
}

/// Opens an existing graph, releasing the current one first so reopening the
/// same directory works.
pub fn open(path: &Path, read_only: bool) -> Result<GraphInfo, OpenError> {
    let mut guard = GRAPH.lock().unwrap_or_else(PoisonError::into_inner);
    *guard = None;
    let mode = if read_only { AccessMode::ReadOnly } else { AccessMode::ReadWrite };
    let graph = Graph::open(path, mode, PROGRAM)?;
    let info = info(&graph);
    *guard = Some(graph);
    Ok(info)
}

/// Creates a new, empty graph in an empty or new directory.
pub fn create(path: &Path) -> Result<GraphInfo, OpenError> {
    let mut guard = GRAPH.lock().unwrap_or_else(PoisonError::into_inner);
    *guard = None;
    let graph = Graph::create(path, PROGRAM)?;
    let info = info(&graph);
    *guard = Some(graph);
    Ok(info)
}

pub fn close() {
    *GRAPH.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn with_graph<T>(f: impl FnOnce(&mut Graph) -> Result<T, QueryError>) -> Result<T, QueryError> {
    let mut guard = GRAPH.lock().unwrap_or_else(PoisonError::into_inner);
    let graph = guard.as_mut().ok_or_else(|| QueryError {
        kind: "no_graph".into(),
        message: crate::i18n::tr("error.no_graph").into(),
        line: None,
        column: None,
    })?;
    f(graph)
}

pub fn run(language: Language, text: &str) -> Result<QueryResult, QueryError> {
    with_graph(|g| g.run(language, text, &Map::new(), QueryOptions { max_rows: MAX_ROWS }))
}

pub fn explain(language: Language, text: &str) -> Result<String, QueryError> {
    with_graph(|g| g.explain(language, text))
}

pub fn schema() -> Result<GraphSchema, QueryError> {
    with_graph(|g| g.schema())
}

pub fn neighbors(id: i64) -> Result<GraphData, QueryError> {
    with_graph(|g| g.neighbors(id, 200))
}

pub fn nodes_by_id(ids: &[i64]) -> Result<GraphData, QueryError> {
    with_graph(|g| g.nodes_by_id(ids))
}

/// `~/x` as the home directory's `x`; anything else as written.
pub fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().join(rest).display().to_string())
            .unwrap_or_else(|| path.to_string()),
        None => path.to_string(),
    }
}

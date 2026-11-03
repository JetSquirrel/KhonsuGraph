//! The window's one open graph, like DuckLocal's one connection. Every call
//! blocks on the engine; UI code reaches it through `smol::unblock`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{LazyLock, Mutex, PoisonError};

use khonsu_core::explore::{Exploration, ExploreRequest};
use khonsu_core::investigation::{Capture, Investigation, Reopened};
use khonsu_core::plugin::PluginError;
use khonsu_core::plugin_analysis::{self, Offered, Report};
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

/// Holds the graph for the whole walk, as any call here does; `cancel` stops
/// it between queries.
pub fn explore(request: &ExploreRequest, cancel: &AtomicBool) -> Result<Exploration, QueryError> {
    with_graph(|g| g.explore(request, cancel))
}

/// Records `capture` against the open graph, which holds it while it reads.
pub fn capture_investigation(capture: Capture) -> Result<Investigation, QueryError> {
    let sources = khonsu_core::settings::load_sources();
    with_graph(|g| {
        let path = g.path().to_path_buf();
        g.capture_investigation(&path, capture, &sources, &AtomicBool::new(false))
    })
}

/// Runs a saved investigation again on the open graph.
pub fn reopen_investigation(saved: &Investigation) -> Result<Reopened, QueryError> {
    let sources = khonsu_core::settings::load_sources();
    with_graph(|g| g.reopen_investigation(saved, &sources, &AtomicBool::new(false)))
}

/// Runs an analysis a plugin offers. It holds the graph only while reading, not
/// while the plugin works. `Ok(None)` when `cancel` stopped it.
pub fn run_analysis(offered: &Offered, cancel: &AtomicBool) -> Result<Option<Report>, String> {
    let sources = khonsu_core::settings::load_sources();
    let input = with_graph(|g| g.read_analysis_input(&offered.info)).map_err(|error| error.message)?;
    match plugin_analysis::run(offered, &input, &sources, cancel, |_| {}) {
        Ok(report) => Ok(Some(report)),
        Err(PluginError::Cancelled) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub fn nodes_by_id(ids: &[i64]) -> Result<GraphData, QueryError> {
    with_graph(|g| g.nodes_by_id(ids))
}


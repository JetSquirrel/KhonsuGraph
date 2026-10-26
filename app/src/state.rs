//! Shared window state: which graph is open, its schema, its data sources and
//! the query history. Views subscribe to the events below, as in DuckLocal.

use gpui_kit::*;
use khonsu_core::settings::{self, HistoryEntry, Settings, Source};
use khonsu_core::GraphSchema;

use crate::db::GraphInfo;

pub struct GraphChanged;
pub struct SchemaChanged;
pub struct SourcesChanged;
pub struct HistoryChanged;
pub struct OpenStateChanged;
pub struct QueryStatsChanged;
pub struct SidebarToggled;

#[derive(Clone, Debug)]
pub struct QueryStats {
    pub elapsed_ms: u64,
    pub rows: usize,
    pub nodes: usize,
    pub edges: usize,
}

pub struct AppState {
    pub graph: Option<GraphInfo>,
    pub schema: Option<GraphSchema>,
    pub schema_error: Option<String>,
    pub sources: Vec<Source>,
    pub history: Vec<HistoryEntry>,
    pub recent: Vec<std::path::PathBuf>,
    pub last_query: Option<QueryStats>,
    /// The source being synced, if any.
    pub syncing: Option<String>,
    opening: bool,
    sidebar_collapsed: bool,
}

impl EventEmitter<GraphChanged> for AppState {}
impl EventEmitter<SchemaChanged> for AppState {}
impl EventEmitter<SourcesChanged> for AppState {}
impl EventEmitter<HistoryChanged> for AppState {}
impl EventEmitter<OpenStateChanged> for AppState {}
impl EventEmitter<QueryStatsChanged> for AppState {}
impl EventEmitter<SidebarToggled> for AppState {}

impl AppState {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            graph: None,
            schema: None,
            schema_error: None,
            sources: Vec::new(),
            history: Vec::new(),
            recent: Settings::load().recent,
            last_query: None,
            syncing: None,
            opening: false,
            sidebar_collapsed: false,
        }
    }

    pub fn is_opening(&self) -> bool {
        self.opening
    }

    pub fn begin_open(&mut self, cx: &mut Context<Self>) {
        self.opening = true;
        cx.emit(OpenStateChanged);
        cx.notify();
    }

    pub fn end_open(&mut self, cx: &mut Context<Self>) {
        self.opening = false;
        cx.emit(OpenStateChanged);
        cx.notify();
    }

    pub fn is_sidebar_collapsed(&self) -> bool {
        self.sidebar_collapsed
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.emit(SidebarToggled);
        cx.notify();
    }

    pub fn read_only(&self) -> bool {
        self.graph.as_ref().is_some_and(|g| g.read_only)
    }

    /// A graph was opened (or closed, with `None`). Remembers it as the one
    /// to reopen next launch.
    pub fn set_graph(&mut self, graph: Option<GraphInfo>, cx: &mut Context<Self>) {
        let mut stored = Settings::load();
        match &graph {
            Some(info) => stored.note_opened(&info.path),
            None => stored.last_opened = None,
        }
        let _ = stored.save();
        self.recent = stored.recent;
        self.graph = graph;
        crate::ui::palette::forget_labels();
        self.schema = None;
        self.schema_error = None;
        self.last_query = None;
        self.reload_lists(cx);
        cx.emit(GraphChanged);
        cx.emit(SchemaChanged);
        cx.notify();
    }

    pub fn forget_recent(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let mut stored = Settings::load();
        stored.recent.retain(|p| p != path);
        if stored.last_opened.as_deref() == Some(path) {
            stored.last_opened = None;
        }
        let _ = stored.save();
        self.recent = stored.recent;
        cx.notify();
    }

    pub fn set_schema(&mut self, schema: Result<GraphSchema, String>, cx: &mut Context<Self>) {
        match schema {
            Ok(schema) => {
                crate::ui::palette::register_labels(schema.labels.iter().map(|l| l.name.as_str()));
                self.schema = Some(schema);
                self.schema_error = None;
            }
            Err(e) => self.schema_error = Some(e),
        }
        cx.emit(SchemaChanged);
        cx.notify();
    }

    /// Sources and history belong to the open graph; reread both.
    pub fn reload_lists(&mut self, cx: &mut Context<Self>) {
        let current = self.graph.as_ref().map(|g| g.path.clone());
        self.sources = settings::load_sources()
            .into_iter()
            .filter(|s| current.as_ref() == Some(&s.graph))
            .collect();
        self.history = settings::load_history()
            .into_iter()
            .filter(|h| current.as_ref() == Some(&h.graph))
            .collect();
        cx.emit(SourcesChanged);
        cx.emit(HistoryChanged);
        cx.notify();
    }

    pub fn set_last_query(&mut self, stats: Option<QueryStats>, cx: &mut Context<Self>) {
        self.last_query = stats;
        cx.emit(QueryStatsChanged);
        cx.notify();
    }

    pub fn set_syncing(&mut self, source: Option<String>, cx: &mut Context<Self>) {
        self.syncing = source;
        cx.emit(SourcesChanged);
        cx.notify();
    }
}

/// `1234 ms` as `1.23 s`, as DuckLocal shows durations.
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.2} s", ms as f64 / 1000.0)
    }
}

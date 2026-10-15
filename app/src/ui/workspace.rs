//! Center workspace, after DuckLocal's: query tabs (Cypher or SQL), the
//! toolbar, the editor, and the tab's own results below a resizable split.
//! With no graph open it is the welcome screen instead.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::settings::{self, HistoryEntry};
use khonsu_core::{util, Language};
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable, WindowExt};

use crate::i18n::{tr, trf};
use crate::spec::view::{Dashboard, DashboardEvent};
use crate::state::{AppState, GraphChanged, QueryStats};
use crate::ui::results::ResultsPanel;
use crate::ui::{ExplainQuery, RunQuery, SaveSpec, RUN_QUERY_KEYSTROKE, WORKSPACE_KEY_CONTEXT};

const RESULTS_PANEL_DEFAULT: f32 = 540.;
/// Statements that may change labels, types or tables: the sidebar reloads
/// after them.
const WRITE_WORDS: &[&str] = &["CREATE", "MERGE", "DELETE", "SET", "INSERT", "DROP", "ALTER", "UPDATE", "REMOVE"];

pub struct QueryTab {
    pub id: u64,
    pub language: Language,
    pub editor: Entity<EditorState>,
    pub results: Entity<ResultsPanel>,
}

/// A `.dash` spec, open as a tab, as in DuckLocal.
pub struct DashboardTab {
    pub id: u64,
    pub title: SharedString,
    pub path: PathBuf,
    pub host: Entity<Dashboard>,
    /// What the dashboard asks of the workspace; dropped with the tab.
    _events: Subscription,
}

pub enum WorkspaceTab {
    Query(QueryTab),
    Dashboard(DashboardTab),
}

impl WorkspaceTab {
    pub fn id(&self) -> u64 {
        match self {
            WorkspaceTab::Query(tab) => tab.id,
            WorkspaceTab::Dashboard(tab) => tab.id,
        }
    }

    pub fn as_query(&self) -> Option<&QueryTab> {
        match self {
            WorkspaceTab::Query(tab) => Some(tab),
            WorkspaceTab::Dashboard(_) => None,
        }
    }

    pub fn as_dashboard(&self) -> Option<&DashboardTab> {
        match self {
            WorkspaceTab::Dashboard(tab) => Some(tab),
            WorkspaceTab::Query(_) => None,
        }
    }
}

/// Tabs as remembered between launches, per graph: a query's text, or the
/// path of a dashboard spec.
#[derive(Serialize, Deserialize)]
struct SavedTab {
    #[serde(default = "cypher")]
    language: Language,
    #[serde(default)]
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dashboard: Option<PathBuf>,
}

fn cypher() -> Language {
    Language::Cypher
}

pub struct Workspace {
    state: Entity<AppState>,
    tabs: Vec<WorkspaceTab>,
    active: usize,
    next_tab_id: u64,
    running: bool,
    _subscriptions: Vec<Subscription>,
}

fn default_text(language: Language) -> &'static str {
    match language {
        Language::Cypher => "MATCH (n)-[r]->(m)\nRETURN n, r, m\nLIMIT 100",
        Language::Sql => "SELECT *\nFROM information_schema.tables\nLIMIT 100",
    }
}

fn tabs_key(graph: &std::path::Path) -> String {
    format!("tabs:{}", graph.display())
}

impl Workspace {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe_in(&state, _window, |this, _, _: &GraphChanged, window, cx| {
            this.restore_tabs(window, cx);
        })];
        Self {
            state,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            running: false,
            _subscriptions: subscriptions,
        }
    }

    fn new_tab(&mut self, language: Language, text: &str, window: &mut Window, cx: &mut Context<Self>) -> QueryTab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                // HawDB's Cypher shares most of its keywords with SQL; the SQL
                // grammar is the closest highlighting the toolkit ships.
                .language("sql")
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
        });
        editor.update(cx, |editor, cx| editor.set_value(text.to_string(), window, cx));
        // ⌘↵ inside the editor arrives as PressEnter (see DuckLocal): undo the
        // newline it inserted, then run.
        let subscription = cx.subscribe_in(&editor, window, |this, editor, event, window, cx| {
            match event {
                InputEvent::Change => this.remember_tabs(cx),
                InputEvent::PressEnter { secondary: true, .. } => {
                    remove_secondary_enter_newline(editor, window, cx);
                    this.run_active(window, cx);
                }
                _ => {}
            }
        });
        self._subscriptions.push(subscription);
        let results = cx.new(|cx| ResultsPanel::new(self.state.clone(), window, cx));
        QueryTab {
            id,
            language,
            editor,
            results,
        }
    }

    /// The open graph changed: bring back its tabs, or start one.
    fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.clear();
        self.active = 0;
        let Some(graph) = self.state.read(cx).graph.clone() else {
            cx.notify();
            return;
        };
        let saved: Vec<SavedTab> = crate::prefs::get(&tabs_key(&graph.path))
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let mut problems = Vec::new();
        for saved in saved {
            match saved.dashboard {
                Some(path) if path.is_file() => self.push_dashboard(path, window, cx),
                Some(path) => problems.push(path.display().to_string()),
                None => {
                    let tab = self.new_tab(saved.language, &saved.text, window, cx);
                    self.tabs.push(WorkspaceTab::Query(tab));
                }
            }
        }
        if !self.tabs.iter().any(|t| t.as_query().is_some()) {
            let tab = self.new_tab(Language::Cypher, default_text(Language::Cypher), window, cx);
            self.tabs.insert(0, WorkspaceTab::Query(tab));
        }
        self.active = 0;
        // A remembered dashboard that is gone is named rather than dropped
        // silently.
        for path in problems {
            window.push_notification(trf("dashboard.restore.gone_path", &[&path]), cx);
        }
        self.focus_active_editor(window, cx);
        cx.notify();
    }

    fn remember_tabs(&self, cx: &App) {
        let Some(graph) = self.state.read(cx).graph.clone() else {
            return;
        };
        let saved: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| match t {
                WorkspaceTab::Query(t) => SavedTab {
                    language: t.language,
                    text: t.editor.read(cx).value().to_string(),
                    dashboard: None,
                },
                WorkspaceTab::Dashboard(t) => SavedTab {
                    language: Language::Cypher,
                    text: String::new(),
                    dashboard: Some(t.path.clone()),
                },
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&saved) {
            let _ = crate::prefs::set(&tabs_key(&graph.path), &json);
        }
    }

    pub fn add_tab(&mut self, language: Language, text: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).graph.is_none() {
            return;
        }
        let tab = self.new_tab(language, text.unwrap_or(""), window, cx);
        self.tabs.push(WorkspaceTab::Query(tab));
        self.active = self.tabs.len() - 1;
        self.remember_tabs(cx);
        self.focus_active_editor(window, cx);
        cx.notify();
    }

    fn push_dashboard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = crate::spec::tabs::title_for(&path);
        let host = cx.new(|cx| Dashboard::new(id, path.clone(), cx));
        let events = cx.subscribe_in(&host, window, |this, _, event, window, cx| match event {
            DashboardEvent::OpenSql { sql, title: _ } => {
                // A dashboard query opened for editing: Cypher unless it
                // reads like SQL.
                let language = if sql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
                    Language::Sql
                } else {
                    Language::Cypher
                };
                this.add_tab(language, Some(sql), window, cx);
            }
        });
        self.tabs.push(WorkspaceTab::Dashboard(DashboardTab {
            id,
            title: title.into(),
            path,
            host,
            _events: events,
        }));
    }

    /// Open a `.dash` spec as a tab, or bring its tab forward when one is
    /// open already.
    pub fn open_dashboard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).graph.is_none() {
            window.push_notification(Notification::error(tr("dashboard.needs_graph")), cx);
            return;
        }
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| tab.as_dashboard().is_some_and(|d| same_file(&d.path, &path)))
        {
            self.activate_tab(ix, window, cx);
            return;
        }
        self.push_dashboard(path, window, cx);
        self.active = self.tabs.len() - 1;
        self.remember_tabs(cx);
        cx.notify();
    }

    pub fn pick_dashboard_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("dashboard.picker.prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if crate::spec::tabs::is_spec(&path) {
                    this.open_dashboard(path, window, cx);
                } else {
                    window.push_notification(trf("dashboard.picker.not_a_spec", &[&path.to_string_lossy()]), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// An agent's presence on a spec reaches every tab showing it.
    pub fn apply_presence(&mut self, update: &crate::spec::presence::Update, cx: &mut Context<Self>) {
        let path = PathBuf::from(&update.path);
        for tab in &self.tabs {
            if let WorkspaceTab::Dashboard(tab) = tab {
                if same_file(&tab.path, &path) {
                    tab.host.update(cx, |dashboard, cx| dashboard.apply_presence(update, cx));
                }
            }
        }
        cx.notify();
    }

    fn save_active_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.tabs.get(self.active).and_then(WorkspaceTab::as_dashboard).map(|t| t.host.clone()) else {
            return;
        };
        if let Err(error) = host.update(cx, |dashboard, cx| dashboard.save(cx)) {
            window.push_notification(Notification::error(error), cx);
        }
    }

    /// What the window shows, for `khonsu open --state`: each tab's query and
    /// result summary, or its dashboard's.
    pub fn snapshot(&self, only: Option<&std::path::Path>, active_only: bool, cx: &App) -> serde_json::Value {
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, _)| !active_only || *ix == self.active)
            .filter(|(_, tab)| match (only, tab) {
                (Some(path), WorkspaceTab::Dashboard(d)) => same_file(&d.path, path),
                (Some(_), WorkspaceTab::Query(_)) => false,
                (None, _) => true,
            })
            .map(|(ix, tab)| match tab {
                WorkspaceTab::Query(t) => serde_json::json!({
                    "kind": "query",
                    "active": ix == self.active,
                    "language": t.language,
                    "text": t.editor.read(cx).value().to_string(),
                    "result": t.results.read(cx).summary(),
                }),
                WorkspaceTab::Dashboard(t) => serde_json::json!({
                    "kind": "dashboard",
                    "active": ix == self.active,
                    "path": t.path.display().to_string(),
                    "dashboard": t.host.read(cx).summary(),
                }),
            })
            .collect();
        serde_json::json!({ "tabs": tabs })
    }

    /// Put `text` in the active tab when it is empty and of the same
    /// language, otherwise in a new tab. Used by the sidebar and history.
    pub fn insert_query(&mut self, language: Language, text: &str, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let reuse = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)
            .filter(|t| t.language == language && t.editor.read(cx).value().trim().is_empty())
            .map(|t| t.editor.clone());
        match reuse {
            Some(editor) => editor.update(cx, |e, cx| e.set_value(text.to_string(), window, cx)),
            None => self.add_tab(language, Some(text), window, cx),
        }
        self.remember_tabs(cx);
        if run {
            self.run_active(window, cx);
        }
        cx.notify();
    }

    pub fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        // The last query tab stays: a dashboard alone has nowhere to send a query.
        let queries = self.tabs.iter().filter(|t| t.as_query().is_some()).count();
        let closing_query = self.tabs.iter().any(|t| t.id() == id && t.as_query().is_some());
        if self.tabs.len() <= 1 || (closing_query && queries <= 1) {
            return;
        }
        if let Some(ix) = self.tabs.iter().position(|t| t.id() == id) {
            self.tabs.remove(ix);
            if self.active >= self.tabs.len() || self.active > ix {
                self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
            }
            self.remember_tabs(cx);
            self.focus_active_editor(window, cx);
            cx.notify();
        }
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(self.active).map(WorkspaceTab::id) {
            self.close_tab(id, window, cx);
        }
    }

    pub fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active = index.min(self.tabs.len() - 1);
        self.focus_active_editor(window, cx);
        cx.notify();
    }

    pub fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n == 0 {
            return;
        }
        let next = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        self.activate_tab(next, window, cx);
    }

    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut App) {
        if let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) {
            tab.editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
    }

    pub fn run_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) else {
            return;
        };
        let text = selected_or_all(&tab.editor, cx);
        if text.trim().is_empty() {
            return;
        }
        let language = tab.language;
        let results = tab.results.clone();
        self.running = true;
        results.update(cx, |r, cx| r.set_running(cx));
        cx.notify();
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let run_text = text.clone();
            let (result, graph) = smol::unblock(move || {
                let result = crate::db::run(language, &run_text);
                let graph = crate::db::with_graph(|g| Ok(g.path().to_path_buf())).ok();
                if let Some(graph) = &graph {
                    let _ = settings::push_history(HistoryEntry {
                        language,
                        text: run_text.trim().to_string(),
                        graph: graph.clone(),
                        at: util::unix_now(),
                        ok: result.is_ok(),
                    });
                }
                (result, graph)
            })
            .await;
            let _ = graph;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                let writes = changes_schema(&text);
                match result {
                    Ok(result) => {
                        let stats = QueryStats {
                            elapsed_ms: result.elapsed_ms,
                            rows: result.row_count,
                            nodes: result.graph.as_ref().map_or(0, |g| g.nodes.len()),
                            edges: result.graph.as_ref().map_or(0, |g| g.edges.len()),
                        };
                        let is_plan = result.columns == ["plan"] && text.trim_start().to_uppercase().starts_with("EXPLAIN");
                        results.update(cx, |r, cx| {
                            if is_plan {
                                let plan = result.rows.iter().filter_map(|r| r[0].as_str()).collect::<Vec<_>>().join("\n");
                                r.set_plan(plan, cx);
                            } else {
                                r.set_result(result, window, cx);
                            }
                        });
                        state.update(cx, |s, cx| s.set_last_query(Some(stats), cx));
                    }
                    Err(error) => {
                        results.update(cx, |r, cx| r.set_error(error, cx));
                        state.update(cx, |s, cx| s.set_last_query(None, cx));
                    }
                }
                state.update(cx, |s, cx| s.reload_lists(cx));
                if writes {
                    crate::ui::reload_schema(state.clone(), window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn explain_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) else {
            return;
        };
        let text = selected_or_all(&tab.editor, cx);
        if text.trim().is_empty() {
            return;
        }
        let language = tab.language;
        let results = tab.results.clone();
        self.running = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let plan = smol::unblock(move || crate::db::explain(language, &text)).await;
            this.update(cx, |this, cx| {
                this.running = false;
                results.update(cx, |r, cx| match plan {
                    Ok(plan) => r.set_plan(plan, cx),
                    Err(e) => r.set_error(e, cx),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let closable = self.tabs.len() > 1;
        TabBar::new("query-tabs")
            .small()
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate_tab(*ix, window, cx)))
            .children(self.tabs.iter().map(|tab| {
                let id = tab.id();
                let (badge, title, marked) = match tab {
                    WorkspaceTab::Query(tab) => {
                        let first_line = tab.editor.read(cx).value().lines().next().unwrap_or("").trim().to_string();
                        let title = if first_line.is_empty() {
                            tr("workspace.untitled").to_string()
                        } else {
                            first_line.chars().take(28).collect()
                        };
                        let badge = match tab.language {
                            Language::Cypher => "CYPHER",
                            Language::Sql => "SQL",
                        };
                        (badge, title, false)
                    }
                    WorkspaceTab::Dashboard(tab) => {
                        let host = tab.host.read(cx);
                        ("DASH", tab.title.to_string(), host.is_dirty(cx) || host.has_presence())
                    }
                };
                Tab::new()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().primary)
                                    .child(badge),
                            )
                            .child(title)
                            .when(marked, |this| this.child(div().size_1p5().rounded_full().bg(cx.theme().warning))),
                    )
                    .when(closable, |this| {
                        this.suffix(
                            Button::new(("close-tab", id as usize))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(tr("menu.close_tab"))
                                .on_click(cx.listener(move |this, _, window, cx| this.close_tab(id, window, cx))),
                        )
                    })
            }))
            .suffix({
                let view = cx.entity().downgrade();
                Button::new("add-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(tr("workspace.add_tab"))
                    .dropdown_menu(move |menu, _, _| {
                        let cypher = view.clone();
                        let sql = view.clone();
                        let dashboard = view.clone();
                        menu.item(PopupMenuItem::new(tr("workspace.new_cypher")).on_click(move |_, window, cx| {
                            if let Some(view) = cypher.upgrade() {
                                view.update(cx, |this, cx| this.add_tab(Language::Cypher, None, window, cx));
                            }
                        }))
                        .item(PopupMenuItem::new(tr("workspace.new_sql")).on_click(move |_, window, cx| {
                            if let Some(view) = sql.upgrade() {
                                view.update(cx, |this, cx| this.add_tab(Language::Sql, None, window, cx));
                            }
                        }))
                        .separator()
                        .item(PopupMenuItem::new(tr("workspace.open_dashboard")).on_click(move |_, window, cx| {
                            if let Some(view) = dashboard.upgrade() {
                                view.update(cx, |this, cx| this.pick_dashboard_file(window, cx));
                            }
                        }))
                    })
            })
    }

    /// After DuckLocal's: the spec's path, save, source and reload.
    fn render_dashboard_toolbar(&mut self, path: &std::path::Path, host: Entity<Dashboard>, cx: &mut Context<Self>) -> impl IntoElement {
        let (running, showing_source, dirty) = {
            let dashboard = host.read(cx);
            (dashboard.is_running(), dashboard.is_showing_source(), dashboard.is_dirty(cx))
        };
        let (source_host, reload_host) = (host.clone(), host.clone());
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Icon::new(AssetIcon::ChartNoAxesColumn).small().text_color(cx.theme().muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(path.display().to_string()),
            )
            .child(
                Button::new("dashboard-save")
                    .outline()
                    .small()
                    .icon(AssetIcon::Save)
                    .label(tr("dashboard.save"))
                    .tooltip_with_action(tr("dashboard.save.tooltip"), &SaveSpec, Some(WORKSPACE_KEY_CONTEXT))
                    .disabled(!dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.save_active_dashboard(window, cx))),
            )
            .child(
                Button::new("dashboard-source")
                    .outline()
                    .small()
                    .icon(if showing_source { AssetIcon::ChartNoAxesColumn } else { AssetIcon::Code })
                    .label(if showing_source { tr("dashboard.source.back") } else { tr("dashboard.source.show") })
                    .tooltip(tr("dashboard.source.tooltip"))
                    .on_click(move |_, _, cx| source_host.update(cx, |dashboard, cx| dashboard.toggle_source(cx))),
            )
            .child(
                Button::new("dashboard-reload")
                    .outline()
                    .small()
                    .icon(IconName::Redo)
                    .label(tr("dashboard.reload"))
                    .loading(running)
                    .tooltip(tr("dashboard.reload.tooltip"))
                    .on_click(move |_, _, cx| reload_host.update(cx, |dashboard, cx| dashboard.reload(cx))),
            )
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let run_keystroke = Keystroke::parse(RUN_QUERY_KEYSTROKE).ok();
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("run-query")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label(tr("workspace.run"))
                    .loading(self.running)
                    .when_some(run_keystroke, |this, keystroke| this.child(Kbd::new(keystroke)))
                    .on_click(cx.listener(|this, _, window, cx| this.run_active(window, cx))),
            )
            .child(
                Button::new("explain-query")
                    .ghost()
                    .small()
                    .icon(gpui_kit::assets::IconName::ListTree)
                    .label(tr("workspace.explain"))
                    .tooltip(tr("workspace.explain.tooltip"))
                    .on_click(cx.listener(|this, _, window, cx| this.explain_active(window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("workspace.selection_hint")),
            )
    }

    fn render_welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.state.read(cx).recent.clone();
        let open_state = self.state.clone();
        let new_state = self.state.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(img("brand/logo.png").size(px(96.)).rounded_xl())
            .child(div().text_2xl().font_weight(FontWeight::BOLD).child("KhonsuGraph"))
            .child(div().text_color(cx.theme().muted_foreground).child(tr("app.tagline")))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("welcome-open")
                            .primary()
                            .icon(IconName::FolderOpen)
                            .label(tr("title_bar.open_graph"))
                            .on_click(move |_, window, cx| crate::ui::pick_graph(open_state.clone(), false, window, cx)),
                    )
                    .child(
                        Button::new("welcome-new")
                            .outline()
                            .icon(IconName::Plus)
                            .label(tr("title_bar.new_graph"))
                            .on_click(move |_, window, cx| crate::ui::pick_graph(new_state.clone(), true, window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .w(crate::ui::scale::design(520.))
                    .mt_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(tr("welcome.recent")),
                    )
                    .when(recent.is_empty(), |this| {
                        this.child(div().px_3().py_2().text_sm().text_color(cx.theme().muted_foreground).child(tr("welcome.no_recent")))
                    })
                    .children(recent.into_iter().enumerate().map(|(ix, path)| {
                        let open = self.state.clone();
                        let read_only = self.state.clone();
                        let forget = self.state.clone();
                        let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
                        h_flex()
                            .id(("recent", ix))
                            .px_3()
                            .py_1p5()
                            .gap_2()
                            .items_center()
                            .hover(|s| s.bg(cx.theme().secondary))
                            .child(Icon::new(IconName::Folder).small().text_color(cx.theme().muted_foreground))
                            .child(
                                div()
                                    .id(("recent-path", ix))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .cursor_pointer()
                                    .child(path.display().to_string())
                                    .on_click(move |_, window, cx| {
                                        crate::ui::open_graph_at(open.clone(), p1.clone(), false, false, window, cx)
                                    }),
                            )
                            .child(
                                Button::new(("recent-ro", ix))
                                    .ghost()
                                    .xsmall()
                                    .label(tr("welcome.read_only"))
                                    .on_click(move |_, window, cx| {
                                        crate::ui::open_graph_at(read_only.clone(), p2.clone(), false, true, window, cx)
                                    }),
                            )
                            .child(
                                Button::new(("recent-forget", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(tr("welcome.forget"))
                                    .on_click(move |_, _, cx| {
                                        forget.update(cx, |s, cx| s.forget_recent(&p3, cx));
                                    }),
                            )
                    })),
            )
            .child(
                div()
                    .w(crate::ui::scale::design(520.))
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("welcome.privacy")),
            )
    }
}

fn selected_or_all(editor: &Entity<EditorState>, cx: &App) -> String {
    let editor = editor.read(cx);
    let selected = editor.selected_text().to_string();
    if selected.trim().is_empty() {
        editor.value().to_string()
    } else {
        selected
    }
}

fn changes_schema(text: &str) -> bool {
    let upper = text.to_uppercase();
    upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| WRITE_WORDS.contains(&word))
}

/// See DuckLocal: ⌘↵ in the editor first inserts a newline; take it back.
fn remove_secondary_enter_newline(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let range = editor.update(cx, |editor, _| {
        let cursor = editor.cursor();
        let text = editor.text();
        let mut start = cursor;
        let mut chars = text.chars_at(cursor);
        let mut prev = chars.prev();
        while matches!(prev, Some(' ' | '\t')) {
            start -= 1;
            prev = chars.prev();
        }
        if prev != Some('\n') {
            return None;
        }
        Some(start - 1..cursor)
    });
    if let Some(range) = range {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace("", window, cx);
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_graph = self.state.read(cx).graph.is_some();
        let dashboard = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_dashboard)
            .map(|t| (t.path.clone(), t.host.clone()));
        let query = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)
            .map(|t| (t.editor.clone(), t.results.clone()));
        v_flex()
            .id("workspace")
            .size_full()
            .min_w_0()
            .key_context(WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &RunQuery, window, cx| this.run_active(window, cx)))
            .on_action(cx.listener(|this, _: &ExplainQuery, window, cx| this.explain_active(window, cx)))
            .on_action(cx.listener(|this, _: &SaveSpec, window, cx| this.save_active_dashboard(window, cx)))
            .when(!has_graph, |this| this.child(self.render_welcome(cx)))
            .when(has_graph, |this| {
                let this = this.child(self.render_tab_bar(cx));
                match (dashboard, query) {
                    (Some((path, host)), _) => this
                        .child(self.render_dashboard_toolbar(&path, host.clone(), cx))
                        .child(div().flex_1().min_h_0().child(host)),
                    (None, Some((editor, results))) => this.child(self.render_toolbar(cx)).child(
                        div().flex_1().min_h_0().child(
                            v_resizable("editor-results")
                                .child(resizable_panel().child(
                                    div().size_full().child(Editor::new(&editor).h(relative(1.)).bordered(false)),
                                ))
                                .child(
                                    resizable_panel()
                                        .size(px(RESULTS_PANEL_DEFAULT))
                                        .size_range(px(160.)..px(2000.))
                                        .child(results),
                                ),
                        ),
                    ),
                    (None, None) => this,
                }
            })
    }
}

/// Two paths name one file, however they were written.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_are_recognised_as_whole_words() {
        assert!(super::changes_schema("MATCH (n) SET n.x = 1"));
        assert!(super::changes_schema("create (:A)"));
        assert!(!super::changes_schema("MATCH (n:Settings) RETURN n.created_at"));
    }
}

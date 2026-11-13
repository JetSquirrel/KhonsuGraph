//! Center workspace, after DuckLocal's: query tabs (Cypher or SQL), the
//! toolbar, the editor, and the tab's own results below a resizable split.
//! The home page stands in for all that when no graph is open, when a graph
//! opens with nothing to come back to, or when asked for. The tabs wait behind it.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, TabSize};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::investigation::{Investigation, Reopened, SavedQuery};
use khonsu_core::settings::{self, HistoryEntry};
use khonsu_core::{util, Language};
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable, WindowExt};

use crate::i18n::{tr, trf};
use crate::spec::view::{Dashboard, DashboardEvent};
use crate::state::{AppState, GraphChanged, GraphWritten, QueryStats};
use crate::ui::results::{ResultsPanel, View};
use crate::ui::{ExplainQuery, RunQuery, SaveSpec, WORKSPACE_KEY_CONTEXT};

mod dashboards;
mod home;
mod run;
mod tabs;
mod toolbar;

use run::*;

const RESULTS_PANEL_DEFAULT: f32 = 540.;

pub struct QueryTab {
    pub id: u64,
    /// "Query N" in the tab bar, as in DuckLocal: one past the highest open.
    pub number: usize,
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

pub struct Workspace {
    state: Entity<AppState>,
    /// The graph the tabs belong to, and are remembered under: during a
    /// switch the state already names the next one.
    tabs_graph: Option<std::path::PathBuf>,
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

/// Whether a query tab holds anything beyond what a new tab starts with.
fn is_work(language: Language, text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text != default_text(language).trim()
}

impl Workspace {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe_in(&state, _window, |this, _, _: &GraphChanged, window, cx| {
                this.restore_tabs(window, cx);
            }),
            // A sync changed the graph: dashboards show it as it now is.
            cx.subscribe(&state, |this, _, _: &GraphWritten, cx| {
                for tab in this.tabs.iter().filter_map(WorkspaceTab::as_dashboard) {
                    tab.host.update(cx, |dashboard, cx| dashboard.graph_written(cx));
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        Self {
            state,
            tabs_graph: None,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            running: false,
            _subscriptions: subscriptions,
        }
    }

}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let home = self.state.read(cx).showing_home();
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
            .when(home, |this| this.child(self.render_home(cx)))
            .when(!home, |this| {
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
    use super::home::{browse_query, graph_sample_query};
    use super::{default_text, is_work, Language};

    #[test]
    fn a_new_tab_is_not_work_until_someone_changes_it() {
        assert!(!is_work(Language::Cypher, ""));
        assert!(!is_work(Language::Cypher, "  \n"));
        assert!(!is_work(Language::Cypher, default_text(Language::Cypher)));
        assert!(!is_work(Language::Sql, default_text(Language::Sql)));
        assert!(is_work(Language::Cypher, "MATCH (n) RETURN n"));
        // The other language's starting text was written by someone.
        assert!(is_work(Language::Sql, default_text(Language::Cypher)));
    }

    #[test]
    fn the_samples_are_bounded_and_a_nodes_only_graph_still_shows_nodes() {
        for query in [browse_query(), graph_sample_query(0), graph_sample_query(5)] {
            assert!(query.contains("LIMIT"), "{query}");
        }
        assert!(!graph_sample_query(0).contains("-[r]->"));
        assert!(graph_sample_query(5).contains("-[r]->"));
    }

    #[test]
    fn writes_are_recognised_as_whole_words() {
        assert!(super::changes_schema("MATCH (n) SET n.x = 1"));
        assert!(super::changes_schema("create (:A)"));
        assert!(!super::changes_schema("MATCH (n:Settings) RETURN n.created_at"));
    }
}

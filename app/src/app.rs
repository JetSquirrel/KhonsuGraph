//! Root view, after DuckLocal's: title bar, sidebar beside the workspace,
//! status bar. Owns the shared `AppState`.

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{v_flex, ActiveTheme, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::settings::Settings;
use khonsu_core::Language;
use khonsu_dash::remote::Message;
use serde_json::json;
use std::path::PathBuf;

use crate::i18n::trf;
use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;
use crate::ui::{
    ActivateTab, AddSource, CloseTab, NewGraph, NewQuery, NewSqlQuery, NextTab, OpenDashboard, OpenGraph, PreviousTab,
    ToggleSidebar,
};

pub struct KhonsuApp {
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
    /// Where `khonsu` commands reach this window; removed with it.
    _remote: Option<khonsu_dash::remote::ServeGuard>,
}

impl KhonsuApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(AppState::new);
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), workspace.clone(), window, cx));
        let title_bar = cx.new(|cx| TitleBarView::new(state.clone(), cx));
        let status_bar = cx.new(|cx| StatusBarView::new(state.clone(), cx));
        cx.subscribe(&state, |_, _, _: &state::SidebarToggled, cx| cx.notify()).detach();
        cx.subscribe(&state, |_, _, _: &state::GraphChanged, cx| cx.notify()).detach();

        // Back to the graph that was open when the app last closed.
        if let Some(path) = Settings::load().last_opened.filter(|p| p.exists()) {
            crate::ui::open_graph_at(state.clone(), path, false, false, window, cx);
        }

        // `khonsu open`, `presence` and `--state` reach the views from here:
        // drained on a slow poll, as DuckLocal's window does.
        cx.spawn_in(window, async move |this, cx| loop {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let pending = crate::remote::take_pending();
            let changes = crate::remote::take_changes();
            if pending.is_empty() && changes.is_empty() {
                continue;
            }
            if this
                .update_in(cx, |this, window, cx| {
                    for (message, answer) in pending {
                        let reply = this.answer(message, window, cx);
                        let _ = answer.send(reply);
                    }
                    // A command wrote to the graph: what the sidebar shows of
                    // it is stale.
                    if !changes.is_empty() {
                        this.state.update(cx, |state, cx| state.reload_lists(cx));
                        crate::ui::reload_schema(this.state.clone(), window, cx);
                        window.push_notification(trf("notify.remote.changed", &[&changes.join("\n")]), cx);
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        Self {
            state,
            title_bar,
            sidebar,
            workspace,
            status_bar,
            _remote: crate::remote::serve(),
        }
    }

    /// A message from a `khonsu` command that the views answer.
    fn answer(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) -> serde_json::Value {
        match message {
            Message::Presence(update) => {
                self.workspace.update(cx, |ws, cx| ws.apply_presence(&update, cx));
                json!({})
            }
            Message::State(request) => {
                let state = self.state.read(cx);
                let graph = state.graph.clone();
                let only = request.path.as_ref().map(PathBuf::from);
                let mut snapshot = self.workspace.read(cx).snapshot(only.as_deref(), request.active, cx);
                snapshot["graph"] = json!(graph.as_ref().map(|g| g.path.display().to_string()));
                snapshot["read_only"] = json!(graph.as_ref().is_some_and(|g| g.read_only));
                if only.is_some() && snapshot["tabs"].as_array().is_some_and(|t| t.is_empty()) {
                    return json!({"error": {"kind": "not_open", "message": "no tab shows that file"}});
                }
                snapshot
            }
            Message::Open(request) => {
                window.activate_window();
                // The graph first: dashboards and the query open on it once
                // it is open, not among the tabs of the graph it replaces.
                let mut graph = None;
                let mut dashboards = Vec::new();
                let mut opened = Vec::new();
                for path in request.paths.iter().map(PathBuf::from) {
                    if khonsu_core::engine::is_graph_dir(&path) {
                        opened.push(json!({"graph": path.display().to_string()}));
                        graph = Some(path);
                    } else if crate::spec::tabs::is_spec(&path) {
                        opened.push(json!({"dashboard": path.display().to_string()}));
                        dashboards.push(path);
                    } else {
                        return json!({"error": {"kind": "not_openable", "message": format!("{} is neither a graph nor a .dash file", path.display())}});
                    }
                }
                if request.text.is_some() {
                    opened.push(json!({"query": true, "run": request.run}));
                }
                let workspace = self.workspace.clone();
                let (text, language, run) = (request.text, request.language.unwrap_or(Language::Cypher), request.run);
                let show: crate::ui::AfterOpen = Box::new(move |window, cx| {
                    workspace.update(cx, |ws, cx| {
                        for path in dashboards {
                            ws.open_dashboard(path, window, cx);
                        }
                        if let Some(text) = text {
                            ws.add_tab(language, Some(&text), window, cx);
                            if run {
                                ws.run_active(window, cx);
                            }
                        }
                    });
                });
                // The graph already open is not reopened: that would reset
                // its tabs for nothing.
                let open_now = self.state.read(cx).graph.as_ref().map(|g| g.path.clone());
                match graph {
                    Some(path) if open_now.as_deref() != Some(khonsu_core::normalize_path(&path).as_path()) => {
                        crate::ui::open_graph_then(self.state.clone(), path, false, false, Some(show), window, cx);
                    }
                    _ => show(window, cx),
                }
                json!({"delivered": true, "opened": opened})
            }
            Message::Query(_) | Message::Check(_) | Message::Schema(_) | Message::Explain(_) | Message::Sync(_) => {
                json!({"error": {"kind": "internal", "message": "answered by the listener"}})
            }
        }
    }
}

impl Render for KhonsuApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.state.read(cx).is_sidebar_collapsed();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.state.update(cx, |state, cx| state.toggle_sidebar(cx));
            }))
            .on_action(cx.listener(|this, _: &NewQuery, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.add_tab(Language::Cypher, None, window, cx));
            }))
            .on_action(cx.listener(|this, _: &NewSqlQuery, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.add_tab(Language::Sql, None, window, cx));
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.close_active_tab(window, cx));
            }))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                let index = action.index;
                this.workspace.update(cx, |ws, cx| ws.activate_tab(index, window, cx));
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.cycle_tab(true, window, cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.cycle_tab(false, window, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenGraph, window, cx| {
                crate::ui::pick_graph(this.state.clone(), false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenDashboard, window, cx| {
                this.workspace.update(cx, |ws, cx| ws.pick_dashboard_file(window, cx));
            }))
            .on_action(cx.listener(|this, _: &NewGraph, window, cx| {
                crate::ui::pick_graph(this.state.clone(), true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddSource, window, cx| {
                let state = this.state.read(cx);
                if state.graph.is_some() && !state.read_only() {
                    crate::ui::sources::open_add(this.state.clone(), window, cx);
                }
            }))
            .child(self.title_bar.clone())
            .child(div().flex_1().min_h_0().map(|this| {
                if collapsed {
                    this.child(self.workspace.clone())
                } else {
                    this.child(
                        h_resizable("main-split")
                            .child(
                                resizable_panel()
                                    .size(crate::ui::scale::design(280.))
                                    .size_range(crate::ui::scale::design(220.)..crate::ui::scale::design(460.))
                                    .child(self.sidebar.clone()),
                            )
                            .child(resizable_panel().child(self.workspace.clone())),
                    )
                }
            }))
            .child(self.status_bar.clone())
    }
}

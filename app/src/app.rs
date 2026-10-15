//! Root view, after DuckLocal's: title bar, sidebar beside the workspace,
//! status bar. Owns the shared `AppState`.

use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::settings::Settings;
use khonsu_core::Language;

use crate::state::{self, AppState};
use crate::ui::sidebar::Sidebar;
use crate::ui::status_bar::StatusBarView;
use crate::ui::title_bar::TitleBarView;
use crate::ui::workspace::Workspace;
use crate::ui::{
    ActivateTab, AddSource, CloseTab, NewGraph, NewQuery, NewSqlQuery, NextTab, OpenGraph, PreviousTab, ToggleSidebar,
};

pub struct KhonsuApp {
    state: Entity<AppState>,
    title_bar: Entity<TitleBarView>,
    sidebar: Entity<Sidebar>,
    workspace: Entity<Workspace>,
    status_bar: Entity<StatusBarView>,
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

        Self {
            state,
            title_bar,
            sidebar,
            workspace,
            status_bar,
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

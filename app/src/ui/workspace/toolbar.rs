//! The tab bar and the toolbars above a query or a dashboard.

use super::*;

impl Workspace {
    pub(super) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let queries = self.tabs.iter().filter(|t| t.as_query().is_some()).count();
        TabBar::new("query-tabs")
            .small()
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate_tab(*ix, window, cx)))
            .children(self.tabs.iter().map(|tab| {
                let id = tab.id();
                let closable = self.tabs.len() > 1 && (tab.as_query().is_none() || queries > 1);
                let (glyph, title, marked) = match tab {
                    WorkspaceTab::Query(tab) => {
                        let title = trf("workspace.tab.default_title", &[&tab.number.to_string()]);
                        // A graph query wears a graph's glyph, a table query
                        // DuckLocal's terminal: the two read apart at a glance.
                        let glyph = match tab.language {
                            Language::Cypher => AssetIcon::Waypoints,
                            Language::Sql => AssetIcon::SquareTerminal,
                        };
                        (glyph, title, false)
                    }
                    WorkspaceTab::Dashboard(tab) => {
                        let host = tab.host.read(cx);
                        (AssetIcon::ChartNoAxesColumn, tab.title.to_string(), host.is_dirty(cx) || host.has_presence())
                    }
                };
                Tab::new()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(Icon::new(glyph).small().text_color(cx.theme().muted_foreground))
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
    pub(super) fn render_dashboard_toolbar(&mut self, path: &std::path::Path, host: Entity<Dashboard>, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .tooltip_with_action(tr("dashboard.save.tooltip"), &SaveSpec, Some(WORKSPACE_KEY_CONTEXT))
                    .disabled(!dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.save_active_dashboard(window, cx))),
            )
            .child(
                Button::new("dashboard-source")
                    .outline()
                    .small()
                    .icon(if showing_source { AssetIcon::ChartNoAxesColumn } else { AssetIcon::Code })
                    // The icon is the button's only word: the tooltip says
                    // which way it goes.
                    .tooltip(if showing_source { tr("dashboard.source.back") } else { tr("dashboard.source.tooltip") })
                    .on_click(move |_, _, cx| source_host.update(cx, |dashboard, cx| dashboard.toggle_source(cx))),
            )
            .child(
                Button::new("dashboard-reload")
                    .outline()
                    .small()
                    .icon(IconName::RotateCw)
                    .loading(running)
                    .tooltip(tr("dashboard.reload.tooltip"))
                    .on_click(move |_, _, cx| reload_host.update(cx, |dashboard, cx| dashboard.reload(cx))),
            )
    }

    pub(super) fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)
            .is_none_or(|tab| tab.editor.read(cx).value().trim().is_empty());
        let opening = self.state.read(cx).is_opening();
        let disabled = self.running || opening || empty;
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
                    // The shortcut lives in the tooltip, as in DuckLocal.
                    .tooltip_with_action(tr("workspace.run.tooltip"), &RunQuery, Some(WORKSPACE_KEY_CONTEXT))
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.run_active(window, cx))),
            )
            .child(
                Button::new("explain-query")
                    .ghost()
                    .small()
                    .icon(gpui_kit::assets::IconName::ListTree)
                    .tooltip(tr("workspace.explain.tooltip"))
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.explain_active(window, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(if opening {
                        "status_bar.opening"
                    } else if self.running {
                        "results.running"
                    } else if empty {
                        "workspace.empty_hint"
                    } else {
                        "workspace.selection_hint"
                    })),
            )
    }
}

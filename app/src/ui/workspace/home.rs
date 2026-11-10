//! The home page: recent graphs, and an overview of the open one.

use super::*;

/// Node types named on the home page; the sidebar lists them all.
const OVERVIEW_LABELS: usize = 8;

pub(super) fn browse_query() -> &'static str {
    "MATCH (n)\nRETURN n\nLIMIT 100"
}

/// A bounded sample for the canvas. A pattern with a relationship would find
/// nothing in a graph that has only nodes, and read as an empty graph.
pub(super) fn graph_sample_query(relationships: u64) -> &'static str {
    if relationships == 0 {
        "MATCH (n)\nRETURN n\nLIMIT 200"
    } else {
        "MATCH (n)-[r]->(m)\nRETURN n, r, m\nLIMIT 200"
    }
}

impl Workspace {
    /// Whether any tab holds something of the person's.
    pub(super) fn has_work(&self, cx: &App) -> bool {
        self.tabs.iter().any(|tab| match tab {
            WorkspaceTab::Query(tab) => is_work(tab.language, &tab.editor.read(cx).value()),
            WorkspaceTab::Dashboard(_) => true,
        })
    }

    pub(super) fn leave_home(&self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.set_home(false, cx));
    }

    /// Back to the tabs just as they were.
    pub(super) fn continue_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_home(cx);
        self.focus_active_editor(window, cx);
    }

    /// Run a query the app wrote, in the tab if it holds nothing of the
    /// person's, and show the result in `view`.
    pub(super) fn explore(&mut self, text: &str, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_home(cx);
        let reuse = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)
            .filter(|tab| tab.language == Language::Cypher && !is_work(tab.language, &tab.editor.read(cx).value()))
            .map(|tab| tab.editor.clone());
        match reuse {
            Some(editor) => editor.update(cx, |editor, cx| editor.set_value(text.to_string(), window, cx)),
            None => self.add_tab(Language::Cypher, Some(text), window, cx),
        }
        if let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) {
            tab.results.clone().update(cx, |results, cx| results.prefer_view(view, cx));
        }
        self.remember_tabs(cx);
        self.run_active(window, cx);
    }

    /// What's in the open graph and what can be done with it.
    pub(super) fn render_overview(&mut self, info: &crate::db::GraphInfo, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (schema, schema_error) = {
            let state = self.state.read(cx);
            (state.schema.clone(), state.schema_error.clone())
        };
        let has_work = self.has_work(cx);
        let read_only = info.read_only;
        let card = v_flex()
            .w(crate::ui::scale::design(520.))
            .p_4()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(info.name()))
                            .when(read_only, |this| {
                                this.child(div().text_xs().px_1p5().rounded_md().border_1().border_color(cx.theme().border).child(tr("title_bar.read_only")))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(muted)
                            .child(info.path.display().to_string()),
                    ),
            );
        let Some(schema) = schema else {
            return card
                .child(match schema_error {
                    Some(error) => div().text_sm().whitespace_normal().text_color(cx.theme().danger).child(error).into_any_element(),
                    None => h_flex().gap_2().items_center().text_sm().text_color(muted).child(gpui_kit::component::spinner::Spinner::new().xsmall()).child(tr("sidebar.loading")).into_any_element(),
                })
                .into_any_element();
        };
        let has_nodes = schema.node_count > 0;
        let nodes_only = has_nodes && schema.relationship_count == 0;
        let mut labels = schema.labels.clone();
        labels.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        let more_labels = labels.len().saturating_sub(OVERVIEW_LABELS);
        labels.truncate(OVERVIEW_LABELS);
        card.child(div().text_sm().child(trf("home.counts", &[&schema.node_count.to_string(), &schema.relationship_count.to_string()])))
            .when(!labels.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(tr("home.labels")))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_1()
                                .items_center()
                                .children(labels.into_iter().enumerate().map(|(ix, label)| {
                                    let sample = crate::ui::sidebar::sample_for_label(&label.name);
                                    Button::new(("home-label", ix))
                                        .outline()
                                        .xsmall()
                                        .label(format!("{} · {}", label.name, label.count))
                                        .tooltip(tr("sidebar.sample_query"))
                                        .on_click(cx.listener(move |this, _, window, cx| this.explore(&sample, View::Table, window, cx)))
                                }))
                                .when(more_labels > 0, |this| {
                                    this.child(div().text_xs().text_color(muted).child(trf("home.more_labels", &[&more_labels.to_string()])))
                                }),
                        ),
                )
            })
            .when(nodes_only, |this| this.child(div().text_sm().text_color(muted).child(tr("home.nodes_only"))))
            .when(!has_nodes, |this| {
                this.child(div().text_sm().whitespace_normal().text_color(muted).child(tr(if read_only { "home.empty_read_only" } else { "home.empty" })))
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .when(has_work, |this| {
                        this.child(
                            Button::new("home-continue")
                                .primary()
                                .small()
                                .label(tr("home.continue"))
                                .on_click(cx.listener(|this, _, window, cx| this.continue_work(window, cx))),
                        )
                    })
                    .when(has_nodes, |this| {
                        this.child(
                            Button::new("home-browse")
                                .map(|button| if has_work { button.outline() } else { button.primary() })
                                .small()
                                .label(tr("home.browse_nodes"))
                                .on_click(cx.listener(|this, _, window, cx| this.explore(browse_query(), View::Table, window, cx))),
                        )
                        .child(Button::new("home-graph").outline().small().label(tr("home.view_graph")).on_click({
                            let sample = graph_sample_query(schema.relationship_count);
                            cx.listener(move |this, _, window, cx| this.explore(sample, View::Graph, window, cx))
                        }))
                    })
                    .when(!has_nodes && !read_only, |this| {
                        let state = self.state.clone();
                        this.child(
                            Button::new("home-add-data")
                                .primary()
                                .small()
                                .label(tr("home.add_data"))
                                .on_click(move |_, window, cx| crate::ui::sources::open_add(state.clone(), window, cx)),
                        )
                    })
                    .child(
                        Button::new("home-write")
                            .outline()
                            .small()
                            .label(tr("home.write_query"))
                            .on_click(cx.listener(|this, _, window, cx| this.continue_work(window, cx))),
                    ),
            )
            .when(has_nodes, |this| this.child(div().text_xs().whitespace_normal().text_color(muted).child(tr("home.sample"))))
            .into_any_element()
    }

    pub(super) fn render_home(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (opening, recent, graph) = {
            let state = self.state.read(cx);
            (state.is_opening(), state.recent.clone(), state.graph.clone())
        };
        let open_state = self.state.clone();
        let new_state = self.state.clone();
        let import_state = self.state.clone();
        let overview = graph.as_ref().map(|info| self.render_overview(info, cx));
        let has_graph = graph.is_some();
        div().id("home").size_full().overflow_y_scroll().child(v_flex()
            .w_full()
            .min_h_full()
            .items_center()
            .justify_center()
            .gap_4()
            .py_6()
            .when(!has_graph, |this| {
                this.child(img("brand/logo.png").size(px(96.)).rounded_xl())
                    .child(div().text_2xl().font_weight(FontWeight::BOLD).child("KhonsuGraph"))
            })
            .child(div().text_color(cx.theme().muted_foreground).child(tr("home.purpose")))
            .children(overview)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("welcome-open")
                            .map(|button| if has_graph { button.outline() } else { button.primary() })
                            .icon(IconName::FolderOpen)
                            .label(tr("title_bar.open_graph"))
                            .disabled(opening)
                            .on_click(move |_, window, cx| crate::ui::pick_graph(open_state.clone(), false, window, cx)),
                    )
                    .child(
                        Button::new("welcome-import")
                            .outline()
                            .label(tr("home.import_csv"))
                            .disabled(opening)
                            .on_click(move |_, window, cx| {
                                crate::ui::graph_dialog::show(import_state.clone(), crate::ui::graph_dialog::Purpose::ImportCsv, window, cx)
                            }),
                    )
                    .child(
                        Button::new("welcome-new")
                            .ghost()
                            .icon(IconName::Plus)
                            .label(tr("title_bar.new_graph"))
                            .disabled(opening)
                            .on_click(move |_, window, cx| crate::ui::pick_graph(new_state.clone(), true, window, cx)),
                    ),
            )
            .when(opening, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(gpui_kit::component::spinner::Spinner::new())
                        .child(tr("status_bar.opening")),
                )
            })
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
                                    .disabled(opening)
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
            ))
    }
}

//! Sidebar, after DuckLocal's: the graph's schema (labels, relationship
//! types, tables), its data sources, and the query history.

use std::collections::HashSet;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::schema::quote_ident;
use khonsu_core::Language;

use crate::i18n::{tr, trf};
use crate::state::{AppState, GraphChanged, HistoryChanged, SchemaChanged, SourcesChanged};
use crate::ui::palette;
use crate::ui::workspace::Workspace;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Schema,
    Sources,
    History,
}

pub struct Sidebar {
    state: Entity<AppState>,
    workspace: Entity<Workspace>,
    section: Section,
    expanded: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

/// Rows hover as one group, so a row's sample-query button can show only
/// while it is hovered: at rest the list reads as names and counts.
const ROW_GROUP: &str = "sidebar-row";

fn shown_on_hover(child: impl IntoElement) -> Div {
    div().opacity(0.).group_hover(ROW_GROUP, |s| s.opacity(1.)).child(child)
}

pub fn sample_for_label(label: &str) -> String {
    format!("MATCH (n:{})\nRETURN n\nLIMIT 100", quote_ident(label))
}

pub fn sample_for_type(rel_type: &str, patterns: &[(String, String)]) -> String {
    let (a, b) = match patterns.first() {
        Some((s, t)) => (format!("a:{}", quote_ident(s)), format!("b:{}", quote_ident(t))),
        None => ("a".into(), "b".into()),
    };
    format!("MATCH ({a})-[r:{}]->({b})\nRETURN a, r, b\nLIMIT 100", quote_ident(rel_type))
}

pub fn sample_for_table(table: &str) -> String {
    format!("SELECT *\nFROM {table}\nLIMIT 100")
}

fn time_ago(secs: u64) -> String {
    let d = khonsu_core::util::unix_now().saturating_sub(secs);
    match d {
        0..=59 => trf("time.seconds", &[&d.to_string()]),
        60..=3599 => trf("time.minutes", &[&(d / 60).to_string()]),
        3600..=86_399 => trf("time.hours", &[&(d / 3600).to_string()]),
        _ => khonsu_core::util::format_unix_time(secs),
    }
}

impl Sidebar {
    pub fn new(state: Entity<AppState>, workspace: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |_, _, _: &SchemaChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &SourcesChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &HistoryChanged, cx| cx.notify()),
            cx.subscribe(&state, |this, _, _: &GraphChanged, cx| {
                this.expanded.clear();
                cx.notify()
            }),
        ];
        Self {
            state,
            workspace,
            section: Section::Schema,
            expanded: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    fn insert(&self, language: Language, text: String, window: &mut Window, cx: &mut App) {
        self.workspace
            .update(cx, |ws, cx| ws.insert_query(language, &text, true, window, cx));
    }

    fn header(&self, title: &'static str, cx: &App) -> Div {
        h_flex()
            .px_3()
            .pt_3()
            .pb_1()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(tr(title))
    }

    fn row(&self, id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
        h_flex()
            .id(id)
            .px_3()
            .py_1()
            .gap_2()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .group(ROW_GROUP)
            .hover(|s| s.bg(cx.theme().secondary))
    }


    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    fn render_schema(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let Some(schema) = state.schema.clone() else {
            return v_flex()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(state.schema_error.clone().unwrap_or_else(|| tr("sidebar.loading").into()))
                .into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let mut list = v_flex().pb_3();

        list = list.child(self.header("sidebar.labels", cx));
        if schema.labels.is_empty() {
            list = list.child(div().px_3().text_sm().text_color(muted).child(tr("sidebar.schema.empty")));
        }
        for (ix, label) in schema.labels.iter().enumerate() {
            let key = format!("l:{}", label.name);
            let open = self.expanded.contains(&key);
            let name = label.name.clone();
            let run_name = label.name.clone();
            list = list.child(
                self.row(("label", ix), cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(key.clone(), cx)))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
                    .child(div().size_2p5().rounded_full().flex_none().bg(palette::label_color(&name, cx)))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(div().text_xs().text_color(muted).child(label.count.to_string()))
                    .child(shown_on_hover(
                        Button::new(("label-run", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Play)
                            .tooltip(tr("sidebar.sample_query"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.insert(Language::Cypher, sample_for_label(&run_name), window, cx);
                            })),
                    )),
            );
            if open {
                list = list.children(label.properties.iter().map(|p| {
                    div().pl(px(44.)).py_0p5().text_xs().font_family(mono.clone()).text_color(muted).child(p.clone())
                }));
            }
        }

        list = list.child(self.header("sidebar.relationship_types", cx));
        if schema.relationship_types.is_empty() {
            list = list.child(div().px_3().text_sm().text_color(muted).child("—"));
        }
        for (ix, rel) in schema.relationship_types.iter().enumerate() {
            let key = format!("r:{}", rel.name);
            let open = self.expanded.contains(&key);
            let sample = sample_for_type(&rel.name, &rel.patterns);
            list = list.child(
                self.row(("rel", ix), cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(key.clone(), cx)))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
                    .child(div().flex_1().min_w_0().truncate().child(format!("→ {}", rel.name)))
                    .child(div().text_xs().text_color(muted).child(rel.count.to_string()))
                    .child(shown_on_hover(
                        Button::new(("rel-run", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Play)
                            .tooltip(tr("sidebar.sample_query"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.insert(Language::Cypher, sample.clone(), window, cx);
                            })),
                    )),
            );
            if open {
                list = list
                    .children(rel.patterns.iter().map(|(s, t)| {
                        div().pl(px(44.)).py_0p5().text_xs().font_family(mono.clone()).text_color(muted).child(format!("({s})→({t})"))
                    }))
                    .children(rel.properties.iter().map(|p| {
                        div().pl(px(44.)).py_0p5().text_xs().font_family(mono.clone()).text_color(muted).child(p.clone())
                    }));
            }
        }

        if !schema.tables.is_empty() {
            list = list.child(self.header("sidebar.tables", cx));
            for (ix, table) in schema.tables.iter().enumerate() {
                let key = format!("t:{}", table.name);
                let open = self.expanded.contains(&key);
                let sample = sample_for_table(&table.name);
                list = list.child(
                    self.row(("table", ix), cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle(key.clone(), cx)))
                        .child(Icon::new(IconName::Inbox).xsmall().text_color(muted))
                        .child(div().flex_1().min_w_0().truncate().child(table.name.clone()))
                        .child(shown_on_hover(
                            Button::new(("table-run", ix))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Play)
                                .tooltip(tr("sidebar.sample_query"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.insert(Language::Sql, sample.clone(), window, cx);
                                })),
                        )),
                );
                if open {
                    list = list.children(table.columns.iter().map(|c| {
                        div().pl(px(44.)).py_0p5().text_xs().font_family(mono.clone()).text_color(muted).child(c.clone())
                    }));
                }
            }
        }
        list.into_any_element()
    }

    fn render_sources(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let sources = state.sources.clone();
        let syncing = state.syncing.clone();
        let read_only = state.read_only();
        let muted = cx.theme().muted_foreground;
        v_flex()
            .p_2()
            .gap_2()
            .when(sources.is_empty(), |this| this.child(div().px_1().text_sm().text_color(muted).child(tr("sources.empty"))))
            .children(sources.into_iter().enumerate().map(|(ix, source)| {
                let last = match &source.last_sync {
                    Some(last) if last.ok => trf("sources.last_ok", &[&time_ago(last.at), &last.nodes.to_string(), &last.edges.to_string()]),
                    Some(last) => trf("sources.last_failed", &[&time_ago(last.at), &last.message]),
                    None => tr("sources.never").to_string(),
                };
                let failed = source.last_sync.as_ref().is_some_and(|l| !l.ok);
                let busy = syncing.is_some();
                let (sync_state, edit_state, remove_state) = (self.state.clone(), self.state.clone(), self.state.clone());
                let (sync_source, edit_source, remove_source) = (source.clone(), source.clone(), source.clone());
                v_flex()
                    .p_2()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(source.name.clone()))
                            .child(div().text_xs().px_1p5().rounded_sm().bg(cx.theme().secondary).child(source.plugin.clone()))
                            .when(!read_only, |this| {
                                this.child(
                                    Button::new(("sync", ix))
                                        .xsmall()
                                        .ghost()
                                        .icon(IconName::RotateCw)
                                        .tooltip(tr("sources.sync"))
                                        .loading(syncing.as_deref() == Some(source.id.as_str()))
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            crate::ui::sources::sync(sync_state.clone(), sync_source.clone(), window, cx)
                                        }),
                                )
                                .child(
                                    Button::new(("edit", ix))
                                        .xsmall()
                                        .ghost()
                                        .icon(Icon::new(AssetIcon::Pencil))
                                        .tooltip(tr("sources.edit"))
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            crate::ui::sources::open_edit(edit_state.clone(), edit_source.clone(), window, cx)
                                        }),
                                )
                                .child(
                                    Button::new(("remove", ix))
                                        .xsmall()
                                        .ghost()
                                        .icon(Icon::new(AssetIcon::Trash))
                                        .tooltip(tr("sources.remove"))
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            crate::ui::sources::remove(remove_state.clone(), remove_source.clone(), window, cx)
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .when(failed, |this| this.text_color(cx.theme().danger))
                            .when(!failed, |this| this.text_color(muted))
                            .whitespace_normal()
                            .child(last),
                    )
            }))
            .into_any_element()
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let history = self.state.read(cx).history.clone();
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        v_flex()
            .py_1()
            .when(history.is_empty(), |this| this.child(div().p_3().text_sm().text_color(muted).child(tr("sidebar.history.empty"))))
            .when(!history.is_empty(), |this| {
                this.child(
                    h_flex().px_3().py_1().child(div().flex_1()).child(
                        Button::new("clear-history")
                            .ghost()
                            .xsmall()
                            .label(tr("sidebar.history.clear"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let _ = khonsu_core::settings::clear_history();
                                this.state.update(cx, |s, cx| s.reload_lists(cx));
                            })),
                    ),
                )
            })
            .children(history.into_iter().take(200).enumerate().map(|(ix, entry)| {
                let text = entry.text.clone();
                let language = entry.language;
                v_flex()
                    .id(("history", ix))
                    .px_3()
                    .py_1()
                    .gap_0p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().secondary))
                    .border_l_2()
                    .border_color(if entry.ok { gpui_kit::transparent_black() } else { cx.theme().danger })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let text = text.clone();
                        this.workspace
                            .update(cx, |ws, cx| ws.insert_query(language, &text, false, window, cx));
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_family(mono.clone())
                            .truncate()
                            .child(entry.text.lines().next().unwrap_or("").to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!(
                                "{} · {}",
                                match entry.language {
                                    Language::Cypher => "Cypher",
                                    Language::Sql => "SQL",
                                },
                                time_ago(entry.at)
                            )),
                    )
            }))
            .into_any_element()
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_graph = self.state.read(cx).graph.is_some();
        let selected = match self.section {
            Section::Schema => 0,
            Section::Sources => 1,
            Section::History => 2,
        };
        let body = if !has_graph {
            div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr("sidebar.no_graph"))
                .into_any_element()
        } else {
            match self.section {
                Section::Schema => self.render_schema(cx),
                Section::Sources => self.render_sources(cx),
                Section::History => self.render_history(cx),
            }
        };
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .px_2()
                    .pt_2()
                    .gap_1()
                    .child(
                        TabBar::new("sidebar-sections")
                            .segmented()
                            .with_size(crate::ui::scale::compact_control())
                            .selected_index(selected)
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.section = match ix {
                                    0 => Section::Schema,
                                    1 => Section::Sources,
                                    _ => Section::History,
                                };
                                cx.notify();
                            }))
                            .child(Tab::new().label(tr("sidebar.tab.schema")))
                            .child(Tab::new().label(tr("sidebar.tab.sources")))
                            .child(Tab::new().label(tr("sidebar.tab.history"))),
                    )
                    .child(div().flex_1())
                    .when(has_graph && self.section == Section::Schema, |this| {
                        this.child(
                            Button::new("refresh-schema")
                                .ghost()
                                .xsmall()
                                .icon(IconName::RotateCw)
                                .tooltip(tr("sidebar.refresh_schema"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    crate::ui::reload_schema(this.state.clone(), window, cx);
                                })),
                        )
                    })
                    // The Sources tab's one action sits where Schema keeps
                    // its refresh, rather than on a row of its own.
                    .when(has_graph && self.section == Section::Sources && !self.state.read(cx).read_only(), |this| {
                        let add_state = self.state.clone();
                        this.child(
                            Button::new("add-source")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .tooltip(tr("sources.add"))
                                .on_click(move |_, window, cx| crate::ui::sources::open_add(add_state.clone(), window, cx)),
                        )
                    }),
            )
            .child(div().id("sidebar-body").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}

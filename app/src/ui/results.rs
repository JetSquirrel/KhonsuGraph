//! One query tab's results: a grid (after DuckLocal's), the graph canvas when
//! the result holds nodes or relationships, and the plan. Errors are shown
//! as HawDB reported them, with the position it named.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::{QueryError, QueryResult};
use serde_json::Value as Json;

use crate::i18n::{tr, trf};
use crate::state::AppState;
use crate::ui::canvas::GraphCanvas;
use crate::ui::grid::{cell_paddings, fit_column_width};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Table,
    Graph,
    Plan,
}

enum Content {
    Empty,
    Running,
    Rows(Rc<QueryResult>),
    Plan(SharedString),
    Error(QueryError),
}

/// A cell as text: nodes as `(:Label name #id)`, relationships as
/// `[:TYPE #id] a→b`, everything else as JSON would print it.
pub fn cell_text(value: &Json) -> String {
    match value {
        Json::Null => "NULL".into(),
        Json::String(s) => s.clone(),
        Json::Object(map) => {
            let id = map.get("_id").and_then(Json::as_i64);
            if let (Some(id), Some(Json::Array(labels))) = (id, map.get("labels")) {
                let labels: Vec<&str> = labels.iter().filter_map(Json::as_str).collect();
                let caption = ["name", "title", "id", "key"]
                    .iter()
                    .find_map(|k| map.get(*k).filter(|v| !v.is_object() && !v.is_array() && !v.is_null()))
                    .map(|v| format!(" {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .unwrap_or_default();
                return format!("(:{}{caption} #{id})", labels.join(":"));
            }
            if let (Some(id), Some(Json::String(t))) = (id, map.get("type")) {
                if let (Some(s), Some(e)) = (map.get("source_id"), map.get("target_id")) {
                    return format!("[:{t} #{id}] {s}→{e}");
                }
            }
            value.to_string()
        }
        Json::Array(items) if !items.is_empty() && items.iter().all(Json::is_object) => {
            items.iter().map(cell_text).collect::<Vec<_>>().join(", ")
        }
        other => other.to_string(),
    }
}

/// CSV of the grid's rows, cells as full JSON where they are not scalars.
pub fn to_csv(columns: &[String], rows: &[Vec<Json>]) -> String {
    fn escape(text: &str) -> String {
        if text.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", text.replace('"', "\"\""))
        } else {
            text.to_string()
        }
    }
    let mut out = columns.iter().map(|c| escape(c)).collect::<Vec<_>>().join(",");
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|v| match v {
                Json::Null => String::new(),
                Json::String(s) => escape(s),
                other => escape(&other.to_string()),
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

pub struct ResultTableDelegate {
    columns: Vec<Column>,
    rows: Vec<Vec<SharedString>>,
    visible: Vec<usize>,
}

impl ResultTableDelegate {
    fn new() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            visible: Vec::new(),
        }
    }

    fn set_result(&mut self, result: &QueryResult) {
        let rows: Vec<Vec<String>> = result
            .rows
            .iter()
            .map(|row| row.iter().map(cell_text).collect())
            .collect();
        let mut columns = vec![Column::new("#", "#").width(crate::ui::scale::design(56.)).text_right().resizable(false)];
        columns.extend(result.columns.iter().enumerate().map(|(ix, name)| {
            // Use the same CJK-aware, UI-scaled fit as dashboard tables.
            let width = fit_column_width(name, &rows, ix, 0.);
            let numeric = result.rows.iter().take(50).all(|r| r.get(ix).is_some_and(|v| v.is_number() || v.is_null()));
            let mut column = Column::new(format!("c{ix}"), name.clone()).width(width);
            column.paddings = Some(cell_paddings());
            if numeric {
                column.text_right()
            } else {
                column
            }
        }));
        self.rows = rows.into_iter().map(|row| row.into_iter().map(SharedString::from).collect()).collect();
        self.columns = columns;
        self.set_filter("");
    }

    fn set_filter(&mut self, filter: &str) {
        let needle = filter.trim().to_lowercase();
        self.visible = (0..self.rows.len())
            .filter(|&ix| needle.is_empty() || self.rows[ix].iter().any(|c| c.to_lowercase().contains(&needle)))
            .collect();
    }

    fn row_text(&self, row_ix: usize) -> String {
        self.rows[self.visible[row_ix]].iter().map(|c| c.to_string()).collect::<Vec<_>>().join("\t")
    }
}

impl TableDelegate for ResultTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.visible.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn context_menu(&mut self, row_ix: usize, menu: PopupMenu, _: &mut Window, _: &mut Context<TableState<Self>>) -> PopupMenu {
        let text = self.row_text(row_ix);
        menu.item(
            PopupMenuItem::new(tr("results.copy_row"))
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))),
        )
    }

    fn render_th(&mut self, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        h_flex().size_full().paddings(cell_paddings()).child(
            Label::new(column.name.clone())
                .text_align(column.align)
                .text_sm()
                .font_family(cx.theme().mono_font_family.clone())
                .font_weight(FontWeight::BOLD)
                .when(col_ix == 0, |this| this.text_color(cx.theme().muted_foreground))
                .flex_1()
                .min_w_0(),
        )
    }

    fn render_td(&mut self, row_ix: usize, col_ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let text: SharedString = if col_ix == 0 {
            (row_ix + 1).to_string().into()
        } else {
            self.rows[self.visible[row_ix]].get(col_ix - 1).cloned().unwrap_or_default()
        };
        let muted = col_ix == 0 || text == "NULL";
        let copy = text.clone();
        h_flex()
            .id(("cell", row_ix * 10_000 + col_ix))
            .size_full()
            .paddings(cell_paddings())
            .overflow_hidden()
            .when(col_ix > 0, |this| {
                this.cursor_pointer().on_click(move |event, window, cx| {
                    if event.click_count() >= 2 {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
                        window.push_notification(tr("results.copied"), cx);
                    }
                })
            })
            .child(
                Label::new(text)
                    .text_align(column.align)
                    .text_ellipsis()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(muted, |this| this.text_color(cx.theme().muted_foreground))
                    .flex_1()
                    .min_w_0(),
            )
    }
}

use gpui_kit::component::WindowExt;

pub struct ResultsPanel {
    state: Entity<AppState>,
    content: Content,
    view: View,
    table: Entity<TableState<ResultTableDelegate>>,
    filter: Entity<InputState>,
    canvas: Entity<GraphCanvas>,
    _subscriptions: Vec<Subscription>,
}

impl ResultsPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| TableState::new(ResultTableDelegate::new(), window, cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(tr("results.filter")));
        let canvas = cx.new(|cx| GraphCanvas::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&filter, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                this.table.update(cx, |table, cx| {
                    table.delegate_mut().set_filter(&text);
                    table.refresh(cx);
                });
                cx.notify();
            }
        })];
        Self {
            state,
            content: Content::Empty,
            view: View::Table,
            table,
            filter,
            canvas,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_running(&mut self, cx: &mut Context<Self>) {
        self.content = Content::Running;
        cx.notify();
    }

    pub fn set_result(&mut self, result: QueryResult, window: &mut Window, cx: &mut Context<Self>) {
        let result = Rc::new(result);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_result(&result);
            table.refresh(cx);
        });
        self.filter.update(cx, |input, cx| input.set_value("", window, cx));
        match &result.graph {
            Some(graph) => {
                if graph.nodes.is_empty() && self.view == View::Graph {
                    self.view = View::Table;
                }
                let graph = graph.clone();
                self.canvas.update(cx, |canvas, cx| canvas.set_data(graph, window, cx));
            }
            None if self.view == View::Graph => self.view = View::Table,
            None => {}
        }
        if self.view == View::Plan {
            self.view = View::Table;
        }
        self.content = Content::Rows(result);
        cx.notify();
    }

    pub fn set_plan(&mut self, plan: String, cx: &mut Context<Self>) {
        self.content = Content::Plan(plan.into());
        self.view = View::Plan;
        cx.notify();
    }

    /// What the tab shows, for `khonsu open --state`: columns, counts and the
    /// first 20 rows, the plan, or the error — as DuckLocal reports a tab.
    pub fn summary(&self) -> serde_json::Value {
        use serde_json::json;
        match &self.content {
            Content::Empty => json!({"status": "empty"}),
            Content::Running => json!({"status": "running"}),
            Content::Plan(plan) => json!({"status": "plan", "plan": plan.to_string()}),
            Content::Error(error) => json!({"status": "error", "error": error}),
            Content::Rows(result) => json!({
                "status": "rows",
                "columns": result.columns,
                "row_count": result.row_count,
                "truncated": result.truncated,
                "rows": result.rows.iter().take(20).collect::<Vec<_>>(),
                "graph": result.graph.as_ref().map(|g| json!({"nodes": g.nodes.len(), "relationships": g.edges.len()})),
            }),
        }
    }

    pub fn set_error(&mut self, error: QueryError, cx: &mut Context<Self>) {
        self.content = Content::Error(error);
        cx.notify();
    }

    fn has_graph(&self) -> bool {
        matches!(&self.content, Content::Rows(r) if r.graph.as_ref().is_some_and(|g| !g.nodes.is_empty()))
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |input, cx| input.set_value("", window, cx));
        // Programmatic input changes need not emit InputEvent::Change.
        self.table.update(cx, |table, cx| {
            table.delegate_mut().set_filter("");
            table.refresh(cx);
        });
        cx.notify();
    }

    fn export_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Rows(result) = &self.content else {
            return;
        };
        let csv = to_csv(&result.columns, &result.rows);
        let dir = dirs_home();
        let rx = cx.prompt_for_new_path(&dir, Some("result.csv"));
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = rx.await else {
                    return;
                };
                let written = smol::unblock({
                    let path = path.clone();
                    move || std::fs::write(&path, csv)
                })
                .await;
                cx.update(|window, cx| match written {
                    Ok(()) => window.push_notification(trf("results.exported", &[&path.display().to_string()]), cx),
                    Err(e) => window.push_notification(
                        gpui_kit::component::notification::Notification::error(e.to_string()),
                        cx,
                    ),
                })
                .ok();
            })
            .detach();
    }

    fn render_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view;
        let has_rows = matches!(self.content, Content::Rows(_));
        let has_plan = matches!(self.content, Content::Plan(_));
        let graph_count = match &self.content {
            Content::Rows(r) => r.graph.as_ref().map(|g| g.nodes.len()),
            _ => None,
        };
        let switch = |id: &'static str, label: String, target: View, enabled: bool, cx: &mut Context<Self>| {
            Button::new(id)
                .xsmall()
                .label(label)
                .when(view == target, |b| b.primary())
                .when(view != target, |b| b.ghost())
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.view = target;
                    cx.notify();
                }))
        };
        let summary = match &self.content {
            Content::Rows(r) if view == View::Table && !self.filter.read(cx).value().trim().is_empty() => Some(trf(
                "results.filtered_rows",
                &[&self.table.read(cx).delegate().visible.len().to_string(), &r.rows.len().to_string()],
            )),
            Content::Rows(r) => Some(if r.truncated {
                trf("results.rows_truncated", &[&r.rows.len().to_string(), &r.row_count.to_string()])
            } else {
                trf("results.rows", &[&r.row_count.to_string()])
            }),
            _ => None,
        };
        let elapsed = match &self.content {
            Content::Rows(r) => Some(crate::state::format_duration(r.elapsed_ms)),
            _ => None,
        };
        h_flex()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(switch("view-table", tr("results.table").into(), View::Table, has_rows, cx))
            .child(switch(
                "view-graph",
                match graph_count {
                    Some(n) => format!("{} · {n}", tr("results.graph")),
                    None => tr("results.graph").into(),
                },
                View::Graph,
                self.has_graph(),
                cx,
            ))
            .child(switch("view-plan", tr("results.plan").into(), View::Plan, has_plan, cx))
            .child(div().w_3())
            .when(view == View::Table && has_rows, |this| {
                this.child(div().w(px(220.)).child(Input::new(&self.filter).xsmall().cleanable(true)))
            })
            .children(summary.map(|s| div().text_xs().text_color(cx.theme().muted_foreground).child(s)))
            .children(elapsed.map(|s| div().ml_2().text_xs().text_color(cx.theme().muted_foreground).child(s)))
            .child(div().flex_1())
            .when(has_rows, |this| {
                this.child(
                    Button::new("export-csv")
                        .xsmall()
                        .ghost()
                        .icon(gpui_kit::assets::IconName::Download)
                        .label(tr("results.export_csv"))
                        .tooltip(tr("results.export_csv_hint"))
                        .on_click(cx.listener(|this, _, window, cx| this.export_csv(window, cx))),
                )
            })
    }
}

fn dirs_home() -> std::path::PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}

fn render_error(error: &QueryError, cx: &App) -> impl IntoElement {
    let position = match (error.line, error.column) {
        (Some(line), Some(column)) => Some(trf("results.error_at", &[&line.to_string(), &column.to_string()])),
        (Some(line), None) => Some(trf("results.error_line", &[&line.to_string()])),
        _ => None,
    };
    let copy = format!(
        "{}{}\n{}",
        error.kind,
        position.as_ref().map(|p| format!(" — {p}")).unwrap_or_default(),
        error.message,
    );
    v_flex()
        .m_3()
        .p_3()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().danger)
        .bg(cx.theme().danger.opacity(0.06))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Icon::new(IconName::CircleX).small().text_color(cx.theme().danger))
                .child(div().font_semibold().child(error.kind.clone()))
                .children(position.map(|p| div().text_color(cx.theme().muted_foreground).child(p)))
                .child(div().flex_1())
                .child(
                    Button::new("copy-query-error")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .label(tr("results.copy_error"))
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            window.push_notification(tr("results.copied"), cx);
                        }),
                ),
        )
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .text_sm()
                .whitespace_normal()
                .child(error.message.clone()),
        )
}

impl Render for ResultsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty_graph = {
            let state = self.state.read(cx);
            state.schema.as_ref().is_some_and(|s| s.is_empty()) && !state.read_only()
        };
        let visible_rows = self.table.read(cx).delegate().visible.len();
        let body: AnyElement = match &self.content {
            Content::Empty if empty_graph => crate::ui::sources::empty_graph_prompt(self.state.clone(), cx).into_any_element(),
            Content::Empty => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(tr("results.empty"))
                .into_any_element(),
            Content::Running => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(gpui_kit::component::spinner::Spinner::new())
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(tr("results.running")))
                .into_any_element(),
            Content::Error(error) => render_error(error, cx).into_any_element(),
            Content::Plan(plan) => div()
                .id("plan")
                .size_full()
                .overflow_scroll()
                .p_3()
                .font_family(cx.theme().mono_font_family.clone())
                .text_sm()
                .whitespace_nowrap()
                .children(plan.lines().map(|l| div().child(l.to_string())))
                .into_any_element(),
            Content::Rows(_) if self.view == View::Graph => self.canvas.clone().into_any_element(),
            Content::Rows(result) => v_flex()
                .size_full()
                .when(result.truncated, |this| {
                    this.child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .bg(cx.theme().warning.opacity(0.12))
                            .child(trf("results.truncated", &[&result.rows.len().to_string()])),
                    )
                })
                .when(result.rows.is_empty(), |this| {
                    this.child(
                        v_flex().flex_1().items_center().justify_center().gap_2().p_4().text_center()
                            .child(div().font_semibold().child(tr("results.no_rows")))
                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(tr("results.no_rows_hint"))),
                    )
                })
                .when(!result.rows.is_empty() && visible_rows == 0, |this| {
                    this.child(
                        v_flex().flex_1().items_center().justify_center().gap_2().p_4().text_center()
                            .child(div().font_semibold().child(tr("results.no_matches")))
                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(tr("results.no_matches_hint")))
                            .child(
                                Button::new("clear-result-filter")
                                    .outline()
                                    .small()
                                    .label(tr("results.clear_filter"))
                                    .on_click(cx.listener(|this, _, window, cx| this.clear_filter(window, cx))),
                            ),
                    )
                })
                .when(visible_rows > 0, |this| {
                    this.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(DataTable::new(&self.table).small().stripe(true).scrollbar_visible(true, true)),
                    )
                })
                .into_any_element(),
        };
        let show_bar = matches!(self.content, Content::Rows(_) | Content::Plan(_));
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .when(show_bar, |this| this.child(self.render_bar(cx)))
            .child(div().flex_1().min_h_0().child(body))
    }
}

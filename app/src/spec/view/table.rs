//! The grid a `table` plot draws in.

use super::*;


/// Compact cell padding shared by header and body cells.
pub(super) fn cell_paddings() -> Edges<Pixels> {
    Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    }
}

/// The table a `table` plot draws: the query's own columns, numeric ones
/// right-aligned, no row numbers or per-cell actions — the results panel's
/// table minus everything a dashboard does not need.
pub(super) struct SpecTableDelegate {
    columns: Vec<Column>,
    result: Arc<QueryResult>,
    /// The result's rows in the order shown: `order[shown]` is the row of
    /// `result`. The query's own order until a header sorts it.
    pub(super) order: Vec<usize>,
    /// The order before the last sort, until the view has moved the selected
    /// row along with it: the table keeps a selection by position, and a
    /// pick is a row, not a position.
    pub(super) resorted: Option<Vec<usize>>,
    /// The column of the cell last right-clicked, for the menu's copies: the
    /// table tells the menu only the row.
    menu_col: Option<usize>,
}

impl SpecTableDelegate {
    pub(super) fn new(result: Arc<QueryResult>) -> Self {
        let columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                // The header also holds the sort arrow beside its name.
                let width = fit_column_width(&column.name, &result.rows, ix, SORT_ICON_WIDTH);
                let mut spec = Column::new(format!("c{ix}"), column.name.clone())
                    .width(width)
                    .sortable();
                if column.kind == ColumnKind::Numeric {
                    spec = spec.text_right();
                }
                spec.paddings = Some(cell_paddings());
                spec
            })
            .collect();
        let order = (0..result.rows.len()).collect();
        Self {
            columns,
            result,
            order,
            resorted: None,
            menu_col: None,
        }
    }

    /// The cell at `col` of the row shown at `shown`.
    fn cell(&self, shown: usize, col: usize) -> &str {
        self.result
            .rows
            .get(self.source_row(shown))
            .and_then(|row| row.get(col))
            .map_or("", String::as_str)
    }

    /// The row shown at `shown`, its cells tab-separated, as a spreadsheet
    /// pastes a row.
    pub(super) fn row_text(&self, shown: usize) -> String {
        (0..self.columns.len())
            .map(|col| self.cell(shown, col))
            .collect::<Vec<_>>()
            .join("	")
    }

    /// Column `col`'s cells in the order shown, one per line, as a
    /// spreadsheet pastes a column.
    pub(super) fn column_text(&self, col: usize) -> String {
        (0..self.order.len())
            .map(|shown| self.cell(shown, col))
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }

    /// The row of the result shown at `shown`.
    pub(super) fn source_row(&self, shown: usize) -> usize {
        self.order.get(shown).copied().unwrap_or(shown)
    }
}

impl TableDelegate for SpecTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.result.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let order = sorted_rows(&self.result, col_ix, sort);
        self.resorted = Some(std::mem::replace(&mut self.order, order));
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let copy = |label: &'static str, text: String| {
            PopupMenuItem::new(tr(label))
                .icon(IconName::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                })
        };
        let mut menu = menu;
        if let Some(col) = self.menu_col.filter(|&col| col < self.columns.len()) {
            menu = menu.item(copy(
                "dashboard.table.copy_cell",
                self.cell(row_ix, col).to_string(),
            ));
        }
        menu = menu.item(copy("dashboard.table.copy_row", self.row_text(row_ix)));
        if let Some(col) = self.menu_col.filter(|&col| col < self.columns.len()) {
            menu = menu.item(copy("dashboard.table.copy_column", self.column_text(col)));
        }
        menu
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        h_flex()
            .size_full()
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .child(
                Label::new(column.name.clone())
                    .text_align(column.align)
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .font_weight(FontWeight::BOLD)
                    .flex_1(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let text = self.cell(row_ix, col_ix).to_string();
        let is_null = text == "NULL";
        h_flex()
            .size_full()
            // Before the row's own right-click, which opens the menu: the
            // menu then knows which cell it is for.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, _| table.delegate_mut().menu_col = Some(col_ix)),
            )
            .when_some(column.paddings, |this, paddings| this.paddings(paddings))
            .overflow_hidden()
            .child(
                Label::new(text)
                    .text_align(column.align)
                    .text_ellipsis()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(is_null, |this| this.text_color(cx.theme().muted_foreground))
                    .flex_1()
                    .min_w_0(),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::SpecTableDelegate;
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};
    use crate::ui::grid::sorted_rows;
    use gpui_kit::component::table::ColumnSort;
    use std::sync::Arc;

    #[test]
    fn a_table_sorts_numbers_as_numbers_and_nulls_last() {
        let column = |name: &str, kind| ColumnMeta {
            name: name.into(),
            kind,
        };
        let result = QueryResult {
            columns: vec![
                column("name", ColumnKind::Text),
                column("n", ColumnKind::Numeric),
            ],
            rows: [["b", "10"], ["a", "9"], ["c", "NULL"], ["d", "1,200"]]
                .iter()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect(),
            truncated: false,
            graph: None,
        };
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Ascending), [1, 0, 3, 2]);
        assert_eq!(
            sorted_rows(&result, 1, ColumnSort::Descending),
            [3, 0, 1, 2]
        );
        assert_eq!(sorted_rows(&result, 0, ColumnSort::Ascending), [1, 0, 2, 3]);
        // A third click goes back to the query's own order.
        assert_eq!(sorted_rows(&result, 1, ColumnSort::Default), [0, 1, 2, 3]);
    }

    #[test]
    fn a_tables_copies_follow_the_shown_order() {
        let column = |name: &str| ColumnMeta {
            name: name.into(),
            kind: ColumnKind::Text,
        };
        let result = QueryResult {
            columns: vec![column("channel"), column("region")],
            rows: [["Web", "East"], ["App", "North"]]
                .iter()
                .map(|row| row.iter().map(|cell| cell.to_string()).collect())
                .collect(),
            truncated: false,
            graph: None,
        };
        let mut table = SpecTableDelegate::new(Arc::new(result));
        assert_eq!(table.row_text(0), "Web	East");
        assert_eq!(
            table.column_text(1),
            "East
North"
        );
        table.order = sorted_rows(&table.result, 0, ColumnSort::Ascending);
        assert_eq!(table.cell(0, 0), "App");
        assert_eq!(table.row_text(0), "App	North");
        assert_eq!(
            table.column_text(0),
            "App
Web"
        );
    }
}

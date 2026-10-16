//! Grid helpers DuckLocal's dashboard tables share with its results grid:
//! fitting a column to its text and sorting rows by a column.

use gpui_kit::component::table::ColumnSort;
use gpui_kit::*;

use crate::query::{ColumnKind, QueryResult};
use crate::ui::chart::parse_number;


/// Compact cell padding shared by header and body cells.
pub(crate) fn cell_paddings() -> Edges<Pixels> {
    Edges {
        top: px(2.),
        bottom: px(2.),
        left: px(10.),
        right: px(10.),
    }
}

/// Advance of one narrow character in the table's monospace cell font at
/// the default interface size; a wide (CJK, fullwidth) character takes two.
/// An estimate made without the text system, so it errs a little wide rather
/// than clip. Scaled with the interface size, as the font is.
const CELL_CHAR_WIDTH: f32 = 9.6;
/// A fitted column is never narrower than this, so short values and
/// one-letter headers still leave room to grab the resize handle.
const MIN_FIT_WIDTH: f32 = 80.;
/// Nor wider than this: one long cell must not push every other column off
/// screen. The rest ellipsizes, and the column can still be dragged wider.
const MAX_FIT_WIDTH: f32 = 360.;
/// Rows sampled to fit a column. A result set can hold 100,000 rows; the
/// first few hundred are what the user sees first.
const FIT_SAMPLE_ROWS: usize = 200;

/// Width for column `col` of `rows` that shows its header and sampled cells
/// unclipped, within [`MIN_FIT_WIDTH`, `MAX_FIT_WIDTH`]. `extra` is room the
/// cell spends on something besides its text, such as a copy button. Columns
/// that together exceed the viewport scroll horizontally.
pub(crate) fn fit_column_width(header: &str, rows: &[Vec<String>], col: usize, extra: f32) -> f32 {
    let paddings = cell_paddings();
    let chrome = f32::from(paddings.left + paddings.right) + extra;
    // At a larger interface size the text is wider than the estimate made at
    // the default one, and a column fitted to it clipped its own values.
    let unit = f32::from(crate::ui::scale::design(1.));
    let (min, max) = (MIN_FIT_WIDTH * unit, MAX_FIT_WIDTH * unit);
    let width = |cols: usize| (cols as f32 * CELL_CHAR_WIDTH * unit + chrome).clamp(min, max);
    let mut fitted = width(display_columns(header));
    for row in rows.iter().take(FIT_SAMPLE_ROWS) {
        // Once capped, scanning more cells cannot change the width. JSON
        // and long text columns otherwise scan up to 200 large strings.
        if fitted >= max {
            break;
        }
        if let Some(cell) = row.get(col) {
            fitted = fitted.max(width(display_columns(cell)));
        }
    }
    fitted
}

/// Monospace columns `text` occupies on its widest line.
fn display_columns(text: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    text.lines().map(UnicodeWidthStr::width).max().unwrap_or(0)
}

pub(crate) fn sorted_rows(result: &QueryResult, col: usize, sort: ColumnSort) -> Vec<usize> {
    let mut order: Vec<usize> = (0..result.rows.len()).collect();
    if sort == ColumnSort::Default {
        return order;
    }
    let cell = |row: usize| {
        result
            .rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(String::as_str)
            .filter(|cell| *cell != "NULL")
    };
    let numeric = result
        .columns
        .get(col)
        .is_some_and(|c| c.kind == ColumnKind::Numeric);
    let compare = |a: &str, b: &str| {
        if numeric {
            if let (Some(a), Some(b)) = (parse_number(a), parse_number(b)) {
                return a.total_cmp(&b);
            }
        }
        a.cmp(b)
    };
    let descending = sort == ColumnSort::Descending;
    order.sort_by(|&a, &b| match (cell(a), cell(b)) {
        (Some(a), Some(b)) if descending => compare(b, a),
        (Some(a), Some(b)) => compare(a, b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    order
}

pub(crate) fn starts_with_ignore_case(name: &str, prefix_lower: &str) -> bool {
    if name.is_ascii() && prefix_lower.is_ascii() {
        name.len() >= prefix_lower.len()
            && name.as_bytes()[..prefix_lower.len()].eq_ignore_ascii_case(prefix_lower.as_bytes())
    } else {
        name.to_lowercase().starts_with(prefix_lower)
    }
}

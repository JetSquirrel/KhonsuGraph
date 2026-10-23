//! A query result in the shape DuckLocal's dashboard and chart code draws:
//! named columns with a kind, and every cell as the text the grid shows.
//! HawDB results carry no column types, so a column's kind is read off its
//! values: all numbers → numeric, all ISO dates or times → temporal, all
//! booleans → boolean, anything else → text. Nodes and relationships show as
//! the grid shows them, `(:Label name #id)` and `[:TYPE #id] a→b`.

use khonsu_core::Language;
use serde_json::Value as Json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Numeric,
    Text,
    Boolean,
    Temporal,
    Other,
}

#[derive(Clone, Debug)]
pub struct ColumnMeta {
    pub name: String,
    pub kind: ColumnKind,
}

#[derive(Clone, Debug)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
    /// The nodes and relationships the result holds, for `graph` plots.
    pub graph: Option<khonsu_core::GraphData>,
}

#[derive(Clone, Debug)]
pub enum QueryOutcome {
    Rows(QueryResult),
}

impl QueryResult {
}

/// `2026-10-15`, `2026-10-15T08:30:00Z`, `2026-10-15 08:30`.
fn looks_temporal(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && (b.len() == 10 || matches!(b[10], b'T' | b' '))
}

fn kind_of(values: &[&Json]) -> ColumnKind {
    let present: Vec<&&Json> = values.iter().filter(|v| !v.is_null()).collect();
    if present.is_empty() {
        return ColumnKind::Other;
    }
    if present.iter().all(|v| v.is_number()) {
        ColumnKind::Numeric
    } else if present.iter().all(|v| v.is_boolean()) {
        ColumnKind::Boolean
    } else if present.iter().all(|v| v.as_str().is_some_and(looks_temporal)) {
        ColumnKind::Temporal
    } else {
        ColumnKind::Text
    }
}

/// Text for a cell; a NULL is `NULL`, as in DuckLocal's grid.
pub fn cell(value: &Json) -> String {
    match value {
        Json::Null => "NULL".into(),
        Json::Number(n) => n.to_string(),
        other => crate::ui::results::cell_text(other),
    }
}

impl From<&khonsu_core::QueryResult> for QueryResult {
    fn from(result: &khonsu_core::QueryResult) -> Self {
        let columns = result
            .columns
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                let values: Vec<&Json> = result.rows.iter().filter_map(|r| r.get(ix)).collect();
                ColumnMeta {
                    name: name.clone(),
                    kind: kind_of(&values),
                }
            })
            .collect();
        Self {
            columns,
            rows: result.rows.iter().map(|row| row.iter().map(cell).collect()).collect(),
            truncated: result.truncated,
            graph: result.graph.clone(),
        }
    }
}

/// Run a dashboard query on the window's graph, as DuckLocal's view runs one
/// on its connection.
pub fn run(language: Language, text: &str) -> anyhow::Result<QueryOutcome> {
    let result = crate::db::run(language, text).map_err(|e| anyhow::anyhow!(e.message))?;
    Ok(QueryOutcome::Rows(QueryResult::from(&result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds_come_from_values() {
        assert_eq!(kind_of(&[&json!(1), &Json::Null, &json!(2.5)]), ColumnKind::Numeric);
        assert_eq!(kind_of(&[&json!("2026-10-15"), &json!("2026-10-16T08:00:00Z")]), ColumnKind::Temporal);
        assert_eq!(kind_of(&[&json!("web"), &json!("2026-10-15")]), ColumnKind::Text);
        assert_eq!(kind_of(&[&json!(true)]), ColumnKind::Boolean);
        assert_eq!(kind_of(&[&Json::Null]), ColumnKind::Other);
    }
}

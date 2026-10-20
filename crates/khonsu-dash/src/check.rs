//! `khonsu check FILE --db GRAPH`: validate a spec and run every query.
//!
//! As in DuckLocal: parse, validate, then run each query read-only — the way
//! opening the dashboard runs it, with nothing picked — and compare every
//! plot's columns with the ones the query returned. One run reports every
//! mistake it finds. A filter is also tried with a value really picked from
//! its plot's rows, so a predicate a query cannot use fails here, not on the
//! first click.
//!
//! HawDB results carry no declared column types, so a column's type is what
//! its values were: `number`, `text`, `boolean`, `node`, `relationship`,
//! `list`, `map`, `mixed`, or `null` when every value was NULL.

use std::collections::HashMap;
use std::path::Path;

use khonsu_core::{Graph, Language, QueryOptions, QueryResult};
use serde_json::{json, Map, Value as Json};

use crate::filter::{self, Pick};
use crate::model::{self, Diagnostic, Note, Spec};

/// Rows a check reads per query: enough to judge types and pick a value.
const CHECK_ROWS: usize = 200;

/// Why a check failed: `spec` for mistakes in the file (exit 2), `sql` for
/// queries the graph refused (exit 1), `io` for a file that could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckError {
    pub kind: &'static str,
    pub message: String,
}

impl CheckError {
    pub fn exit_code(&self) -> u8 {
        if self.kind == "spec" {
            2
        } else {
            1
        }
    }
}

fn located(path: &str, diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|d| format!("{path}:{}: {}", d.line, d.message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The type a column's values have, as described in the module docs.
pub fn column_type(values: impl Iterator<Item = Json>) -> &'static str {
    let mut seen: Option<&'static str> = None;
    for value in values {
        let ty = match &value {
            Json::Null => continue,
            Json::Number(_) => "number",
            Json::String(_) => "text",
            Json::Bool(_) => "boolean",
            Json::Array(_) => "list",
            Json::Object(map) if map.contains_key("_id") && map.contains_key("labels") => "node",
            Json::Object(map) if map.contains_key("_id") && map.contains_key("source_id") => "relationship",
            Json::Object(_) => "map",
        };
        match seen {
            None => seen = Some(ty),
            Some(previous) if previous != ty => return "mixed",
            Some(_) => {}
        }
    }
    seen.unwrap_or("null")
}

/// `(name, type)` for every column of a result.
pub fn columns_of(result: &QueryResult) -> Vec<(String, String)> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(ix, name)| {
            let ty = column_type(result.rows.iter().map(|row| row.get(ix).cloned().unwrap_or(Json::Null)));
            (name.clone(), ty.to_string())
        })
        .collect()
}

/// The text to run for `query` with `picks` in place.
pub fn statement(spec: &Spec, query: &model::Query, picks: &HashMap<String, Pick>) -> String {
    filter::apply(&query.text, query.language, picks, &spec.targets_of(query))
}

/// The value a filter would pick from its plot's result: the first row's
/// column, or for a graph the first node's property.
fn sample_pick(spec: &Spec, filter: &model::Filter, results: &HashMap<String, QueryResult>) -> Option<Json> {
    let plot = spec.plots.iter().find(|p| p.name == filter.plot)?;
    let result = results.get(&plot.query)?;
    if plot.kind == "graph" {
        let graph = result.graph.as_ref()?;
        return graph.nodes.iter().find_map(|n| n.properties.get(&filter.column).cloned());
    }
    let ix = result.columns.iter().position(|c| c.eq_ignore_ascii_case(&filter.column))?;
    result.rows.first().and_then(|row| row.get(ix).cloned())
}

pub fn check_file(file: &Path, graph: &mut Graph, summary: bool) -> Result<Json, CheckError> {
    let path = file.display().to_string();
    let source = std::fs::read_to_string(file).map_err(|e| CheckError {
        kind: "io",
        message: format!("{path}: {e}"),
    })?;
    check_source(&path, &source, graph, summary)
}

pub fn check_source(path: &str, source: &str, graph: &mut Graph, summary: bool) -> Result<Json, CheckError> {
    let parsed = model::parse(source).map_err(|e| CheckError {
        kind: "spec",
        message: format!("{path}:{}: {}", e.line, e.message),
    })?;
    // A flawed spec is still checked as far as it goes — its sound queries
    // run, their plots' columns compared — so one run reports every mistake.
    let (spec, mut diagnostics) = model::validate_partial(&parsed);
    let spec_ok = diagnostics.is_empty();

    let mut failures = Vec::new();
    let mut results: HashMap<String, QueryResult> = HashMap::new();
    let mut columns: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let none = HashMap::new();
    for query in &spec.queries {
        // Empty, writing or misplaced-filter queries already have their
        // diagnostic; running them would only say it again.
        if query.text.trim().is_empty()
            || model::writing_keyword(query.language, &query.text).is_some()
            || !filter::placement_problems(&query.text, query.language).is_empty()
        {
            continue;
        }
        let text = statement(&spec, query, &none);
        match graph.run(query.language, &text, &Map::new(), QueryOptions { max_rows: CHECK_ROWS }) {
            Ok(result) => {
                columns.insert(query.name.clone(), columns_of(&result));
                results.insert(query.name.clone(), result);
            }
            Err(error) => failures.push(format!("{path}:{}: query {:?}: {}", query.line, query.name, error.message)),
        }
    }

    // A graph plot needs nodes or relationships to draw.
    for plot in spec.plots.iter().filter(|p| p.kind == "graph") {
        if let Some(result) = results.get(&plot.query) {
            let draws = result.graph.as_ref().is_some_and(|g| !g.is_empty());
            let typed = columns
                .get(&plot.query)
                .is_some_and(|cols| cols.iter().any(|(_, ty)| ty == "node" || ty == "relationship"));
            if !draws && !typed && result.row_count > 0 {
                diagnostics.push(Diagnostic {
                    line: plot.line,
                    col: plot.col,
                    span: plot.span,
                    message: format!(
                        "graph {:?}: query {:?} returns no nodes or relationships; return them whole (RETURN a, r, b), not their properties",
                        plot.name, plot.query
                    ),
                });
            }
        }
    }

    // Each filter, really picked: the value from its plot's rows, in every
    // other query that reads it.
    for filter in &spec.filters {
        let Some(value) = sample_pick(&spec, filter, &results) else {
            continue;
        };
        let pick = Pick {
            column: filter.column.clone(),
            expr: filter.match_expr.clone(),
            value,
        };
        let picking_query = spec.plots.iter().find(|p| p.name == filter.plot).map(|p| p.query.clone());
        for query in spec.queries.iter().filter(|q| spec.filters_of(q).iter().any(|f| f.name == filter.name)) {
            if Some(&query.name) == picking_query.as_ref() || !results.contains_key(&query.name) {
                continue;
            }
            let picks = HashMap::from([(filter.name.clone(), pick.clone())]);
            let text = statement(&spec, query, &picks);
            if let Err(error) = graph.run(query.language, &text, &Map::new(), QueryOptions { max_rows: 1 }) {
                failures.push(format!(
                    "{path}:{}: query {:?} with filter {:?} picked ({}): {}",
                    query.line,
                    query.name,
                    filter.name,
                    pick.predicate(query.language),
                    error.message
                ));
            }
        }
    }

    // Plots whose query ran get their columns checked; a graph filter picks
    // a node property, not a result column, so it is not looked up there.
    let mut checked = spec.clone();
    checked.plots.retain(|plot| results.contains_key(&plot.query));
    checked.filters.retain(|filter| {
        spec.plots
            .iter()
            .any(|plot| plot.name == filter.plot && plot.kind != "graph" && results.contains_key(&plot.query))
    });
    let lookup = |name: &str| -> Result<Vec<(String, String)>, String> {
        Ok(columns.get(name).cloned().unwrap_or_default())
    };
    diagnostics.extend(model::check_columns(&checked, &lookup));

    if !diagnostics.is_empty() {
        diagnostics.sort_by_key(|d| (d.line, d.col));
        let mut message = located(path, &diagnostics);
        for failure in &failures {
            message.push('\n');
            message.push_str(failure);
        }
        return Err(CheckError { kind: "spec", message });
    }
    if !failures.is_empty() {
        let _ = spec_ok;
        return Err(CheckError {
            kind: "sql",
            message: failures.join("\n"),
        });
    }

    // Passing is not the same as showing something: a pattern or a filter
    // that matches nothing returns no rows and no error.
    let warnings: Vec<String> = spec
        .queries
        .iter()
        .filter(|q| results.get(&q.name).is_some_and(|r| r.row_count == 0))
        .map(|q| format!("{path}:{}: query {:?} returns no rows with nothing picked; its plots will be empty", q.line, q.name))
        .collect();
    if summary {
        return Ok(json!({
            "file": path,
            "ok": true,
            "queries": spec.queries.len(),
            "filters": spec.filters.len(),
            "plots": spec.plots.len(),
            "warnings": warnings,
        }));
    }
    Ok(json!({
        "file": path,
        "ok": true,
        "warnings": warnings,
        "queries": spec.queries.iter().map(|q| {
            let mut entry = json!({
                "name": q.name,
                "language": match q.language { Language::Cypher => "cypher", Language::Sql => "sql" },
                "line": q.line,
            });
            if let Some(result) = results.get(&q.name) {
                entry["rows"] = json!(result.row_count);
                entry["columns"] = json!(columns[&q.name].iter().map(|(name, ty)| json!({"name": name, "type": ty})).collect::<Vec<_>>());
                if let Some(graph) = &result.graph {
                    entry["graph"] = json!({"nodes": graph.nodes.len(), "relationships": graph.edges.len()});
                    // Counted on the rows a check reads, not the whole result.
                    if result.truncated {
                        entry["graph"]["sampled_rows"] = json!(result.rows.len());
                    }
                }
            }
            entry
        }).collect::<Vec<_>>(),
        "filters": spec.filters.iter().map(|f| json!({
            "name": f.name,
            "plot": f.plot,
            "column": f.column,
            "match": f.match_expr,
            "queries": spec.queries.iter()
                .filter(|q| spec.filters_of(q).iter().any(|used| used.name == f.name))
                .map(|q| q.name.clone())
                .collect::<Vec<_>>(),
            "line": f.line,
        })).collect::<Vec<_>>(),
        "plots": spec.plots.iter().map(|p| json!({
            "name": p.name,
            "type": p.kind,
            "query": p.query,
            "x": (!p.x.is_empty()).then_some(&p.x),
            "y": p.y,
            "series": p.series,
            "value": p.value,
            "caption": p.caption,
            "group": p.group,
            "note": p.note.as_ref().map(|note| match note {
                Note::Text(words) => json!({"text": words}),
                Note::Column(column) => json!({"column": column}),
            }),
            "tone": p.tone,
            "title": p.title,
            "width": p.width(),
            "line": p.line,
        })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn column_types_come_from_values() {
        assert_eq!(column_type([json!(1), Json::Null, json!(2.5)].into_iter()), "number");
        assert_eq!(column_type([json!("a"), json!(1)].into_iter()), "mixed");
        assert_eq!(column_type([Json::Null].into_iter()), "null");
        assert_eq!(column_type([json!({"_id": 1, "labels": ["A"]})].into_iter()), "node");
        assert_eq!(
            column_type([json!({"_id": 1, "type": "R", "source_id": 0, "target_id": 1})].into_iter()),
            "relationship"
        );
    }
}

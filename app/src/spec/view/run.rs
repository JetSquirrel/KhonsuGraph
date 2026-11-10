//! Running a spec: reading it, running its queries off the main thread, and
//! landing the outcome.

use super::*;

impl Dashboard {
    /// Re-read the spec and re-run every query on the window's connection: the
    /// toolbar's reload, and what a change of database asks for. A run in
    /// flight wins; a second request while it runs would only redo it.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.reload_now(cx);
    }

    /// The graph changed under the dashboard: re-run everything, now or, with
    /// a run in flight that read the graph as it was, as soon as it lands.
    pub fn graph_written(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.reload_pending = true;
        } else {
            self.reload_now(cx);
        }
    }

    pub(super) fn reload_now(&mut self, cx: &mut Context<Self>) {
        if self.showing_source && !self.is_dirty(cx) {
            // The source view shows the file as it is now, not as it was when
            // toggled open — but a dirty buffer is the user's work, and a
            // reload is no reason to lose it.
            self.read_source();
        }
        // A reload re-reads the data as well as the spec: nothing is reused.
        self.start_run(HashMap::new(), cx);
    }

    /// Re-run after a pick changed: every query whose SQL the picks leave as
    /// it was keeps its result, so only the narrowed plots redraw.
    pub(super) fn refilter(&mut self, cx: &mut Context<Self>) {
        if self.running {
            self.refilter_pending = true;
            return;
        }
        self.start_run(self.outcomes.clone(), cx);
    }

    pub(super) fn start_run(&mut self, reuse: Outcomes, cx: &mut Context<Self>) {
        self.running = true;
        cx.notify();
        let path = self.path.clone();
        let picks = self.picks.clone();
        cx.spawn(async move |this, cx| {
            let run = smol::unblock(move || load(&path, &picks, &reuse)).await;
            this.update(cx, |this, cx| {
                this.running = false;
                match run.spec_error {
                    // A reload that failed never replaces a working dashboard:
                    // the previous one stays up and the reason appears above
                    // it — the difference between a typo while editing and
                    // losing the dashboard to it.
                    Some(error) if !this.plots.is_empty() => {
                        this.stale_reason = Some(error);
                    }
                    spec_error => {
                        this.spec_error = spec_error;
                        this.stale_reason = None;
                        // A table whose query came back as the very same
                        // result keeps its entity: its scroll position and
                        // its selected row — the pick — are the user's.
                        let tables = run
                            .plots
                            .iter()
                            .map(|plot| {
                                let old = this
                                    .plots
                                    .iter()
                                    .position(|p| p.name == plot.name && p.kind == "table")?;
                                let same = match (
                                    this.results.get(&plot.query),
                                    run.results.get(&plot.query),
                                ) {
                                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                                    _ => false,
                                };
                                same.then(|| this.tables.get(old).cloned().flatten())
                                    .flatten()
                            })
                            .collect::<Vec<_>>();
                        this.table_subscriptions.retain(|(id, _)| {
                            tables
                                .iter()
                                .flatten()
                                .any(|table| table.entity_id() == *id)
                        });
                        this.tables = tables;
                        let titles: HashMap<String, String> = this
                            .plots
                            .iter()
                            .map(|plot| {
                                let title = if plot.title.is_empty() {
                                    plot.name.clone()
                                } else {
                                    plot.title.clone()
                                };
                                (plot.name.clone(), title)
                            })
                            .collect();
                        let mut plots = run.plots;
                        for plot in &mut plots {
                            if let Some(before) = this.plots.iter().find(|p| p.name == plot.name) {
                                keep_series_order(plot, &before.series_names);
                            }
                        }
                        this.plots = plots;
                        this.results = run.results;
                        this.outcomes = run.outcomes;
                        this.spec = run.spec;
                        // An edit on disk: the plots it changed, added and
                        // took out are an agent's, when one has been at work.
                        let prints = run.prints;
                        if std::mem::take(&mut this.external_reload) {
                            let before = &this.plot_prints;
                            let mut changed = Vec::new();
                            let mut added = Vec::new();
                            for (name, print) in &prints {
                                match before.get(name) {
                                    None => added.push(name.clone()),
                                    Some(old) if old != print => changed.push(name.clone()),
                                    Some(_) => {}
                                }
                            }
                            let mut removed: Vec<(String, String)> = before
                                .keys()
                                .filter(|name| !prints.contains_key(*name))
                                .map(|name| {
                                    let title =
                                        titles.get(name).cloned().unwrap_or_else(|| name.clone());
                                    (name.clone(), title)
                                })
                                .collect();
                            changed.sort();
                            added.sort();
                            removed.sort();
                            this.presence
                                .edited(&changed, &added, &removed, Instant::now());
                            this.keep_presence_fresh(cx);
                        }
                        this.plot_prints = prints;
                        // A pick whose filter is gone from the spec goes too.
                        let spec = this.spec.clone();
                        this.picks.retain(|name, _| {
                            spec.as_ref()
                                .is_some_and(|spec| spec.filters.iter().any(|f| &f.name == name))
                        });
                    }
                }
                if std::mem::take(&mut this.reload_pending) {
                    this.refilter_pending = false;
                    this.reload_now(cx);
                } else if std::mem::take(&mut this.refilter_pending) {
                    this.refilter(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Each plot's definition as text — its block as written and its query's
/// SQL — so two runs can tell which plots an edit changed. Where a block sits
/// in the file is no part of it: an edit above it moves it, not changes it.
pub(super) fn plot_prints(spec: &Spec, source: &str) -> HashMap<String, String> {
    // A block runs from its keyword to the next block's: the spans the model
    // keeps mark only where each starts.
    let mut starts: Vec<usize> = spec
        .queries
        .iter()
        .map(|block| block.span.0)
        .chain(spec.plots.iter().map(|block| block.span.0))
        .chain(spec.filters.iter().map(|block| block.span.0))
        .collect();
    starts.sort_unstable();
    spec.plots
        .iter()
        .map(|plot| {
            let start = plot.span.0;
            let end = starts
                .iter()
                .copied()
                .find(|&next| next > start)
                .unwrap_or(source.len());
            let block = source.get(start..end).unwrap_or_default().trim_end();
            let sql = spec
                .queries
                .iter()
                .find(|query| query.name == plot.query)
                .map(|query| query.text.as_str())
                .unwrap_or_default();
            // Whitespace aside: an editor that rewrites line endings or
            // re-indents the file has changed no plot.
            let text = format!("{block}\n{sql}");
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (plot.name.clone(), text)
        })
        .collect()
}

/// Everything one run of a dashboard produces, computed off the UI thread.
pub(super) struct Run {
    spec_error: Option<String>,
    spec: Option<Arc<Spec>>,
    /// Each plot's definition as text (see `plot_prints`).
    prints: HashMap<String, String>,
    plots: Vec<PreparedPlot>,
    results: HashMap<String, Arc<QueryResult>>,
    outcomes: Outcomes,
}

/// Read, parse, validate, run, prepare. Blocking — call it off the UI thread.
/// `picks` narrow the queries that read their filters; a query whose SQL
/// comes out as `reuse` last ran it keeps that outcome instead of running.
pub(super) fn load(path: &Path, picks: &HashMap<String, Pick>, reuse: &Outcomes) -> Run {
    let display = path.display().to_string();
    let spec = (|| -> Result<(Spec, String), String> {
        let source = std::fs::read_to_string(path).map_err(|e| format!("{display}: {e}"))?;
        let file =
            model::parse(&source).map_err(|e| format!("{display}:{}: {}", e.line, e.message))?;
        let spec = model::validate(&file).map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(|d| format!("{display}:{}: {}", d.line, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        Ok((spec, source))
    })();
    let (spec, prints) = match spec {
        Ok((spec, source)) => {
            let prints = plot_prints(&spec, &source);
            (spec, prints)
        }
        Err(spec_error) => {
            return Run {
                spec_error: Some(spec_error),
                spec: None,
                prints: HashMap::new(),
                plots: Vec::new(),
                results: HashMap::new(),
                outcomes: HashMap::new(),
            };
        }
    };

    // Every query runs once, however many plots draw from it, and only after
    // it has been shown to read and nothing else: opening a file someone sent
    // must not be what runs its `DROP` or `COPY … TO`.
    // The check is on the SQL as written; what runs has the sources it names
    // in front of it, which only read.
    // Picks go in before the sources, which only read.
    let _ = path;
    let zone = spec.timezone.as_deref().and_then(khonsu_dash::zone::Zone::parse);
    let mut outcomes: Outcomes = HashMap::new();
    for query in &spec.queries {
        let sql = match model::writing_keyword(query.language, &query.text) {
            Some(word) => Err(trf("dashboard.query_writes", &[&query.name, &word])),
            None => Ok(crate::spec::filter::apply(&query.text, query.language, &picks_for(&spec, query, picks))),
        };
        let entry = match sql {
            Err(error) => (String::new(), Err(error)),
            Ok(sql) => match reuse.get(&query.name) {
                Some((ran, outcome)) if *ran == sql => (sql, outcome.clone()),
                _ => {
                    let outcome = match crate::query::run(query.language, &sql) {
                        Ok(QueryOutcome::Rows(result)) => Ok(Arc::new(match zone {
                            Some(zone) => in_zone(result, zone, &picked_columns(&spec, &query.name)),
                            None => result,
                        })),
                        Err(e) => Err(format!("{e:#}")),
                    };
                    (sql, outcome)
                }
            },
        };
        outcomes.insert(query.name.clone(), entry);
    }

    // validate() has already said every plot's query exists.
    let plots = spec
        .plots
        .iter()
        .map(|plot| prepare(plot, outcomes.get(&plot.query).map(|(_, outcome)| outcome)))
        .collect();

    let results = outcomes
        .iter()
        .filter_map(|(name, (_, outcome))| {
            outcome
                .as_ref()
                .ok()
                .map(|result| (name.clone(), result.clone()))
        })
        .collect();

    Run {
        spec_error: None,
        spec: Some(Arc::new(spec)),
        prints,
        plots,
        results,
        outcomes,
    }
}

/// `result` with the times in its time columns moved to `zone`, except in the
/// columns named in `keep`: a filter picks those, and its pick has to match
/// the graph's value as stored. A time that names no offset stays as it is.
pub(super) fn in_zone(mut result: QueryResult, zone: khonsu_dash::zone::Zone, keep: &[String]) -> QueryResult {
    let columns: Vec<usize> = result
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == ColumnKind::Temporal && !keep.iter().any(|k| k.eq_ignore_ascii_case(&c.name)))
        .map(|(ix, _)| ix)
        .collect();
    for row in &mut result.rows {
        for &ix in &columns {
            if let Some(moved) = row.get(ix).and_then(|cell| zone.convert(cell)) {
                row[ix] = moved;
            }
        }
    }
    result
}

/// The columns of `query` a filter picks on a plot that draws it.
pub(super) fn picked_columns(spec: &Spec, query: &str) -> Vec<String> {
    spec.filters
        .iter()
        .filter(|f| spec.plots.iter().any(|p| p.name == f.plot && p.query == query))
        .map(|f| f.column.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::{in_zone, load, plot_prints, Run};
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};
    use crate::spec::filter::Pick;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn times_move_to_the_dashboards_zone_except_where_a_filter_picks() {
        let column = |name: &str, kind| ColumnMeta { name: name.into(), kind };
        let result = QueryResult {
            columns: vec![column("at", ColumnKind::Temporal), column("seen", ColumnKind::Temporal), column("host", ColumnKind::Text)],
            rows: vec![
                vec!["1970-01-01T02:15:00Z".into(), "1970-01-01T02:15:00Z".into(), "1970-01-01T02:15:00Z".into()],
                vec!["1970-01-01 10:15".into(), "NULL".into(), "web".into()],
            ],
            truncated: false,
            graph: None,
        };
        let zone = khonsu_dash::zone::Zone::parse("Asia/Hong_Kong").unwrap();
        let moved = in_zone(result, zone, &["SEEN".to_string()]);
        assert_eq!(moved.rows[0], ["1970-01-01 10:15:00", "1970-01-01T02:15:00Z", "1970-01-01T02:15:00Z"]);
        assert_eq!(moved.rows[1], ["1970-01-01 10:15", "NULL", "web"], "a time without an offset stays");
    }

    const BOARD: &str = r#"
query "by_channel" { cypher = "MATCH (o:Order) RETURN o.channel AS channel, count(o) AS amount ORDER BY channel" }
query "total" { cypher = "MATCH (o:Order) WHERE $channel RETURN count(o) AS total" }
query "count" { cypher = "MATCH (o:Order) RETURN count(o) AS n" }
plot "channels" { type = "bar" query = query.by_channel x = channel y = amount }
plot "total" { type = "card" query = query.total }
plot "count" { type = "card" query = query.count }
filter "channel" {
  plot  = plot.channels
  match = "o.channel"
}
"#;

    fn total(run: &Run) -> String {
        run.results["total"].rows[0][0].clone()
    }

    #[test]
    fn a_pick_narrows_the_queries_that_read_it_and_reuses_the_rest() {
        let base = std::env::var_os("KHONSU_TEST_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("view-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::create(&dir.join("graph")).unwrap();
        for statement in [
            "CREATE (:Order {channel: 'web', amount: 10})",
            "CREATE (:Order {channel: 'shop', amount: 20})",
            "CREATE (:Order {channel: 'web', amount: 5})",
            "CREATE (:Order {amount: 1})",
        ] {
            crate::db::run(khonsu_core::Language::Cypher, statement).unwrap();
        }
        let path = dir.join("board.dash");
        std::fs::write(&path, BOARD).unwrap();

        let fresh = load(&path, &HashMap::new(), &HashMap::new());
        assert_eq!(fresh.spec_error, None);
        assert_eq!(total(&fresh), "4");

        let mut picks = HashMap::new();
        picks.insert(
            "channel".to_string(),
            Pick {
                column: "channel".into(),
                expr: Some("o.channel".into()),
                values: vec![serde_json::json!("web")],
            },
        );
        let narrowed = load(&path, &picks, &fresh.outcomes);
        assert_eq!(total(&narrowed), "2");
        // The picking plot's own query is not narrowed, and neither it nor a
        // query that reads no filter ran again: the same result comes back.
        for unchanged in ["by_channel", "count"] {
            assert!(
                Arc::ptr_eq(&fresh.results[unchanged], &narrowed.results[unchanged]),
                "{unchanged} ran again"
            );
        }
        assert_eq!(narrowed.results["by_channel"].rows.len(), 3);

        // The grid's NULL picks the orders with no channel.
        picks.get_mut("channel").unwrap().values = vec![serde_json::Value::Null];
        assert_eq!(total(&load(&path, &picks, &narrowed.outcomes)), "1");

        // A reload reuses nothing: every query runs.
        let reloaded = load(&path, &picks, &HashMap::new());
        assert!(!Arc::ptr_eq(&fresh.results["count"], &reloaded.results["count"]));
        crate::db::close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_plots_an_edit_touches_print_differently() {
        let prints = |source: &str| {
            let file = crate::spec::model::parse(source).unwrap();
            let spec = crate::spec::model::validate(&file).unwrap();
            plot_prints(&spec, source)
        };
        let spec = |title: &str, sql: &str| {
            format!(
                "query \"q\" {{
  sql = \"{sql}\"
}}
                 plot \"a\" {{
  type = \"card\"
  query = query.q
  title = \"{title}\"
}}
                 plot \"b\" {{
  type = \"card\"
  query = query.q
  title = \"B\"
}}
"
            )
        };
        let before = prints(&spec("A", "SELECT 1 AS n"));
        // A longer title moves plot b down the file without changing it.
        let retitled = prints(&spec("A, renamed at length", "SELECT 1 AS n"));
        assert_ne!(before["a"], retitled["a"]);
        assert_eq!(before["b"], retitled["b"]);
        // Line endings and indentation are not a change.
        let crlf = spec("A", "SELECT 1 AS n").replace('\n', "\r\n    ");
        assert_eq!(prints(&crlf)["b"], before["b"]);
        // A query both read changes both.
        let requeried = prints(&spec("A", "SELECT 2 AS n"));
        assert_ne!(before["b"], requeried["b"]);
    }
}

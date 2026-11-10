//! Filters: what a click on a bar, a row or a node picks, and the chips that
//! show and clear the picks.

use super::*;

impl Dashboard {
    /// Whether a click on this plot picks a value: some filter names it.
    pub(super) fn is_pickable(&self, plot: &str) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|spec| spec.filters.iter().any(|f| f.plot == plot))
    }

    /// The band a bar plot shows as picked: the value of a filter that picks
    /// the plot's own x.
    pub(super) fn picked_bands(&self, plot: &PreparedPlot) -> Vec<SharedString> {
        let Some(spec) = self.spec.as_ref() else {
            return Vec::new();
        };
        spec.filters
            .iter()
            .filter(|f| f.plot == plot.name && f.column.eq_ignore_ascii_case(&plot.label_name))
            .find_map(|f| self.picks.get(&f.name))
            .map(|pick| pick.values.iter().map(|value| SharedString::from(pick_text(value))).collect())
            .unwrap_or_default()
    }

    /// Pick `value` for `filter`: one that takes several adds it, or takes it
    /// out when it is in already; another replaces its pick, or with `toggle`
    /// clears it when `value` is what it has. Whether anything changed.
    pub(super) fn pick(&mut self, filter: &model::Filter, value: serde_json::Value, toggle: bool) -> bool {
        if filter.multiple {
            let pick = self.picks.entry(filter.name.clone()).or_insert_with(|| Pick {
                column: filter.column.clone(),
                expr: filter.match_expr.clone(),
                values: Vec::new(),
            });
            pick.toggle(value);
            if pick.values.is_empty() {
                self.picks.remove(&filter.name);
            }
            return true;
        }
        let pick = Pick {
            column: filter.column.clone(),
            expr: filter.match_expr.clone(),
            values: vec![value],
        };
        if self.picks.get(&filter.name) == Some(&pick) {
            if toggle {
                self.picks.remove(&filter.name);
            }
            toggle
        } else {
            self.picks.insert(filter.name.clone(), pick);
            true
        }
    }

    /// A click on a pickable bar plot: the band under it becomes the pick —
    /// or, when it is the band already picked, the pick is cleared.
    pub(super) fn bar_clicked(&mut self, ix: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let plot = &self.plots[ix];
        let Some(bounds) = self.plot_bounds.get(&plot.name).and_then(|cell| cell.get()) else {
            return;
        };
        let local = point(position.x - bounds.origin.x, position.y - bounds.origin.y);
        let Some(band) = GroupedBars::new("dashboard-hit", plot).band_at(local, bounds) else {
            return;
        };
        let Some(result) = self.results.get(&plot.query).cloned() else {
            return;
        };
        // The row the bar came from: the first whose x is the band.
        let Some(x) = result
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&plot.label_name))
        else {
            return;
        };
        let Some(row) = result.rows.iter().position(|row| {
            row.get(x)
                .is_some_and(|cell| cell.as_str() == band.as_ref())
        }) else {
            return;
        };
        let name = plot.name.clone();
        self.pick_row(&name, &result, row, true, cx);
    }

    /// Pick, for every filter on `plot`, its column's value in `row`. With
    /// `toggle`, a pick that is already the one made clears instead.
    pub(super) fn pick_row(
        &mut self,
        plot: &str,
        result: &QueryResult,
        row: usize,
        toggle: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = self.spec.clone() else {
            return;
        };
        let mut changed = false;
        for filter in spec.filters.iter().filter(|f| f.plot == plot) {
            let Some(column) = result
                .columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&filter.column))
            else {
                continue;
            };
            let Some(cell) = result.rows.get(row).and_then(|r| r.get(column)) else {
                continue;
            };
            let numeric = result.columns[column].kind == ColumnKind::Numeric;
            changed |= self.pick(filter, pick_value(cell, numeric), toggle);
        }
        if changed {
            self.refilter(cx);
            cx.notify();
        }
    }

    /// A node selected on a `graph` panel: every filter on it picks the
    /// node's property named by its `column`; selecting the picked node
    /// again clears.
    pub(super) fn pick_node(&mut self, plot: &str, node: &khonsu_core::GraphNode, cx: &mut Context<Self>) {
        let Some(spec) = self.spec.clone() else {
            return;
        };
        let mut changed = false;
        for filter in spec.filters.iter().filter(|f| f.plot == plot) {
            let Some(value) = node.properties.get(&filter.column) else {
                continue;
            };
            changed |= self.pick(filter, value.clone(), true);
        }
        if changed {
            self.refilter(cx);
            cx.notify();
        }
    }

    /// Clear one filter's pick, or every pick; a table it was picked on lets
    /// go of its selected row.
    pub(super) fn clear_picks(&mut self, only: Option<&str>, cx: &mut Context<Self>) {
        let cleared: Vec<String> = self
            .picks
            .keys()
            .filter(|name| only.is_none_or(|only| only == name.as_str()))
            .cloned()
            .collect();
        if cleared.is_empty() {
            return;
        }
        for name in &cleared {
            self.picks.remove(name);
        }
        if let Some(spec) = self.spec.clone() {
            for filter in spec.filters.iter().filter(|f| cleared.contains(&f.name)) {
                for (ix, plot) in self.plots.iter().enumerate() {
                    if plot.name == filter.plot {
                        if let Some(Some(table)) = self.tables.get(ix) {
                            table.update(cx, |table, cx| table.clear_selection(cx));
                        }
                    }
                }
            }
        }
        self.refilter(cx);
        cx.notify();
    }

    /// The picks in force, as chips above the plots: each says what it
    /// narrows to and clears on its ✕.
    pub(super) fn render_picks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.picks.is_empty() {
            return None;
        }
        let spec = self.spec.clone()?;
        let chips = spec
            .filters
            .iter()
            .filter_map(|filter| Some((filter, self.picks.get(&filter.name)?)))
            .enumerate()
            .map(|(ix, (filter, pick))| {
                let name = filter.name.clone();
                let value = pick.values.iter().map(pick_text).collect::<Vec<_>>().join(", ");
                Button::new(("dashboard-pick", ix))
                    .outline()
                    .xsmall()
                    .label(format!("{} = {value}", filter.column))
                    .icon(IconName::Close)
                    .tooltip(tr("dashboard.filter.clear"))
                    .on_click(cx.listener(move |this, _, _, cx| this.clear_picks(Some(&name), cx)))
            })
            .collect::<Vec<_>>();
        let several = chips.len() > 1;
        Some(
            h_flex()
                .flex_none()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_1p5()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("dashboard.filter.label")),
                )
                .children(chips)
                .when(several, |this| {
                    this.child(
                        Button::new("dashboard-picks-clear")
                            .ghost()
                            .xsmall()
                            .label(tr("dashboard.filter.clear_all"))
                            .on_click(cx.listener(|this, _, _, cx| this.clear_picks(None, cx))),
                    )
                })
                .into_any_element(),
        )
    }
}

/// A picked cell as the value its predicate compares: a number when its
/// column holds numbers, NULL for a cell showing NULL, text otherwise.
pub(super) fn pick_value(cell: &str, numeric: bool) -> serde_json::Value {
    if cell == "NULL" {
        return serde_json::Value::Null;
    }
    if numeric {
        if let Ok(n) = cell.parse::<i64>() {
            return serde_json::Value::from(n);
        }
        if let Some(n) = cell.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(cell.to_string())
}

/// A pick as its chip shows it.
pub(super) fn pick_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".into(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The picks a query sees: each filter it reads that has one — except a
/// filter picked on a plot this very query draws, which would otherwise
/// narrow the plot down to the one value just clicked.
pub(super) fn picks_for(
    spec: &Spec,
    query: &model::Query,
    picks: &HashMap<String, Pick>,
) -> HashMap<String, Pick> {
    spec.filters_of(query)
        .into_iter()
        .filter(|filter| {
            spec.plots
                .iter()
                .find(|plot| plot.name == filter.plot)
                .is_none_or(|plot| plot.query != query.name)
        })
        .filter_map(|filter| {
            let pick = picks.get(&filter.name)?;
            // The column is the spec's as it stands, not as it was clicked.
            Some((
                filter.name.clone(),
                Pick {
                    column: filter.column.clone(),
                    expr: filter.match_expr.clone(),
                    values: pick.values.clone(),
                },
            ))
        })
        .collect()
}

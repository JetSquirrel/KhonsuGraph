//! One plot's panel: its chart, card, text, table or graph, and the SQL on its
//! back.

use super::*;

impl Dashboard {
    /// The canvas for graph panel `ix`, made on first use and fed the
    /// panel's data whenever a run brings new data.
    pub(super) fn graph_canvas(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<GraphCanvas>> {
        let plot = &self.plots[ix];
        let data = plot.graph.clone()?;
        let name = plot.name.clone();
        let caption = plot.caption.clone();
        let group = plot.group.clone();
        let (labels, highlight) = (plot.labels.clone(), plot.highlight.clone());
        let style = plot.style.clone();
        let enlarged = self.maximized.as_deref() == Some(name.as_str());
        if !self.graph_canvases.contains_key(&name) {
            let canvas = cx.new(|cx| GraphCanvas::new(window, cx));
            let picked = name.clone();
            let subscription = cx.subscribe(&canvas, move |this, _, event: &CanvasEvent, cx| match event {
                CanvasEvent::NodeSelected(node) => this.pick_node(&picked, node, cx),
            });
            self.graph_canvases.insert(name.clone(), (canvas, None, subscription));
        }
        let (canvas, shown, _) = self.graph_canvases.get_mut(&name)?;
        let canvas = canvas.clone();
        if shown.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &data)) {
            *shown = Some(data.clone());
            canvas.update(cx, |canvas, cx| {
                canvas.configure(caption, group, true, cx);
                canvas.set_style(style);
                canvas.set_look(labels.as_deref(), highlight.as_deref(), cx);
                canvas.set_compact(!enlarged, window, cx);
                canvas.set_data((*data).clone(), window, cx);
            });
        }
        Some(canvas)
    }

    /// One panel of the grid: the plot, or — turned over by the SQL button
    /// in its top-right corner — the SQL behind it. Beside that button, the
    /// comment button opens the plot's comment card over the dashboard.
    /// Both stay out of the way until the pointer is over the plot, except
    /// that a plot with open threads keeps their count in sight.
    pub(super) fn render_plot(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group = SharedString::from(format!("dashboard-plot-{ix}"));
        let shown = self.sql_shown.contains(&self.plots[ix].name);
        let face = if shown {
            self.render_sql(ix, cx)
        } else {
            self.render_plot_face(ix, window, cx)
        };
        let name = self.plots[ix].name.clone();
        // Icons, like the comment and enlarge buttons beside it: the query
        // behind the plot as code, and the plot to turn back to.
        let toggle = Button::new(("dashboard-sql", ix))
            .xsmall()
            .ghost()
            .icon(Icon::new(if shown {
                AssetIcon::ChartNoAxesColumn
            } else {
                AssetIcon::Code
            }))
            .tooltip(if shown {
                tr("dashboard.sql.hide_tooltip")
            } else {
                tr("dashboard.sql.show_tooltip")
            })
            .on_click(cx.listener({
                let name = name.clone();
                move |this, _, _, cx| {
                    if !this.sql_shown.remove(&name) {
                        this.sql_shown.insert(name.clone());
                    }
                    cx.notify();
                }
            }));
        let is_max = self.maximized.as_deref() == Some(name.as_str());
        let maximize = Button::new(("dashboard-maximize", ix))
            .xsmall()
            .ghost()
            .icon(Icon::new(if is_max {
                AssetIcon::Minimize2
            } else {
                AssetIcon::Maximize2
            }))
            .tooltip(if is_max {
                tr("dashboard.restore_plot.tooltip")
            } else {
                tr("dashboard.maximize.tooltip")
            })
            .on_click(cx.listener({
                let name = name.clone();
                move |this, _, window, cx| {
                    this.toggle_maximized(&name, window, cx);
                }
            }));
        // A text panel runs no query: there is no SQL to show.
        let has_sql = self.plots[ix].kind != "text";
        let comments = self.render_comment_button(ix, window, cx);
        let open = self.comments.open_on(&name);
        let commenting = self.comment_open.as_deref() == Some(name.as_str());
        let hover_only = |this: Div, keep: bool| {
            if keep {
                this
            } else {
                this.opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
            }
        };
        div()
            .size_full()
            .relative()
            .group(group.clone())
            .child(face)
            .child(
                h_flex()
                    .absolute()
                    .top_1p5()
                    .right_2()
                    .gap_0p5()
                    .child(hover_only(div(), open > 0 || commenting).child(comments))
                    .when(has_sql, |this| {
                        this.child(
                            hover_only(div(), shown)
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().background)
                                .child(toggle),
                        )
                    })
                    .child(
                        div()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().background)
                            .child(maximize),
                    ),
            )
            .into_any_element()
    }

    /// The SQL a plot's query last ran — sources and the current picks
    /// included, so it runs as it stands in a query tab — or, before it has
    /// run, the query as written.
    pub(super) fn render_sql(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let plot = &self.plots[ix];
        let title = plot.title.clone();
        let sql = self
            .outcomes
            .get(&plot.query)
            .map(|(sql, _)| sql.clone())
            .or_else(|| {
                self.spec.as_ref().and_then(|spec| {
                    spec.queries
                        .iter()
                        .find(|q| q.name == plot.query)
                        .map(|q| q.text.clone())
                })
            })
            .unwrap_or_default();
        let sql = sql.trim().to_string();
        let styles = crate::spec::highlight::sql_styles(&sql, &*cx.theme().highlight_theme);
        let copied = sql.clone();
        let opened = sql.clone();
        // Headed as the plot's face is, so flipping to the SQL does not move
        // the title.
        v_flex()
            .size_full()
            .px(px(CARD_INSET))
            .pt_4()
            .pb(px(CARD_INSET))
            .gap_2()
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .text_ellipsis()
                    .pr_16()
                    .child(title),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(("dashboard-sql-copy", ix))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Copy)
                            .tooltip(tr("dashboard.sql.copy"))
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                            }),
                    )
                    .child(
                        Button::new(("dashboard-sql-open", ix))
                            .xsmall()
                            .ghost()
                            .label(tr("dashboard.sql.open"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DashboardEvent::OpenSql { sql: opened.clone() })
                            })),
                    ),
            )
            .child(
                div()
                    .id(("dashboard-sql-text", ix))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(StyledText::new(sql).with_highlights(styles)),
            )
            .into_any_element()
    }

    pub(super) fn render_plot_face(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.plots[ix].kind == "card" && self.plots[ix].failure.is_none() {
            let size = self
                .card_sizes
                .get(&ix)
                .copied()
                .unwrap_or_else(|| self.card_value_size(ix));
            return render_card(&self.plots[ix], size, cx);
        }
        if self.plots[ix].kind == "text" {
            return render_text(&self.plots[ix], cx);
        }
        let (title, mut notice, failure, is_table, is_empty) = {
            let plot = &self.plots[ix];
            let is_empty = match plot.kind.as_str() {
                "map" => plot.geo.as_ref().is_none_or(|geo| geo.point_count() == 0),
                "graph" => plot.graph.as_ref().is_none_or(|g| g.is_empty()),
                "timeline" => plot.timeline.as_ref().is_none_or(|t| t.events.is_empty()),
                "pie" => plot.pie.is_empty(),
                _ => plot.points.is_empty(),
            };
            (
                plot.title.clone(),
                plot.notice.clone(),
                plot.failure.clone(),
                plot.kind == "table",
                is_empty,
            )
        };
        // The axis subtitle the results chart shows: which series, over which
        // x column — for a pie, what the shares are of; for a map, which
        // columns place the points. Tables and failures have nothing to say.
        let drawn = !is_table && failure.is_none() && !is_empty;
        if failure.is_none() && self.is_pickable(&self.plots[ix].name) {
            let hint = tr("dashboard.filter.hint").to_string();
            notice = Some(match notice.take() {
                Some(notice) => crate::i18n::join(&notice, &hint),
                None => hint,
            });
        }
        let mut legend = Vec::new();
        let axes = drawn.then(|| {
            let plot = &self.plots[ix];
            let series = plot.series_names.join(", ");
            match plot.kind.as_str() {
                "map" => {
                    if let Some(geo) = &plot.geo {
                        let (notes, key) = map_notes(geo, cx);
                        notice = match (notice.take(), notes) {
                            (Some(a), Some(b)) => Some(crate::i18n::join(&a, &b)),
                            (a, b) => a.or(b),
                        };
                        legend = key;
                    }
                    plot.label_name.clone()
                }
                "graph" => {
                    let graph = plot.graph.as_ref();
                    let nodes = graph.map_or(0, |g| g.nodes.len());
                    let mut counts = trf(
                        "dashboard.graph.counts",
                        &[&nodes.to_string(), &graph.map_or(0, |g| g.edges.len()).to_string()],
                    );
                    // Say when the picture is not the whole result: rows the
                    // dashboard dropped, or nodes past what the canvas draws.
                    if let Some(rows) = plot.graph_rows_cut {
                        counts = crate::i18n::join(&counts, &trf("dashboard.graph.rows_cut", &[&rows.to_string()]));
                    }
                    if nodes > crate::ui::canvas::NODE_LIMIT {
                        counts = crate::i18n::join(
                            &counts,
                            &trf("canvas.truncated", &[&crate::ui::canvas::NODE_LIMIT.to_string(), &nodes.to_string()]),
                        );
                    }
                    counts
                }
                "timeline" => {
                    let events = plot.timeline.as_ref().map_or(0, |t| t.events.len());
                    match plot.series_names.first() {
                        Some(lanes) => trf("timeline.title_lanes", &[&events.to_string(), &plot.label_name, lanes]),
                        None => trf("timeline.title", &[&events.to_string(), &plot.label_name]),
                    }
                }
                "pie" => {
                    legend = pie_parts(&plot.pie, series.clone().into(), "dashboard-pie-key", cx).1;
                    trf("chart.pie.title", &[&series, &plot.label_name])
                }
                _ => {
                    // Several series need a key; one is named in the title.
                    if plot.series_names.len() > 1 {
                        legend = plot
                            .series_names
                            .iter()
                            .enumerate()
                            .map(|(s, name)| {
                                (
                                    crate::ui::palette::color(s, cx),
                                    SharedString::from(name.clone()),
                                )
                            })
                            .collect();
                    }
                    trf("chart.title.by", &[&series, &plot.label_name])
                }
            }
        });
        // What is plotted, then what a click does, behind an info mark beside
        // the title, so a grid of plots does not spend a line on each. A key
        // that already names the series says the first half better, so a
        // keyed line or bar plot leaves it out.
        let keyed_xy = !legend.is_empty() && !matches!(self.plots[ix].kind.as_str(), "pie" | "map");
        let subtitle = match (axes.filter(|_| !keyed_xy), notice) {
            (Some(axes), Some(notice)) => Some(crate::i18n::join(&axes, &notice)),
            (axes, notice) => axes.or(notice),
        };

        let body = if let Some(message) = failure {
            div()
                .size_full()
                .p_3()
                .child(
                    Alert::error(("dashboard-plot-error", ix), message)
                        .title(tr("dashboard.plot_failed")),
                )
                .into_any_element()
        } else if is_table {
            self.render_table(ix, window, cx)
        } else if is_empty {
            empty_chart(cx)
        } else if self.plots[ix].kind == "graph" {
            match self.graph_canvas(ix, window, cx) {
                Some(canvas) => div().size_full().child(canvas).into_any_element(),
                None => empty_chart(cx),
            }
        } else if self.plots[ix].kind == "bar" && self.is_pickable(&self.plots[ix].name) {
            let picked = self.picked_bands(&self.plots[ix]);
            let chart = GroupedBars::new(
                (
                    ElementId::from("dashboard-chart"),
                    self.plots[ix].name.clone(),
                ),
                &self.plots[ix],
            )
            .selected(picked);
            // The chart fills this box, so the box's bounds are the chart's:
            // the canvas records them for the click to hit-test against.
            let bounds = self
                .plot_bounds
                .entry(self.plots[ix].name.clone())
                .or_default()
                .clone();
            div()
                .size_full()
                .px_2()
                .pb_2()
                .child(
                    div()
                        .size_full()
                        .relative()
                        .cursor_pointer()
                        .child(chart)
                        .child(
                            canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.bar_clicked(ix, event.position, cx)
                            }),
                        ),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .px_2()
                .pb_2()
                .child(chart_element(ix, &self.plots[ix], cx))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .px(px(CARD_INSET))
                    .pt_4()
                    .pb_2()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1p5()
                            // Clear of the SQL and comment buttons.
                            .pr_16()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_base()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .when_some(subtitle, |this, subtitle| {
                                this.child(plot_info(ix, subtitle, cx))
                            }),
                    )
                    .when(!legend.is_empty(), |this| {
                        this.child(legend_row(legend, cx))
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// A `table` plot draws its query's grid; the query is guaranteed to have
    /// succeeded, because `prepare` would have made the failure otherwise.
    pub(super) fn render_table(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.plots[ix].query.clone();
        let Some(result) = self.results.get(&query).cloned() else {
            return empty_chart(cx);
        };
        if self.tables[ix].is_none() {
            let delegate = SpecTableDelegate::new(result.clone());
            let table = cx.new(|cx| TableState::new(delegate, window, cx));
            let plot = self.plots[ix].name.clone();
            if self.is_pickable(&plot) {
                // A row selected — by click or by the arrow keys — is a pick;
                // the table's own highlight is what shows it. The row is a
                // position in the shown order, which a sort may have changed.
                let picked_from = result.clone();
                let subscription =
                    cx.subscribe(&table, move |this, table, event: &TableEvent, cx| {
                        if let TableEvent::SelectRow(row) = event {
                            let row = table.read(cx).delegate().source_row(*row);
                            this.pick_row(&plot, &picked_from, row, false, cx);
                        }
                    });
                self.table_subscriptions
                    .push((table.entity_id(), subscription));
            }
            // After a sort, the selected row moves to where its row went.
            let subscription = cx.observe(&table, |_, table, cx| {
                table.update(cx, |table, cx| {
                    let Some(before) = table.delegate_mut().resorted.take() else {
                        return;
                    };
                    let Some(selected) = table.selected_row() else {
                        return;
                    };
                    let row = before.get(selected).copied().unwrap_or(selected);
                    if let Some(shown) = table.delegate().order.iter().position(|&r| r == row) {
                        table.set_selected_row(shown, cx);
                    }
                });
            });
            self.table_subscriptions
                .push((table.entity_id(), subscription));
            self.tables[ix] = Some(table);
        }
        let table = self.tables[ix].clone().expect("built just above");
        let truncated = result.truncated;
        let row_count = result.rows.len();
        let rows = table
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .clone();
        // Stripes only once the rows fill the panel: striping pads a short
        // table with empty rows down to the panel's foot, which reads as
        // rows of nothing rather than as the end of the data.
        let overflows = rows.max_offset().y > px(0.);
        let wheel = self.wheel.clone();
        // Where the table sits, for the wheel to tell whether it is on screen.
        let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let record = bounds.clone();
        v_flex()
            .size_full()
            .when(truncated, |this| {
                this.child(
                    Alert::warning(
                        ("dashboard-truncated", ix),
                        trf("results.truncated", &[&row_count.to_string()]),
                    )
                    .banner()
                    .small(),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    // Runs after the table's own scrolling and before the
                    // stack's, so it decides whether the stack moves too.
                    .on_scroll(move |event, window, cx| {
                        if wheel.table_scrolled(ix, &rows, bounds.get(), event, window) {
                            cx.stop_propagation();
                        }
                    })
                    .child(
                        DataTable::new(&table)
                            .small()
                            .stripe(overflows)
                            .scrollbar_visible(true, true),
                    )
                    .child(
                        canvas(move |b, _, _| record.set(Some(b)), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                    ),
            )
            .into_any_element()
    }
}

/// One plot's chart element, built from the prepared data on each frame;
/// cloning the `Arc`s is a refcount bump, not a copy of the row set.
pub(super) fn chart_element(plot_ix: usize, plot: &PreparedPlot, cx: &App) -> AnyElement {
    let palette = crate::ui::palette::series(cx);
    let id = ("dashboard-chart", plot_ix);
    // The hand-built plots key their hover state and path caches on the
    // plot's name — unique after validation — which survives reordering the
    // spec's blocks where the panel index would not.
    let named_id = || (ElementId::from("dashboard-chart"), plot.name.clone());
    // A label count the axis can fit, not a stride: wide labels (dates) get
    // few ticks, and the first and last value are always among them.
    let x_labels = x_label_count(&plot.points);
    let single = plot.series_names.len() <= 1;
    let name = plot
        .series_names
        .first()
        .cloned()
        .unwrap_or_else(|| plot.title.clone());

    match plot.kind.as_str() {
        "pie" => pie_parts(&plot.pie, name.into(), named_id(), cx).0,
        "timeline" => match &plot.timeline {
            Some(timeline) => crate::spec::timeline::render(&plot.name, timeline, cx),
            None => empty_chart(cx),
        },
        "map" => match &plot.geo {
            Some(geo) => GeoPlot::new(named_id(), geo.clone()).into_any_element(),
            None => empty_chart(cx),
        },
        // Every bar draws on the hand-built chart, one series or several: it
        // colours by series, and lays a category axis on its side rather than
        // drop the labels that do not fit.
        "bar" => GroupedBars::new(named_id(), plot).into_any_element(),
        "area" if single => {
            let color = palette[0];
            AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .id(id)
                .y(|d: &Arc<PlotPoint>| d.values[0])
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.4), 1.),
                    linear_color_stop(color.opacity(0.05), 0.),
                ))
                .linear()
                .name(name)
                .x_tick_count(x_labels)
                .tooltip_value(|_: &Arc<PlotPoint>, _: usize, v: f64| format_value(v).into())
                .into_any_element()
        }
        // A scatter's points are not connected, and its x is the band axis
        // `prepare` built — the hand-built plot draws it that way, one series
        // or many.
        "scatter" => SeriesPlot::scatter(named_id(), plot).into_any_element(),
        // The catalog's one multi-series chart fills; filling is what `area`
        // asks for.
        "area" => {
            let mut chart = AreaChart::new(plot.points.clone())
                .x(|d: &Arc<PlotPoint>| d.band.clone())
                .linear()
                .id(id)
                .x_tick_count(x_labels)
                .tooltip_value(|_: &Arc<PlotPoint>, _: usize, v: f64| format_value(v).into());
            for (s, series_name) in plot.series_names.iter().enumerate() {
                let color = palette[s % palette.len()];
                chart = chart
                    .y(move |d: &Arc<PlotPoint>| d.values[s])
                    .stroke(color)
                    .fill(linear_gradient(
                        0.,
                        linear_color_stop(color.opacity(0.4), 1.),
                        linear_color_stop(color.opacity(0.05), 0.),
                    ))
                    .name(series_name.clone());
            }
            chart.into_any_element()
        }
        // A line, one series or several, draws lines and no fill, straight
        // between points and over a labelled value axis.
        _ => SeriesPlot::lines(named_id(), plot).into_any_element(),
    }
}

pub(super) fn render_card(plot: &PreparedPlot, size: f32, cx: &App) -> AnyElement {
    let value = plot.card.clone().unwrap_or_default();
    let tone = plot
        .tone
        .as_deref()
        .and_then(|name| crate::ui::palette::tone(name, cx));
    v_flex()
        .size_full()
        .px(px(CARD_INSET))
        .py_3()
        .gap_1()
        .justify_center()
        .overflow_hidden()
        // A toned card says so across its top, the way a status strip does;
        // the label underneath says what the colour means. The strip bends
        // with the card's corners — just inside its 1px border — since the
        // card clips its content to a rectangle, not to its rounded shape.
        .when_some(tone, |this, (rule, _)| {
            this.border_t_3()
                .border_color(rule)
                .rounded_t(px(PANEL_RADIUS - 1.))
        })
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .text_ellipsis()
                .child(plot.title.clone()),
        )
        .child(
            // A number is never ellipsized while a smaller size fits it:
            // `11,439,7…` reads as a different number.
            div()
                .text_size(px(size))
                .line_height(px(size * 1.25))
                // Regular weight: the size already makes it the headline, and
                // a row of bold numbers shouts over the charts below it.
                .font_weight(FontWeight::NORMAL)
                .whitespace_nowrap()
                .text_ellipsis()
                .when_some(tone, |this, (_, ink)| this.text_color(ink))
                .child(value),
        )
        .when_some(plot.note.clone(), |this, note| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .text_ellipsis()
                    .child(note),
            )
        })
        .when_some(plot.notice.clone(), |this, notice| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .text_ellipsis()
                    .child(notice),
            )
        })
        .into_any_element()
}

/// A `text` panel: a heading and the words under it, left-aligned like the
/// cards' labels so a dashboard's sections read down one edge.
pub(super) fn render_text(plot: &PreparedPlot, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        // Inset to the cards' content, so the heading and the card labels
        // under it start on one edge.
        .px(px(CARD_INSET + 1.))
        .py_3()
        .gap_1()
        .justify_center()
        .overflow_hidden()
        .when(!plot.title.is_empty(), |this| {
            this.child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::MEDIUM)
                    .text_ellipsis()
                    .child(plot.title.clone()),
            )
        })
        .when_some(plot.text.clone(), |this, text| {
            this.child(
                div()
                    .text_base()
                    .text_color(cx.theme().muted_foreground)
                    .child(text),
            )
        })
        .into_any_element()
}

/// The info mark beside a plot's title, as in a Grafana panel: what is plotted
/// and what a click does, shown when the pointer rests on it. A button, so its
/// tooltip shows and looks like the SQL and comment buttons' beside it.
pub(super) fn plot_info(ix: usize, text: String, cx: &App) -> impl IntoElement {
    Button::new(("dashboard-plot-info", ix))
        .xsmall()
        .ghost()
        .icon(Icon::new(IconName::Info).text_color(cx.theme().muted_foreground))
        .tooltip(text)
}

/// The plot body when nothing parsed: the same medium-over-muted message the
/// results chart's empty state uses.
pub(super) fn empty_chart(cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_4()
        .gap_2()
        .child(
            Icon::new(IconName::ChartPie)
                .large()
                .text_color(cx.theme().muted_foreground.alpha(0.5)),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_center()
                .child(tr("chart.empty.no_rows")),
        )
        .into_any_element()
}

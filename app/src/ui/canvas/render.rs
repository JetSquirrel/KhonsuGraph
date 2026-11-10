//! The canvas as drawn: the graph, its toolbar, legend and inspector.

use super::*;

impl Render for GraphCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let halo = theme.background;
        let muted = theme.muted_foreground;
        let warning = theme.warning;
        let border = theme.border;

        // The legend says what the colors mean: labels, or the color
        // property's values in slot order.
        let mut legend: Vec<(String, Hsla, usize)> = Vec::new();
        match self.style.color.as_deref() {
            Some(key) => {
                let look = self.look();
                let mut counts: HashMap<String, usize> = HashMap::new();
                let mut none = 0;
                for node in self.nodes.values() {
                    match group_value(node, key) {
                        Some(value) => *counts.entry(value).or_default() += 1,
                        None => none += 1,
                    }
                }
                for value in &self.color_slots {
                    if let Some(&count) = counts.get(value) {
                        let probe = GraphNode {
                            id: 0,
                            labels: Vec::new(),
                            properties: [(key.to_string(), Json::String(value.clone()))].into_iter().collect(),
                        };
                        let color = to_node(&probe, &look, cx).color.unwrap_or(muted);
                        legend.push((value.clone(), color, count));
                    }
                }
                if none > 0 {
                    legend.push((trf("canvas.no_value", &[key]), muted, none));
                }
            }
            None => {
                let mut counts: BTreeMap<String, usize> = BTreeMap::new();
                for label in self.nodes.values().map(first_label).filter(|l| !l.is_empty()) {
                    *counts.entry(label.to_string()).or_default() += 1;
                }
                legend.extend(counts.into_iter().map(|(label, count)| {
                    let color = palette::label_color(&label, cx);
                    (label, color, count)
                }));
            }
        }

        let group_keys = group_keys(self.nodes.values());
        let group_key = self.group_key.clone();
        let this = cx.entity().downgrade();

        let walk_warning = self.explorer.warning();
        let note = if let Some(warning) = walk_warning.clone() {
            warning
        } else if self.hidden > 0 {
            trf("canvas.truncated", &[&self.nodes.len().to_string(), &(self.nodes.len() + self.hidden).to_string()])
        } else if self.compact {
            tr("canvas.compact_hint").to_string()
        } else {
            tr("canvas.hint").to_string()
        };

        let maximized = self.maximized;
        let body = h_flex()
            .size_full()
            .child(
                div()
                    .id("graph-canvas")
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(self.graph.clone())
                    .child(
                        h_flex()
                            .absolute()
                            .top_2()
                            .left_2()
                            .gap_1()
                            .child(
                                Button::new("canvas-fit")
                                    .xsmall()
                                    .outline()
                                    .label(tr("canvas.fit"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.graph.update(cx, |graph, cx| graph.fit_view(cx));
                                    })),
                            )
                            .child(
                                Button::new("canvas-relayout")
                                    .xsmall()
                                    .outline()
                                    .label(tr("canvas.relayout"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.graph.update(cx, |graph, cx| graph.relayout(cx));
                                    })),
                            )
                            .when(self.explorer.can_go_back() && self.explorer.running.is_none(), |this_bar| {
                                this_bar.child(
                                    Button::new("explore-back")
                                        .xsmall()
                                        .outline()
                                        .label(tr("explore.back"))
                                        .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
                                )
                            })
                            .when(!group_keys.is_empty() || group_key.is_some(), |this_bar| {
                                let label = match &group_key {
                                    Some(key) => trf("canvas.group_by", &[key]),
                                    None => tr("canvas.group").to_string(),
                                };
                                let current = group_key.clone();
                                let keys = group_keys.clone();
                                let menu_target = this.clone();
                                this_bar.child(Button::new("canvas-group").xsmall().outline().label(label).dropdown_menu(
                                    move |menu, _, _| {
                                        let none_target = menu_target.clone();
                                        let mut menu = menu.item(
                                            PopupMenuItem::new(tr("canvas.group_none"))
                                                .checked(current.is_none())
                                                .on_click(move |_, _, cx| {
                                                    none_target.update(cx, |canvas, cx| canvas.set_group_key(None, cx)).ok();
                                                }),
                                        );
                                        if !keys.is_empty() {
                                            menu = menu.separator();
                                        }
                                        for key in &keys {
                                            let target = menu_target.clone();
                                            let picked = key.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(key.clone())
                                                    .checked(current.as_deref() == Some(key.as_str()))
                                                    .on_click(move |_, _, cx| {
                                                        let picked = picked.clone();
                                                        target.update(cx, |canvas, cx| canvas.set_group_key(Some(picked), cx)).ok();
                                                    }),
                                            );
                                        }
                                        menu
                                    },
                                ))
                            })
                            .when(group_key.is_some(), |this_bar| {
                                this_bar
                                    .child(
                                        Button::new("canvas-collapse")
                                            .xsmall()
                                            .outline()
                                            .label(tr("canvas.collapse_groups"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.graph.update(cx, |graph, cx| graph.collapse_all_groups(cx));
                                            })),
                                    )
                                    .child(
                                        Button::new("canvas-expand-groups")
                                            .xsmall()
                                            .outline()
                                            .label(tr("canvas.expand_groups"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.graph.update(cx, |graph, cx| graph.expand_all_groups(cx));
                                            })),
                                    )
                            }),
                    )
                    .child(
                        v_flex()
                            .absolute()
                            .top_2()
                            .right_2()
                            .items_end()
                            .gap_1()
                            .when(self.can_fill_window, |this| {
                                this.child(
                                    Button::new("canvas-maximize")
                                        .xsmall()
                                        .ghost()
                                        .icon(Icon::new(if maximized { AssetIcon::Minimize2 } else { AssetIcon::Maximize2 }))
                                        .tooltip(tr(if maximized { "canvas.restore" } else { "canvas.maximize" }))
                                        .on_click(cx.listener(|this, _, window, cx| this.toggle_maximized(window, cx))),
                                )
                            })
                            .when(!legend.is_empty(), |this| {
                                this.child(
                                    v_flex()
                                        .p_2()
                                        .gap_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(border)
                                        .bg(halo)
                                        .text_xs()
                                        .children(legend.into_iter().map(|(label, color, count)| {
                                            h_flex()
                                                .gap_1p5()
                                                .items_center()
                                                .child(div().size_2p5().rounded_full().bg(color))
                                                .child(div().flex_1().child(label))
                                                .child(div().pl_3().text_color(muted).child(count.to_string()))
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .bottom_2()
                            .left_2()
                            .max_w(relative(0.95))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(halo)
                            .text_xs()
                            .text_color(if walk_warning.is_some() { warning } else { muted })
                            .whitespace_normal()
                            .child(note),
                    ),
            )
            .when(!self.compact || maximized, |this| this.children(self.render_inspector(cx)));

        if !maximized {
            return div().size_full().child(body).into_any_element();
        }
        // Over everything below the title bar, so the window's own buttons
        // stay in reach; the panel keeps a note where the canvas was.
        let viewport = window.viewport_size();
        let theme = cx.theme();
        let (background, border) = (theme.background, theme.border);
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(muted)
            .child(tr("canvas.maximized_note"))
            .child(
                deferred(
                    anchored().position(point(px(0.), TITLE_BAR_HEIGHT)).child(
                        div()
                            .id("graph-canvas-maximized")
                            .track_focus(&self.focus)
                            .occlude()
                            .w(viewport.width)
                            .h(viewport.height - TITLE_BAR_HEIGHT)
                            .bg(background)
                            .border_t_1()
                            .border_color(border)
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    this.toggle_maximized(window, cx);
                                }
                            }))
                            .child(body),
                    ),
                )
                .with_priority(1),
            )
            .into_any_element()
    }
}

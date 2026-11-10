//! The inspector beside the canvas: the selected node or relationship, its
//! evidence, analyses and walks.

use super::*;

impl GraphCanvas {
    /// Where the selected record came from, with what is missing stated.
    pub(super) fn render_evidence(&self, record: Evidence, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let mono = cx.theme().mono_font_family.clone();
        let unknown = tr("evidence.unknown");
        let copy = serde_json::to_string_pretty(&record).expect("evidence serializes");
        v_flex()
            .gap_1()
            .child(div().text_sm().font_semibold().child(tr("evidence.title")))
            .children(explorer::evidence_rows(&record).into_iter().map(|(key, value)| {
                let missing = value == unknown;
                v_flex()
                    .child(div().text_xs().text_color(muted).child(tr(key)))
                    .child(
                        div()
                            .text_xs()
                            .font_family(mono.clone())
                            .whitespace_normal()
                            .when(missing, |this| this.text_color(muted))
                            .child(value),
                    )
            }))
            .when(!record.stable, |this| {
                this.child(div().text_xs().whitespace_normal().text_color(warning).child(tr("evidence.not_stable")))
            })
            .when(!record.redacted.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(warning)
                        .child(trf("evidence.redacted", &[&record.redacted.join(", ")])),
                )
            })
            .child(
                Button::new("evidence-copy")
                    .xsmall()
                    .outline()
                    .label(tr("evidence.copy"))
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
            )
            .into_any_element()
    }

    /// What the analyses that apply say about a selected node, and why. None
    /// apply unless a plugin declared this node's label.
    pub(super) fn render_analyses(&self, id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let node = self.nodes.get(&id)?;
        let sections: Vec<AnyElement> = self
            .explorer
            .analyses
            .iter()
            .enumerate()
            .filter(|(_, offered)| offered.applies_to(&node.labels))
            .map(|(ix, offered)| self.render_analysis(ix, offered, node, id, cx))
            .collect();
        (!sections.is_empty()).then(|| v_flex().gap_2().children(sections).into_any_element())
    }

    /// One analysis on a node. All its words come from the plugin.
    pub(super) fn render_analysis(&self, section: usize, offered: &Offered, node: &GraphNode, id: i64, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let mono = cx.theme().mono_font_family.clone();
        let explorer = &self.explorer;
        let info = &offered.info;
        let key = explorer::analysis_key(offered);
        let report = explorer.reports.get(&key);
        let findings = report.map(|report| explorer::findings_for(report, node));

        let mut list = Vec::new();
        for (ix, finding) in findings.iter().flatten().enumerate() {
            let button = section * 1000 + ix;
            let copy = serde_json::to_string_pretty(finding).expect("a finding serializes");
            let finding_id = finding.finding_id.clone();
            let finding_key = key.clone();
            // Offered again only when another path is showing: Back would otherwise
            // step through the same view twice.
            let showing = self.path_view && explorer.walk.as_ref().is_some_and(|walk| walk.hops == finding.hops);
            let drawable = !finding.hops.is_empty() && !showing;
            let conditions = finding.conditions.iter().map(|condition| {
                let statement = explorer::analysis_text(info, &format!("condition.{}", condition.id), &condition.statement);
                let color = if condition.status == ConditionStatus::Met { muted } else { warning };
                let fields = condition.fields.iter().filter_map(|field| {
                    let value = field.value.known()?;
                    Some(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .child(format!("{} = {}", field.source_field, crate::ui::results::cell_text(value))),
                    )
                });
                v_flex()
                    .child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .child(format!("{} · {statement}", tr(explorer::status_key(condition.status)))),
                    )
                    .child(div().text_xs().whitespace_normal().text_color(color).child(condition.detail.clone()))
                    .children(fields.collect::<Vec<_>>())
            });
            let tone = info.verdicts.iter().find(|verdict| verdict.id == finding.verdict).map(|verdict| verdict.tone);
            let verdict_color = if tone == Some(VerdictTone::Quiet) { muted } else { warning };
            list.push(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .child(div().text_xs().font_semibold().text_color(verdict_color).child(explorer::verdict_text(info, &finding.verdict)))
                    .child(div().text_xs().whitespace_normal().child(explorer::rule_text(info, &finding.rule_id)))
                    .child(div().text_xs().font_family(mono.clone()).whitespace_normal().text_color(muted).child(finding.rule_id.clone()))
                    .children(conditions.collect::<Vec<_>>())
                    .when_some(finding.path.as_ref(), |this, path| {
                        this.child(div().pt_1().text_xs().font_semibold().child(tr("explore.path")))
                            .children(path.hops.iter().map(|hop| div().text_xs().whitespace_normal().font_family(mono.clone()).child(hop.clone())))
                    })
                    .child(div().pt_1().text_xs().font_semibold().child(tr("analysis.assumptions")))
                    .children(finding.assumptions.iter().map(|text| div().text_xs().whitespace_normal().text_color(muted).child(text.clone())))
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .when(drawable, |this| {
                                this.child(
                                    Button::new(("analysis-path", button))
                                        .xsmall()
                                        .outline()
                                        .label(tr("analysis.show_path"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.show_finding(&finding_key, &finding_id, id, cx))),
                                )
                            })
                            .child(
                                Button::new(("analysis-copy", button))
                                    .xsmall()
                                    .outline()
                                    .label(tr("analysis.copy"))
                                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                            ),
                    ),
            );
        }

        let checked = report.is_some();
        let running = explorer.checking.contains_key(&key);
        let complete = report.is_none_or(|report| report.complete);
        let none = findings.as_ref().is_some_and(Vec::is_empty);
        let claim = explorer::analysis_text(info, "claim", &info.claim);
        let label = if running {
            tr("analysis.stop")
        } else if checked {
            tr("analysis.recheck")
        } else {
            tr("analysis.check")
        };
        let run = offered.clone();
        v_flex()
            .gap_1()
            .child(div().text_sm().font_semibold().child(explorer::analysis_text(info, "title", &info.title)))
            .child(
                Button::new(("analysis-check", section))
                    .xsmall()
                    .outline()
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| this.check_analysis(run.clone(), window, cx))),
            )
            .when_some(explorer.analysis_errors.get(&key).cloned(), |this, error| {
                this.child(div().text_xs().whitespace_normal().text_color(warning).child(trf("analysis.failed", &[&error])))
            })
            .when(checked && !claim.is_empty(), |this| this.child(div().text_xs().whitespace_normal().text_color(muted).child(claim)))
            .when(!complete, |this| this.child(div().text_xs().whitespace_normal().text_color(warning).child(tr("analysis.incomplete"))))
            .when(none, |this| this.child(div().text_xs().whitespace_normal().text_color(muted).child(tr("analysis.none"))))
            .children(list)
            .into_any_element()
    }

    pub(super) fn selected_evidence(&self, selected: Selected) -> Option<Evidence> {
        match selected {
            Selected::Node(id) => self.nodes.get(&id).map(|node| evidence::for_node(node, &self.sources)),
            Selected::Edge(id) => {
                let edge = self.edges.get(&id)?;
                let ends: Vec<GraphNode> = [edge.source, edge.target].iter().filter_map(|id| self.nodes.get(id).cloned()).collect();
                Some(evidence::for_edge(edge, &ends, &self.sources))
            }
        }
    }

    pub(super) fn render_explorer(&self, node: i64, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let border = cx.theme().border;
        let mono = cx.theme().mono_font_family.clone();
        let explorer = &self.explorer;
        let running = explorer.running.is_some();
        let target = cx.entity().downgrade();
        let current = explorer.direction;
        let way = |id: &'static str, label: &'static str, direction: Direction, cx: &mut Context<Self>| {
            let button = Button::new(id).xsmall().label(tr(label));
            let button = if current == direction { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, _, cx| {
                this.explorer.direction = direction;
                cx.notify();
            }))
        };
        let types_label = match explorer.types.len() {
            0 => tr("explore.types_all").to_string(),
            count => trf("explore.types_some", &[&count.to_string()]),
        };
        let (available, chosen) = (explorer.available.clone(), explorer.types.clone());
        let is_start = explorer.starts.contains(&node);
        let path = explorer.walk.as_ref().map(|walk| explorer::explained_path(walk, node)).unwrap_or_default();
        let path_evidence = explorer
            .walk
            .as_ref()
            .and_then(|walk| evidence::for_path(walk, node, &self.sources))
            .map(|record| serde_json::to_string_pretty(&record).expect("evidence serializes"));

        v_flex()
            .pt_2()
            .mt_1()
            .gap_1p5()
            .border_t_1()
            .border_color(border)
            .child(div().text_sm().font_semibold().child(tr("explore.title")))
            .child(
                h_flex()
                    .gap_1()
                    .child(way("explore-out", "explore.out", Direction::Out, cx))
                    .child(way("explore-in", "explore.in", Direction::In, cx))
                    .child(way("explore-both", "explore.both", Direction::Both, cx)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("explore-fewer")
                            .xsmall()
                            .outline()
                            .icon(IconName::Minus)
                            .tooltip(tr("explore.fewer_hops"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.explorer.change_hops(-1);
                                cx.notify();
                            })),
                    )
                    .child(div().text_xs().child(trf("explore.hops", &[&explorer.max_hops.to_string()])))
                    .child(
                        Button::new("explore-more")
                            .xsmall()
                            .outline()
                            .icon(IconName::Plus)
                            .tooltip(tr("explore.more_hops"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.explorer.change_hops(1);
                                cx.notify();
                            })),
                    ),
            )
            .child(Button::new("explore-types").xsmall().outline().label(types_label).dropdown_menu(move |mut menu, _, _| {
                for name in &available {
                    let (target, picked) = (target.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(chosen.contains(name)).on_click(move |_, _, cx| {
                        target
                            .update(cx, |canvas, cx| {
                                canvas.explorer.toggle_type(&picked);
                                cx.notify();
                            })
                            .ok();
                    }));
                }
                menu
            }))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("explore-start")
                            .xsmall()
                            .outline()
                            .label(tr(if is_start { "explore.remove_start" } else { "explore.add_start" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.explorer.toggle_start(node);
                                cx.notify();
                            })),
                    )
                    .when(!explorer.starts.is_empty(), |this| {
                        this.child(div().text_xs().text_color(muted).child(trf("explore.starts", &[&explorer.starts.len().to_string()])))
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("explore-run")
                            .xsmall()
                            .primary()
                            .label(tr("explore.run"))
                            .loading(running)
                            .on_click(cx.listener(|this, _, window, cx| this.explore(window, cx))),
                    )
                    .child(
                        Button::new("investigation-save")
                            .xsmall()
                            .outline()
                            .label(tr("investigation.save"))
                            .on_click(cx.listener(move |this, _, window, cx| this.save_investigation(node, window, cx))),
                    )
                    .when_some(explorer.running.clone(), |this, cancel| {
                        this.child(
                            Button::new("explore-cancel")
                                .xsmall()
                                .outline()
                                .label(tr("explore.cancel"))
                                .on_click(move |_, _, _| cancel.store(true, Ordering::Relaxed)),
                        )
                    }),
            )
            .children(explorer.status_lines().into_iter().map(|(line, warn)| {
                div().text_xs().whitespace_normal().text_color(if warn { warning } else { muted }).child(line)
            }))
            .when(!path.is_empty(), |this| {
                this.child(div().pt_1().text_xs().font_semibold().child(tr("explore.path")))
                    .children(path.into_iter().map(|hop| {
                        div().text_xs().whitespace_normal().font_family(mono.clone()).child(hop)
                    }))
                    .child(div().text_xs().whitespace_normal().text_color(muted).child(tr("explore.candidate")))
                    .when_some(path_evidence, |this, copy| {
                        this.child(
                            Button::new("evidence-copy-path")
                                .xsmall()
                                .outline()
                                .label(tr("evidence.copy_path"))
                                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                        )
                    })
                    .when(!self.path_view, |this| {
                        this.child(
                            Button::new("explore-show-path")
                                .xsmall()
                                .outline()
                                .label(tr("explore.show_path"))
                                .on_click(cx.listener(move |this, _, _, cx| this.show_path(node, cx))),
                        )
                    })
            })
            .into_any_element()
    }

    pub(super) fn render_inspector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.selected?;
        let (title, color, subtitle, properties) = match selected {
            Selected::Node(id) => {
                let node = self.nodes.get(&id)?;
                let label = node.labels.join(":");
                (
                    format!("{label} #{id}"),
                    Some(palette::label_color(first_label(node), cx)),
                    None,
                    &node.properties,
                )
            }
            Selected::Edge(id) => {
                let edge = self.edges.get(&id)?;
                (
                    format!("→ {} #{id}", edge.rel_type),
                    None,
                    Some(format!("#{} → #{}", edge.source, edge.target)),
                    &edge.properties,
                )
            }
        };
        // The evidence above says what it masked, so the list must not show it.
        let properties = khonsu_core::evidence::masked_properties(properties);
        let mut rows: Vec<(&String, &Json)> = properties.iter().collect();
        // Provenance last: it says where a value came from, not what it is.
        rows.sort_by_key(|(k, _)| (k.starts_with("khonsu_"), k.to_string()));
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.theme().muted_foreground;
        let analyses = self.selected_node().and_then(|id| self.render_analyses(id, cx));
        Some(
            v_flex()
                .id("inspector")
                .w(crate::ui::scale::design(280.))
                .h_full()
                .flex_none()
                .overflow_y_scroll()
                .border_l_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .p_3()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .children(color.map(|c| div().size_3().rounded_full().bg(c)))
                        .child(div().font_semibold().child(title)),
                )
                .children(subtitle.map(|s| div().text_xs().text_color(muted).font_family(mono.clone()).child(s)))
                // Above the properties: a node can have enough of them to push
                // a walk's result out of sight.
                .when_some(self.selected_node(), |this, id| {
                    this.child(self.render_explorer(id, cx)).child(div().border_b_1().border_color(cx.theme().border))
                })
                .when_some(analyses, |this, analyses| {
                    this.child(analyses).child(div().border_b_1().border_color(cx.theme().border))
                })
                .when_some(self.selected_evidence(selected), |this, record| {
                    this.child(self.render_evidence(record, cx)).child(div().border_b_1().border_color(cx.theme().border))
                })
                .children(rows.into_iter().map(|(key, value)| {
                    let provenance = key.starts_with("khonsu_");
                    v_flex()
                        .when(provenance, |this| this.opacity(0.6))
                        .child(div().text_xs().text_color(muted).font_family(mono.clone()).child(key.clone()))
                        .child(
                            div()
                                .text_sm()
                                .font_family(mono.clone())
                                .whitespace_normal()
                                .child(crate::ui::results::cell_text(value)),
                        )
                }))
                .when_some(
                    match selected {
                        Selected::Node(id) => Some(id),
                        Selected::Edge(_) => None,
                    },
                    |this, id| {
                        this.child(
                            Button::new("expand-node")
                                .small()
                                .outline()
                                .icon(IconName::Plus)
                                .tooltip(tr("canvas.expand"))
                                .loading(self.expanding)
                                .on_click(cx.listener(move |this, _, window, cx| this.expand(id, window, cx))),
                        )
                    },
                )
                .into_any_element(),
        )
    }
}

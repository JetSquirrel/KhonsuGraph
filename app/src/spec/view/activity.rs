//! Agents at work on the dashboard, as drawn: name tags, change marks, the
//! plots being added or removed, and the activity log.

use super::*;

impl Dashboard {
    /// An agent's command said what it is doing to this dashboard.
    pub fn apply_presence(&mut self, update: &presence::Update, cx: &mut Context<Self>) {
        self.presence.apply(update, Instant::now());
        self.keep_presence_fresh(cx);
        cx.notify();
    }

    /// Whether an agent is at work on this dashboard, for the tab strip.
    pub fn has_presence(&self) -> bool {
        self.presence.is_working()
    }

    /// Threads an agent answered that are still open: a question back to the
    /// person, or a request it could not carry out.
    pub(super) fn waiting_threads(&self) -> usize {
        self.comments
            .threads
            .iter()
            .filter(|thread| {
                !thread.resolved
                    && thread
                        .comments
                        .last()
                        .is_some_and(|comment| comment.author == comments::Author::Agent)
            })
            .count()
    }

    /// End quiet sessions and count the changed plots' time in view, for as
    /// long as there is anything to show.
    pub(super) fn keep_presence_fresh(&mut self, cx: &mut Context<Self>) {
        if self.presence_ticking || self.presence.is_empty() {
            return;
        }
        self.presence_ticking = true;
        self._presence_tick = Some(cx.spawn(async move |this, cx| loop {
            smol::Timer::after(PRESENCE_TICK).await;
            let more = this.update(cx, |this, cx| {
                let waiting = this.waiting_threads();
                let mut moved = this.presence.tick(Instant::now(), waiting);
                if this.presence.has_changes() {
                    let seen = this.plots_in_view();
                    moved |= this.presence.viewed(&seen, PRESENCE_TICK);
                    // A frame each tick while marks wait to be seen: what is
                    // in view is read off the last one painted.
                    cx.notify();
                }
                if moved {
                    cx.notify();
                }
                this.presence_ticking = !this.presence.is_empty();
                this.presence_ticking
            });
            if !matches!(more, Ok(true)) {
                break;
            }
        }));
    }

    /// The changed plots the person can see now: drawn in the last moment,
    /// in a focused window, and fully inside the scrolled view — or, for a
    /// plot taller than the view, filling it.
    pub(super) fn plots_in_view(&self) -> Vec<String> {
        let fresh = self
            .painted
            .get()
            .is_some_and(|at| at.elapsed() < PRESENCE_TICK * 2);
        if !fresh || !self.window_active.get() || self.showing_source {
            return Vec::new();
        }
        let Some(view) = self.wheel.viewport().get() else {
            return Vec::new();
        };
        self.card_bounds
            .iter()
            .filter(|(name, _)| self.presence.change(name).is_some())
            .filter(|(_, cell)| {
                cell.get().is_some_and(|card| {
                    let top = card.top().max(view.top());
                    let bottom = card.bottom().min(view.bottom());
                    let shown = bottom - top;
                    shown >= card.size.height.min(view.size.height) * 0.95
                })
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The spec would not read, parse or validate: the whole tab is the error,
    /// the way an app that has never loaded shows its failure.
    /// An agent's mark on plot `ix`: a ring in the agent colour over the card
    /// — breathing while it says it is about to edit the plot, steady once
    /// its edit has changed or added it, until the person has seen it.
    pub(super) fn render_plot_presence(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        let color = crate::ui::palette::presence(cx);
        let ring = div()
            .absolute()
            .inset_0()
            .rounded(px(PANEL_RADIUS))
            .border_2()
            .border_color(color);
        if self.presence.change(name).is_some() {
            return Some(ring.bg(color.opacity(0.05)).into_any_element());
        }
        self.presence.editing(name)?;
        Some(
            ring.with_animation(
                SharedString::from(format!("presence-ring-{}-{name}", self.id)),
                pulse(true),
                |ring, delta| ring.opacity(delta),
            )
            .into_any_element(),
        )
    }

    /// The name tag of an agent's mark: on the card's top edge, half over the
    /// gap above it, so it never covers the plot's own title.
    pub(super) fn render_presence_tag(&self, ix: usize, cx: &App) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        let (text, ongoing) = match self.presence.change(name) {
            Some(mark) => {
                let key = match mark.change {
                    presence::Change::Changed => "presence.changed",
                    presence::Change::Added => "presence.added",
                };
                (trf(key, &[&mark.agent]), false)
            }
            None => (
                trf("presence.editing", &[self.presence.editing(name)?]),
                true,
            ),
        };
        Some(
            presence_tag(crate::ui::palette::presence(cx))
                .absolute()
                .top(px(-PRESENCE_TAG_RISE))
                .left(px(PAGE_GAP / 2. + 10.))
                .child(presence_dot(
                    format!("presence-dot-{}-{name}-{ongoing}", self.id).into(),
                    ongoing,
                ))
                .child(text)
                .into_any_element(),
        )
    }

    /// Records where a marked plot is drawn, for `plots_in_view`.
    pub(super) fn record_card_bounds(&mut self, ix: usize) -> Option<AnyElement> {
        let name = &self.plots[ix].name;
        self.presence.change(name)?;
        let cell = self.card_bounds.entry(name.clone()).or_default().clone();
        Some(
            canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
                .absolute()
                .inset_0()
                .into_any_element(),
        )
    }

    /// The plots an agent said it is about to add, held a place for below
    /// the last row until its edit lands.
    pub(super) fn render_adding(&self, cx: &App) -> Option<AnyElement> {
        let names: Vec<String> = self.plots.iter().map(|plot| plot.name.clone()).collect();
        let adding = self.presence.adding(&names);
        if adding.is_empty() {
            return None;
        }
        let color = crate::ui::palette::presence(cx);
        Some(
            v_flex()
                .gap(px(PAGE_GAP))
                .px(px(PAGE_GAP))
                .pb(px(PAGE_GAP))
                .children(adding.into_iter().map(|(plot, agent)| {
                    h_flex()
                        .h(px(ADDING_HEIGHT))
                        .justify_center()
                        .items_center()
                        .gap_1p5()
                        .rounded(px(PANEL_RADIUS))
                        .border_2()
                        .border_color(color.opacity(0.6))
                        .bg(color.opacity(0.04))
                        .text_sm()
                        .text_color(color)
                        .child(presence_dot_in(
                            format!("presence-adding-{}-{plot}", self.id).into(),
                            color,
                            true,
                        ))
                        .child(trf("presence.adding", &[&agent, &plot]))
                }))
                .into_any_element(),
        )
    }

    /// Plots an agent's edit took out: one line above the dashboard until the
    /// person dismisses it.
    pub(super) fn render_removed(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let removed = self.presence.removed();
        if removed.is_empty() {
            return None;
        }
        let color = crate::ui::palette::presence(cx);
        let mut lines: Vec<String> = Vec::new();
        for agent in removed.iter().map(|r| r.agent.as_str()) {
            if lines.iter().any(|line| line.starts_with(agent)) {
                continue;
            }
            let titles: Vec<String> = removed
                .iter()
                .filter(|r| r.agent == agent)
                .map(|r| format!("\u{201c}{}\u{201d}", r.title))
                .collect();
            lines.push(trf(
                "presence.removed",
                &[agent, &titles.join(tr("presence.list_separator"))],
            ));
        }
        Some(
            h_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(color.opacity(0.06))
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .children(lines.into_iter().map(|line| div().child(line))),
                )
                .child(
                    Button::new(("dashboard-removed-dismiss", self.id as usize))
                        .xsmall()
                        .ghost()
                        .label(tr("presence.dismiss"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presence.dismiss_removed();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Who is at work on the dashboard: one name each in the top corner,
    /// breathing while it works, steady once done, red while the dashboard
    /// shows an error. A click opens what it did.
    pub(super) fn render_presence_names(&self, cx: &App) -> Option<AnyElement> {
        let sessions = self.presence.sessions();
        if sessions.is_empty() {
            return None;
        }
        let failing =
            self.spec_error.is_some() || self.plots.iter().any(|plot| plot.failure.is_some());
        let waiting = self.waiting_threads();
        let titles: HashMap<String, String> = self
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
        let last = sessions.len() - 1;
        let pills = sessions.iter().enumerate().map(|(ix, session)| {
            let working = session.is_working();
            let color = if failing {
                cx.theme().danger
            } else if working {
                crate::ui::palette::presence(cx)
            } else {
                cx.theme().success
            };
            let state = if failing {
                tr("presence.has_errors")
            } else if working {
                tr("presence.working")
            } else {
                tr("presence.done")
            };
            let lines: Vec<(String, String)> = session
                .log
                .iter()
                .map(|entry| {
                    (
                        entry.at.format("%H:%M").to_string(),
                        log_line(&entry.did, &titles),
                    )
                })
                .collect();
            let header = trf("presence.session", &[&session.agent]);
            let shows_waiting = ix == last && waiting > 0;
            let trigger = Button::new(SharedString::from(format!(
                "presence-name-{}-{}",
                self.id, session.agent
            )))
            .xsmall()
            .outline()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(crate::ui::agent_avatar::agent_avatar(
                        &session.agent,
                        px(16.),
                        cx,
                    ))
                    .child(session.agent.clone())
                    .child(presence_dot_in(
                        format!("presence-name-dot-{}-{}-{working}", self.id, session.agent).into(),
                        color,
                        working && !failing,
                    )),
            );
            let popover = Popover::new(SharedString::from(format!(
                "presence-log-{}-{}",
                self.id, session.agent
            )))
            .anchor(Anchor::TopRight)
            .trigger(trigger)
            .content(move |_, _, cx| {
                v_flex()
                    .w(px(280.))
                    .gap_1()
                    .text_sm()
                    .child(
                        h_flex()
                            .justify_between()
                            .font_weight(FontWeight::MEDIUM)
                            .child(header.clone())
                            .child(div().text_color(color).child(state)),
                    )
                    .children(lines.iter().map(|(at, text)| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(40.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(at.clone()),
                            )
                            .child(div().min_w_0().child(text.clone()))
                    }))
                    .when(shows_waiting, |this| {
                        this.child(
                            div()
                                .pt_1()
                                .text_color(cx.theme().muted_foreground)
                                .child(trf("presence.waiting", &[&waiting.to_string()])),
                        )
                    })
            });
            div().relative().child(popover).when(shows_waiting, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-5.))
                        .right(px(-5.))
                        .min_w(px(15.))
                        .h(px(15.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(crate::ui::palette::presence(cx))
                        .text_color(gpui_kit::white())
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(waiting.to_string()),
                )
            })
        });
        Some(
            h_flex()
                .absolute()
                .top_2()
                .right_3()
                .gap_2()
                .children(pills)
                .into_any_element(),
        )
    }
}

/// How far a presence name tag rises above its card's top edge, into the
/// gap between rows.
pub(super) const PRESENCE_TAG_RISE: f32 = 8.;

/// How often presence is brought up to date: quiet sessions ended, and the
/// changed plots' time in view counted.
pub(super) const PRESENCE_TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// The height of the place held for a plot an agent is about to add.
pub(super) const ADDING_HEIGHT: f32 = 56.;

/// A presence name tag's pill, in the agent colour.
pub(super) fn presence_tag(color: Hsla) -> Div {
    h_flex()
        .gap_1()
        .items_center()
        .px_1p5()
        .rounded(px(4.))
        .bg(color)
        .text_color(gpui_kit::white())
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
}

/// One line of a session's log, plots by their titles.
pub(super) fn log_line(did: &presence::Did, titles: &HashMap<String, String>) -> String {
    let named = |plots: &[String]| {
        plots
            .iter()
            .map(|plot| format!("\u{201c}{}\u{201d}", titles.get(plot).unwrap_or(plot)))
            .collect::<Vec<_>>()
            .join(tr("presence.list_separator"))
    };
    match did {
        presence::Did::Read(plots) => trf("presence.log.read", &[&named(plots)]),
        presence::Did::Checked => tr("presence.log.checked").to_string(),
        presence::Did::Replied(plots) => trf("presence.log.replied", &[&named(plots)]),
        presence::Did::Edited {
            changed,
            added,
            removed,
        } => {
            let parts = [
                ("presence.log.changed", *changed),
                ("presence.log.added", *added),
                ("presence.log.removed", *removed),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(key, count)| trf(key, &[&count.to_string()]))
            .collect::<Vec<_>>()
            .join(tr("presence.list_separator"));
            // The parts read as one sentence, which starts with a capital.
            let mut chars = parts.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A dot in `color`: breathing while an agent is at work, steady after.
pub(super) fn presence_dot_in(id: SharedString, color: Hsla, ongoing: bool) -> impl IntoElement {
    div()
        .size_2()
        .rounded_full()
        .bg(color)
        .with_animation(id, pulse(ongoing), |dot, delta| dot.opacity(delta))
}

/// A presence mark's dot: breathing while the work goes on, steady once done.
pub(super) fn presence_dot(id: SharedString, ongoing: bool) -> impl IntoElement {
    div()
        .size_1p5()
        .rounded_full()
        .bg(gpui_kit::white())
        .with_animation(id, pulse(ongoing), |dot, delta| dot.opacity(delta))
}

/// Opacity between faint and full and back, a breath every 1.2s; or full,
/// held, for a mark of work already done.
pub(super) fn pulse(ongoing: bool) -> Animation {
    if ongoing {
        Animation::new(std::time::Duration::from_millis(1200))
            .repeat()
            .with_easing(|t| 0.35 + 0.65 * (0.5 - 0.5 * (t * std::f32::consts::TAU).cos()))
    } else {
        Animation::new(std::time::Duration::from_millis(1)).with_easing(|_| 1.)
    }
}

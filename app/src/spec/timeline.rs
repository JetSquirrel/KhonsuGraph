//! A dashboard `timeline`: one event per row, placed along its time so the
//! gaps between steps show, in a swim lane per value of `series` (a
//! client, a service, a vendor). Hovering an event names it, its time,
//! how long after the event before it in its lane, and its tooltip columns.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::trf;
use crate::query::QueryResult;

/// One row of the query, placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub lane: usize,
    /// Seconds on the timeline's clock: Unix time for a time with an offset,
    /// the clock as written for one without.
    pub at: f64,
    /// The time as the query returned it.
    pub when: String,
    pub caption: String,
    pub tooltip: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub lanes: Vec<String>,
    /// Ordered by time.
    pub events: Vec<Event>,
    /// Rows whose time could not be read.
    pub skipped: usize,
}

impl Timeline {
    pub fn span(&self) -> Option<(f64, f64)> {
        Some((self.events.first()?.at, self.events.last()?.at))
    }
}

/// Seconds for `text`: RFC 3339 or `YYYY-MM-DD HH:MM[:SS[.fff]]` with or
/// without an offset, or a bare date.
pub fn seconds(text: &str) -> Option<f64> {
    let text = text.trim();
    let spaced = text.replacen(' ', "T", 1);
    if let Ok(time) = DateTime::parse_from_rfc3339(&spaced) {
        return Some(time.timestamp_millis() as f64 / 1000.);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M%z"] {
        if let Ok(time) = DateTime::parse_from_str(&spaced, format) {
            return Some(time.timestamp_millis() as f64 / 1000.);
        }
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(&spaced, format) {
            return Some(time.and_utc().timestamp_millis() as f64 / 1000.);
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp() as f64)
}

/// The timeline `result` draws: `time`, `lane`, `caption` and `tooltip` are
/// column indexes. Lanes keep the order they first appear in.
pub fn build(result: &QueryResult, time: usize, lane: Option<usize>, caption: Option<usize>, tooltip: &[usize]) -> Timeline {
    let mut timeline = Timeline::default();
    for row in &result.rows {
        let cell = |ix: usize| row.get(ix).cloned().unwrap_or_default();
        let Some(at) = row.get(time).and_then(|cell| seconds(cell)) else {
            timeline.skipped += 1;
            continue;
        };
        let lane_name = lane.map(cell).unwrap_or_default();
        let lane = match timeline.lanes.iter().position(|l| *l == lane_name) {
            Some(ix) => ix,
            None => {
                timeline.lanes.push(lane_name);
                timeline.lanes.len() - 1
            }
        };
        timeline.events.push(Event {
            lane,
            at,
            when: cell(time),
            caption: caption.map(cell).unwrap_or_default(),
            tooltip: tooltip.iter().map(|&ix| (result.columns[ix].name.clone(), cell(ix))).collect(),
        });
    }
    timeline.events.sort_by(|a, b| a.at.total_cmp(&b.at));
    timeline
}

/// `seconds` as the gap it is: `45s`, `12m`, `3h 20m`, `2d 4h`.
pub fn gap(seconds: f64) -> String {
    let s = seconds.abs().round() as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 if s % 3600 / 60 == 0 => format!("{}h", s / 3600),
        3600..86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ if s % 86_400 / 3600 == 0 => format!("{}d", s / 86_400),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// Width of the lane names, left of the track.
const LANE_GUTTER: f32 = 112.;
const LANE_HEIGHT: f32 = 44.;
const DOT: f32 = 9.;
/// An event's caption is drawn when it starts at least this share of the
/// track after the last caption drawn in its lane; the rest are on hover.
const CAPTION_ROOM: f64 = 0.14;
/// Times written under the track.
const AXIS_TICKS: usize = 5;

/// Where `at` falls on the track, 0 to 1. A single moment sits in the middle.
fn place(at: f64, (start, end): (f64, f64)) -> f32 {
    if end > start {
        ((at - start) / (end - start)) as f32
    } else {
        0.5
    }
}

pub fn render(name: &str, timeline: &Arc<Timeline>, cx: &App) -> AnyElement {
    let Some(span) = timeline.span() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let lanes = timeline.lanes.len().max(1);
    let mut rows = Vec::with_capacity(lanes);
    for (lane_ix, lane_name) in timeline.lanes.iter().enumerate() {
        let color = crate::ui::palette::color(lane_ix, cx);
        let mut marks: Vec<AnyElement> = Vec::new();
        let mut last_caption: Option<f32> = None;
        let mut previous: Option<f64> = None;
        for (event_ix, event) in timeline.events.iter().enumerate().filter(|(_, e)| e.lane == lane_ix) {
            let x = place(event.at, span);
            let shows_caption = !event.caption.is_empty() && last_caption.is_none_or(|last| (x - last) as f64 >= CAPTION_ROOM);
            if shows_caption {
                last_caption = Some(x);
            }
            let mut lines = Vec::new();
            if !event.caption.is_empty() {
                lines.push(event.caption.clone());
            }
            lines.push(event.when.clone());
            if let Some(before) = previous {
                lines.push(trf("timeline.after", &[&gap(event.at - before)]));
            }
            lines.extend(event.tooltip.iter().map(|(k, v)| format!("{k}: {v}")));
            previous = Some(event.at);
            let text = lines.join("\n");
            marks.push(
                div()
                    .id(SharedString::from(format!("timeline-{name}-{event_ix}")))
                    .absolute()
                    .left(relative(x))
                    .top(px(LANE_HEIGHT / 2. - DOT / 2.))
                    .ml(px(-DOT / 2.))
                    .size(px(DOT))
                    .rounded_full()
                    .bg(color)
                    .border_1()
                    .border_color(theme.background)
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx))
                    .into_any_element(),
            );
            if shows_caption {
                // Right of the dot, unless the dot is near the end.
                let caption = div()
                    .absolute()
                    .top(px(LANE_HEIGHT / 2. + DOT / 2. + 1.))
                    .max_w(px(160.))
                    .text_xs()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(event.caption.clone());
                marks.push(
                    if x > 0.85 {
                        caption.right(relative(1. - x)).into_any_element()
                    } else {
                        caption.left(relative(x)).into_any_element()
                    },
                );
            }
        }
        rows.push(
            h_flex()
                .h(px(LANE_HEIGHT))
                .border_b_1()
                .border_color(border.opacity(0.5))
                .child(
                    div()
                        .w(px(LANE_GUTTER))
                        .flex_none()
                        .pr_2()
                        .text_xs()
                        .text_color(muted)
                        .text_ellipsis()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(lane_name.clone()),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .mx_3()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(LANE_HEIGHT / 2.))
                                .h(px(1.))
                                .bg(border),
                        )
                        .children(marks),
                )
                .into_any_element(),
        );
    }
    let (start, end) = span;
    let ticks = if end > start { AXIS_TICKS } else { 1 };
    let axis = (0..ticks).map(|i| {
        let share = if ticks == 1 { 0.5 } else { i as f32 / (ticks - 1) as f32 };
        let at = start + (end - start) * share as f64;
        let label = timeline
            .events
            .iter()
            .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))
            .filter(|_| i == 0 || i + 1 == ticks)
            .map(|event| event.when.clone())
            .unwrap_or_else(|| trf("timeline.offset", &[&gap(at - start)]));
        let tick = div().absolute().top_0().text_xs().text_color(muted).whitespace_nowrap().child(label);
        match i {
            0 => tick.left_0(),
            _ if i + 1 == ticks => tick.right_0(),
            _ => tick.left(relative(share)).ml(px(-20.)),
        }
    });
    v_flex()
        .size_full()
        .px_2()
        .pb_2()
        .child(v_flex().flex_1().justify_center().children(rows))
        .child(
            h_flex().h(px(18.)).child(div().w(px(LANE_GUTTER)).flex_none()).child(
                div().relative().flex_1().h_full().mx_3().children(axis),
            ),
        )
        .when(timeline.skipped > 0, |this| {
            this.child(div().text_xs().text_color(muted).child(trf("timeline.skipped", &[&timeline.skipped.to_string()])))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::{build, gap, seconds};
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};

    #[test]
    fn times_read_with_or_without_an_offset() {
        assert_eq!(seconds("1970-01-01T02:15:00Z"), Some(8_100.));
        assert_eq!(seconds("1970-01-01 10:15:00+08:00"), Some(8_100.));
        assert_eq!(seconds("1970-01-01 02:15"), Some(8_100.));
        assert_eq!(seconds("1970-01-01"), Some(0.));
        assert_eq!(seconds("soon"), None);
    }

    #[test]
    fn events_are_ordered_in_lanes_as_first_seen() {
        let column = |name: &str| ColumnMeta { name: name.into(), kind: ColumnKind::Text };
        let row = |at: &str, who: &str, what: &str| vec![at.to_string(), who.to_string(), what.to_string()];
        let result = QueryResult {
            columns: vec![column("at"), column("who"), column("what")],
            rows: vec![
                row("1970-01-01 02:20", "vendor", "callback"),
                row("1970-01-01 02:00", "client", "request"),
                row("not a time", "client", "?"),
                row("1970-01-01 02:05", "client", "retry"),
            ],
            truncated: false,
            graph: None,
        };
        let timeline = build(&result, 0, Some(1), Some(2), &[]);
        assert_eq!(timeline.lanes, ["vendor", "client"]);
        assert_eq!(timeline.skipped, 1);
        let order: Vec<&str> = timeline.events.iter().map(|e| e.caption.as_str()).collect();
        assert_eq!(order, ["request", "retry", "callback"]);
        assert_eq!(timeline.span().map(|(s, e)| e - s), Some(1200.));
    }

    #[test]
    fn gaps_read_as_people_say_them() {
        assert_eq!(gap(45.), "45s");
        assert_eq!(gap(720.), "12m");
        assert_eq!(gap(7200.), "2h");
        assert_eq!(gap(12_000.), "3h 20m");
        assert_eq!(gap(187_200.), "2d 4h");
    }
}

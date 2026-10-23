//! What the dashboard's charts share: pie slices folded past a handful,
//! legends, tick and value formatting, and the notes under a map. The plots
//! themselves are in `crate::spec::plot` and `crate::ui::geo`.

use std::sync::Arc;

use gpui_kit::component::chart::PieChart;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::h_flex;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::spec::prepare::PlotPoint;
use crate::ui::geo::{GeoData, MAX_GEO_POINTS};

/// One pie slice: a band's first-series value, or the fold of the smallest.
#[derive(Debug)]
pub struct PieSlice {
    name: SharedString,
    value: f32,
    /// Palette slot; `None` for the folded "other" slice.
    slot: Option<usize>,
}

impl PieSlice {
    #[cfg(test)]
    pub(crate) fn value(&self) -> f32 {
        self.value
    }
}

/// A chart is at most a couple of thousand pixels wide, so plotting more
/// points than this cannot show more detail — it only multiplies the
/// primitives the renderer has to push. A wide `PIVOT` over a fortnight of
/// per-minute metrics reaches millions of plotted values, which is enough to
/// wedge the GPU renderer. Dashboards (`spec::prepare`) cap at this too.
pub(crate) const MAX_POINTS: usize = 1_500;
/// The palette holds eight colors; past a dozen lines a chart is unreadable
/// whatever the cost of drawing it.
pub(crate) const MAX_SERIES: usize = 12;
/// Bars are wider than line vertices, and unlike a time axis, adjacent bands
/// cannot be meaningfully merged — so surplus bars are dropped, not averaged.
pub(crate) const MAX_BARS: usize = 200;
/// Past this many slices a pie is unreadable; the smallest fold into "other".
const MAX_SLICES: usize = 8;

/// The first series as pie slices, largest first, the smallest folded into
/// "other" past `MAX_SLICES`; plus how many rows had no positive value.
pub(crate) fn pie_slices(rows: &[Arc<PlotPoint>]) -> (Vec<Arc<PieSlice>>, usize) {
    let mut positive: Vec<(SharedString, f64)> = rows
        .iter()
        .filter(|r| r.values[0] > 0.)
        .map(|r| (r.band.clone(), r.values[0]))
        .collect();
    let skipped = rows.len() - positive.len();
    positive.sort_by(|a, b| b.1.total_cmp(&a.1));
    let folded = positive.len() > MAX_SLICES;
    let kept = if folded {
        MAX_SLICES - 1
    } else {
        positive.len()
    };
    let rest: f64 = positive[kept..].iter().map(|(_, v)| v).sum();
    let mut slices: Vec<Arc<PieSlice>> = positive
        .into_iter()
        .take(kept)
        .enumerate()
        .map(|(slot, (name, value))| {
            Arc::new(PieSlice {
                name,
                value: value as f32,
                slot: Some(slot),
            })
        })
        .collect();
    if folded {
        slices.push(Arc::new(PieSlice {
            name: tr("chart.map.other").into(),
            value: rest as f32,
            slot: None,
        }));
    }
    (slices, skipped)
}

/// Parse a rendered cell back to a number, tolerating thousands separators.
/// Avoids allocating unless the cell actually contains one.
pub(crate) fn parse_number(cell: &str) -> Option<f64> {
    if cell.contains(',') {
        cell.replace(',', "").parse().ok()
    } else {
        cell.parse().ok()
    }
}

/// What a map says under its title: capping, dropped rows, the color
/// column; and its legend. Shared with dashboards.
pub(crate) fn map_notes(geo: &GeoData, cx: &App) -> (Option<String>, Vec<(Hsla, SharedString)>) {
    let mut notices = Vec::new();
    if let Some(total) = geo.capped_from {
        notices.push(trf(
            "chart.map.notice.capped",
            &[&MAX_GEO_POINTS.to_string(), &total.to_string()],
        ));
    }
    if geo.dropped > 0 {
        notices.push(trf("chart.map.notice.dropped", &[&geo.dropped.to_string()]));
    }
    if geo.outside > 0 {
        notices.push(trf("chart.map.notice.outside", &[&geo.outside.to_string()]));
    }
    if let Some(name) = &geo.category_name {
        notices.push(trf("chart.map.colored_by", &[name]));
    }
    if let Some(name) = &geo.size_name {
        notices.push(trf("chart.map.sized_by", &[name]));
    }
    if geo.size_missing > 0 {
        notices.push(trf(
            "chart.map.notice.unsized",
            &[&geo.size_missing.to_string()],
        ));
    }
    let legend = geo
        .categories
        .iter()
        .enumerate()
        .map(|(slot, name)| (geo.category_color(slot, cx), name.clone()))
        .collect();
    (crate::i18n::join_all(&notices), legend)
}

/// A key of colored dots and names, wrapping as wide as it is given.
pub(crate) fn legend_row(legend: Vec<(Hsla, SharedString)>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .flex_wrap()
        .gap_x_3()
        .gap_y_1()
        .children(legend.into_iter().map(|(color, name)| {
            h_flex()
                .gap_1()
                .items_center()
                .child(div().size_2().rounded_full().bg(color))
                .child(div().text_xs().text_color(muted).child(name))
        }))
        .into_any_element()
}

/// A pie over `slices` and its legend (each slice with its share), shared by
/// the results panel and dashboards so both color and fold slices alike.
/// Also returns the total the shares are of.
pub(crate) fn pie_parts(
    slices: &[Arc<PieSlice>],
    series: SharedString,
    id: impl Into<ElementId>,
    cx: &App,
) -> (AnyElement, Vec<(Hsla, SharedString)>, f64) {
    let palette = crate::ui::palette::series(cx);
    let muted = cx.theme().muted_foreground;
    // Past the palette, slices reuse its colors at lower strength so
    // neighbours still differ.
    let color = move |slice: &PieSlice| match slice.slot {
        Some(slot) if slot < palette.len() => palette[slot],
        Some(slot) => palette[slot % palette.len()].opacity(0.55),
        None => muted.opacity(0.6),
    };
    let total: f64 = slices.iter().map(|s| s.value as f64).sum();
    let legend = slices
        .iter()
        .map(|s| {
            let share = s.value as f64 / total * 100.;
            // A real slice too thin for one decimal is not nothing.
            let share = if share > 0. && share < 0.05 {
                "<0.1%".to_string()
            } else {
                format!("{share:.1}%")
            };
            (
                color(s),
                SharedString::from(format!("{} ({share})", s.name)),
            )
        })
        .collect();
    let chart = PieChart::new(slices.to_vec())
        .value(|s: &Arc<PieSlice>| s.value)
        .color(move |s: &Arc<PieSlice>| color(s))
        .pad_angle(0.01)
        .tooltip_name(|s: &Arc<PieSlice>| s.name.clone())
        .name(series)
        .id(id)
        .into_any_element();
    (chart, legend, total)
}

/// An axis's tick labels, in one unit: the largest tick picks K or M for
/// all of them, so an axis reads `14K 12K 10K 8K`, not `14K 12K 10K 8000`.
pub(crate) fn format_ticks(ticks: &[f64]) -> Vec<String> {
    let largest = ticks.iter().fold(0f64, |most, t| most.max(t.abs()));
    let (divisor, suffix) = if largest >= 1_000_000.0 {
        (1_000_000.0, "M")
    } else if largest >= 10_000.0 {
        (1_000.0, "K")
    } else {
        return ticks.iter().map(|t| format_value(*t)).collect();
    };
    ticks
        .iter()
        .map(|t| {
            let scaled = t / divisor;
            if scaled == 0. {
                "0".to_string()
            } else if scaled == scaled.trunc() {
                format!("{scaled:.0}{suffix}")
            } else {
                format!("{scaled:.1}{suffix}")
            }
        })
        .collect()
}

pub(crate) fn format_value(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 10_000.0 {
        format!("{:.0}K", value / 1_000.0)
    } else if value == value.trunc() {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{format_ticks, parse_number};

    #[test]
    fn an_axis_speaks_one_unit() {
        assert_eq!(
            format_ticks(&[0., 2000., 4000., 8000., 10000., 12000.]),
            ["0", "2K", "4K", "8K", "10K", "12K"]
        );
        assert_eq!(format_ticks(&[0., 2500., 12500.]), ["0", "2.5K", "12.5K"]);
        assert_eq!(
            format_ticks(&[0., 500000., 1500000.]),
            ["0", "0.5M", "1.5M"]
        );
        // Below ten thousand, plain numbers, as before.
        assert_eq!(format_ticks(&[0., 2.5, 8000.]), ["0", "2.5", "8000"]);
    }

    #[test]
    fn thousands_separators_still_parse() {
        assert_eq!(parse_number("1,884,201.40"), Some(1_884_201.40));
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("NULL"), None);
    }

}

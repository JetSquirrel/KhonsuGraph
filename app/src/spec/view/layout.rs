//! The twelve-column grid: which plots share a row, how tall each row is, and
//! the one plot blown up to fill the tab.

use super::*;

impl Dashboard {
    /// Blow plot `name` up to fill the dashboard, or put it back.
    pub(super) fn toggle_maximized(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.maximized.as_deref() == Some(name) {
            None
        } else {
            window.focus(&self.focus, cx);
            Some(name.to_string())
        };
        self.set_maximized(next, window, cx);
    }

    /// Enlarge plot `name`, or none. An enlarged graph panel has room for
    /// the inspector its grid cell leaves out.
    pub(super) fn set_maximized(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        for (plot, (canvas, _, _)) in &self.graph_canvases {
            let enlarged = name.as_deref() == Some(plot.as_str());
            canvas.update(cx, |canvas, cx| canvas.set_compact(!enlarged, window, cx));
        }
        self.maximized = name;
        cx.notify();
    }

    /// The blown-up plot's index; `None` once a reload drops it.
    pub(super) fn maximized_ix(&self) -> Option<usize> {
        let name = self.maximized.as_deref()?;
        self.plots.iter().position(|plot| plot.name == name)
    }

    /// The blown-up plot alone, drawn over the whole window below the title
    /// bar — as the query canvas fills it — so the sidebar, the tabs and the
    /// rest of the dashboard step aside. The dashboard keeps a note where
    /// the plots were; the title bar stays, so the window's buttons do too.
    pub(super) fn render_maximized(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport = window.viewport_size();
        if self.plots[ix].kind == "card" {
            let room = Some(viewport.width.as_f32() - CARD_PADDING - PAGE_GAP * 2.);
            let size = card_value_size(self.plots[ix].card.as_deref().unwrap_or_default(), room);
            self.card_sizes.insert(ix, size);
        }
        let boxed = self.plots[ix].kind != "text";
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let overlay = div()
            .id(format!("dashboard-maximized-{}", self.id))
            .occlude()
            .w(viewport.width)
            .h(viewport.height - TITLE_BAR_HEIGHT)
            .bg(crate::ui::palette::page(cx))
            .text_color(cx.theme().foreground)
            .border_t_1()
            .border_color(border)
            .p(px(PAGE_GAP))
            .child(
                div()
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .when(boxed, |this| {
                        this.bg(crate::ui::palette::panel(cx))
                            .border_1()
                            .border_color(border)
                            .rounded(px(PANEL_RADIUS))
                    })
                    .child(self.render_plot(ix, window, cx))
                    .children(self.render_plot_presence(ix, cx)),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(muted)
            .child(tr("dashboard.maximized_note"))
            .child(
                deferred(anchored().position(point(px(0.), TITLE_BAR_HEIGHT)).child(overlay))
                    .with_priority(1),
            )
            .into_any_element()
    }

    /// The font size at which card `ix`'s value fits its share of the
    /// dashboard's last laid-out width, less its padding.
    pub(super) fn card_value_size(&self, ix: usize) -> f32 {
        let plot = &self.plots[ix];
        let room = self.wheel.viewport().get().map(|viewport| {
            viewport.size.width.as_f32() * plot.width as f32 / model::GRID_COLUMNS as f32
                - CARD_PADDING
        });
        card_value_size(plot.card.as_deref().unwrap_or_default(), room)
    }

    /// The height a row of text panels needs for its title and words at the
    /// width each has and the interface size in use: a fixed height clipped
    /// a second line, and every line at a larger size.
    pub(super) fn text_row_height(&self, row: &[usize]) -> f32 {
        let scale = f32::from(crate::ui::scale::design(1.));
        let viewport = self.wheel.viewport().get().map(|v| v.size.width.as_f32());
        row.iter()
            .map(|&ix| {
                let plot = &self.plots[ix];
                let room = viewport.map_or(TEXT_ROOM_GUESS, |width| {
                    width * plot.width as f32 / model::GRID_COLUMNS as f32
                        - 2. * (CARD_INSET + 1.)
                        - PAGE_GAP
                });
                let per_line = (room / (TEXT_CHAR * scale)).max(8.);
                let lines: f32 = plot
                    .text
                    .as_deref()
                    .unwrap_or_default()
                    .lines()
                    .map(|line| {
                        let width = unicode_width::UnicodeWidthStr::width(line) as f32;
                        (width / per_line).ceil().max(1.)
                    })
                    .sum();
                let title = if plot.title.is_empty() {
                    0.
                } else {
                    TEXT_TITLE
                };
                (TEXT_PADDING + title + lines * TEXT_LINE) * scale
            })
            .fold(TEXT_DEFAULT, f32::max)
    }

    /// The height a grid row of nothing but drawn tables needs to show every
    /// row of the longest, up to a chart's default height; `None` for any
    /// other row.
    pub(super) fn table_row_height(&self, row: &[usize]) -> Option<f32> {
        let rows = row
            .iter()
            .map(|&ix| {
                let plot = &self.plots[ix];
                if plot.kind != "table" || plot.failure.is_some() {
                    return None;
                }
                let result = self.results.get(&plot.query)?;
                // A truncated result carries a banner above the grid.
                Some(result.rows.len() + usize::from(result.truncated))
            })
            .collect::<Option<Vec<_>>>()?;
        let longest = rows.into_iter().max()? as f32;
        Some((TABLE_CHROME + longest * TABLE_ROW).min(PLOT_DEFAULT))
    }
}

/// Plots in rows of the grid: each row takes plots in order until the next
/// would not fit in the twelve columns, then a new row starts. Indices into
/// the plot list, every plot in exactly one row.
pub(super) fn grid_rows(widths: impl IntoIterator<Item = u8>) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = model::GRID_COLUMNS;
    for (ix, width) in widths.into_iter().enumerate() {
        if used + width > model::GRID_COLUMNS {
            rows.push(Vec::new());
            used = 0;
        }
        used += width;
        rows.last_mut().expect("pushed above").push(ix);
    }
    rows
}

/// A card: its title over its one value, large. What was dropped to get to
/// one value — the rows past the first — is said under it, small.
/// A card's inset from its border, each side.
pub(super) const CARD_INSET: f32 = 20.;

/// What a card's share of the width loses before its value: the inset and
/// border on both sides, and the page gap around the card.
pub(super) const CARD_PADDING: f32 = 2. * (CARD_INSET + 1.) + PAGE_GAP;

/// A card value's font size: as large as this, and never smaller than the
/// floor — past that, a value that still does not fit is cut off.
pub(super) const CARD_VALUE_MAX: f32 = 32.;

pub(super) const CARD_VALUE_MIN: f32 = 14.;

/// The font size at which `value` fits across `room` pixels. Digits and
/// separators are about 0.6em wide in the UI font; a wide (CJK) character
/// counts double.
pub(super) fn card_value_size(value: &str, room: Option<f32>) -> f32 {
    use unicode_width::UnicodeWidthStr;
    let Some(room) = room.filter(|room| *room > 0.) else {
        return CARD_VALUE_MAX;
    };
    let ems = value.width().max(1) as f32 * 0.6;
    (room / ems).clamp(CARD_VALUE_MIN, CARD_VALUE_MAX)
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: that brings `gpui_kit::*`, whose `test` macro
    // shadows the built-in `#[test]`.
    use super::{card_value_size, grid_rows, CARD_VALUE_MAX, CARD_VALUE_MIN};

    #[test]
    fn a_card_value_shrinks_to_fit_rather_than_clip() {
        // Room to spare, or no layout yet: full size.
        assert_eq!(card_value_size("42", Some(300.)), CARD_VALUE_MAX);
        assert_eq!(card_value_size("11,439,704.50", None), CARD_VALUE_MAX);
        // Thirteen characters in 150px: smaller, and it fits.
        let size = card_value_size("11,439,704.50", Some(150.));
        assert!(size < CARD_VALUE_MAX && 13. * 0.6 * size <= 150.);
        // Never below the floor, however narrow.
        assert_eq!(card_value_size("11,439,704.50", Some(20.)), CARD_VALUE_MIN);
    }

    #[test]
    fn plots_fill_rows_and_wrap() {
        // Four cards, then two halves, then a full row, then a lone third
        // that no longer fits beside an eight.
        assert_eq!(
            grid_rows([3, 3, 3, 3, 6, 6, 12, 8, 6]),
            vec![vec![0, 1, 2, 3], vec![4, 5], vec![6], vec![7], vec![8]]
        );
        // Existing specs: every plot the full width, one per row.
        assert_eq!(grid_rows([12, 12]), vec![vec![0], vec![1]]);
        assert!(grid_rows([]).is_empty());
    }
}

//! How the canvas draws: captions, highlighting, groups, style, compact and
//! maximized.

use super::*;

impl GraphCanvas {
    /// For a dashboard panel: no inspector, `caption` as the caption and
    /// nodes grouped by `group`. Takes effect with the next `set_data`.
    /// A dashboard `graph`'s `labels` and `highlight` (see
    /// `khonsu_dash::model::LABELS`); `None` keeps the defaults.
    pub fn set_look(&mut self, labels: Option<&str>, highlight: Option<&str>, cx: &mut Context<Self>) {
        self.label_mode = match labels {
            Some("all") => LabelMode::Always,
            Some("focus") => LabelMode::Focus,
            Some("none") => LabelMode::Never,
            _ => LabelMode::Auto,
        };
        self.focus_dimming = highlight != Some("none");
        let (mode, dimming) = (self.label_mode, self.focus_dimming);
        self.graph.update(cx, |graph, cx| {
            graph.set_label_mode(mode, cx);
            graph.set_focus_dimming(dimming, cx);
        });
    }

    pub fn configure(&mut self, caption_key: Option<String>, group_key: Option<String>, compact: bool, cx: &mut Context<Self>) {
        self.caption_key = caption_key;
        self.group_key = group_key;
        self.style = NodeStyle::default();
        self.compact = compact;
        self.can_fill_window = !compact;
        cx.notify();
    }

    /// Color and size the nodes by their properties, for a dashboard `graph`.
    /// Takes effect with the next `set_data`; call after `configure`.
    pub fn set_style(&mut self, style: NodeStyle) {
        self.style = style;
    }

    pub(super) fn look(&self) -> Look<'_> {
        Look {
            caption: self.caption_key.as_deref(),
            group: self.group_key.as_deref(),
            style: &self.style,
            color_slots: &self.color_slots,
            size_range: self.size_range,
        }
    }

    /// Give new values of the color property their slots, and widen the size
    /// range to take in `nodes`.
    pub(super) fn note_styled(&mut self, nodes: &[GraphNode]) {
        if let Some(key) = self.style.color.clone() {
            for value in nodes.iter().filter_map(|n| group_value(n, &key)) {
                if !self.color_slots.contains(&value) {
                    self.color_slots.push(value);
                }
            }
        }
        if let Some(key) = self.style.size.clone() {
            for value in nodes.iter().filter_map(|n| number(n, &key)) {
                self.size_range = Some(match self.size_range {
                    Some((low, high)) => (low.min(value), high.max(value)),
                    None => (value, value),
                });
            }
        }
    }

    /// Group the nodes on the canvas by `key`, or stop grouping. The layout
    /// starts over, since group members are drawn together.
    pub fn set_group_key(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.group_key == key {
            return;
        }
        self.group_key = key;
        self.selected = None;
        let look = self.look();
        let nodes: Vec<Node> = self.nodes.values().map(|n| to_node(n, &look, cx)).collect();
        let edges: Vec<Edge> = self.edges.values().map(to_edge).collect();
        let color_by = self.color_by();
        self.graph.update(cx, |graph, cx| {
            graph.set_color_by(color_by, cx);
            graph.set_data(Graph::new(nodes, edges), cx);
        });
        cx.notify();
    }

    /// Show the canvas over the whole window, or back in its place, and fit
    /// the graph to its new size once that size has been laid out.
    pub fn toggle_maximized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.maximized = !self.maximized;
        if self.maximized {
            self.previous_focus = window.focused(cx);
            window.focus(&self.focus, cx);
        } else if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        }
        self.fit_once_laid_out(window);
        cx.notify();
    }

    /// Show the inspector beside the canvas, or not, as a dashboard panel
    /// does once enlarged; the graph is fitted to the size it is left with.
    pub fn set_compact(&mut self, compact: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.compact == compact {
            return;
        }
        self.compact = compact;
        self.fit_once_laid_out(window);
        cx.notify();
    }

    /// Fit the graph to the canvas after the next layout, once a change of
    /// size has landed.
    pub(super) fn fit_once_laid_out(&self, window: &mut Window) {
        let graph = self.graph.clone();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, cx| {
                graph.update(cx, |graph, cx| graph.fit_view(cx));
            });
        });
    }

    pub(super) fn color_by(&self) -> ColorBy {
        if self.group_key.is_some() {
            ColorBy::Group
        } else {
            ColorBy::Category
        }
    }
}

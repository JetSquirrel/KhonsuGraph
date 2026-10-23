//! The graph canvas: nodes and relationships of a result, drawn by
//! [`gpui_graph::NetworkGraph`]. This view owns the HawDB side — converting
//! results, fetching missing endpoints and neighbors, the inspector and the
//! legend — and leaves layout, painting and interaction to the component.
//!
//! Drag the background to pan, scroll to zoom, drag a node to move it. Click
//! a node or relationship to inspect it; double-click a node to add its
//! neighbors around it. Expanding never moves nodes already on the canvas.
//! Grouping by a node property outlines each value's nodes; double-click a
//! collapsed group to open it again.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use gpui_graph::{ColorBy, Edge, GraphData as Graph, LabelMode, NetworkGraph, NetworkGraphEvent, Node, Selection};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt, TITLE_BAR_HEIGHT};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::{GraphData, GraphEdge, GraphNode};
use serde_json::Value as Json;

use crate::i18n::{tr, trf};
use crate::ui::palette;

/// Above this many nodes the canvas shows the first ones and says so.
pub const NODE_LIMIT: usize = 2000;
const CAPTION_KEYS: &[&str] = &["name", "title", "label", "display_name", "id", "key", "khonsu_id"];
/// Edge property carrying the HawDB relationship id, so a selected edge maps back.
const EDGE_ID: &str = "khonsu_edge_id";

fn caption(node: &GraphNode, preferred: Option<&str>) -> String {
    for key in preferred.into_iter().chain(CAPTION_KEYS.iter().copied()) {
        match node.properties.get(key) {
            Some(Json::String(s)) => return truncate(s, 26),
            Some(v @ (Json::Number(_) | Json::Bool(_))) => return v.to_string(),
            _ => {}
        }
    }
    format!("{} #{}", node.labels.first().map(String::as_str).unwrap_or("node"), node.id)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn first_label(node: &GraphNode) -> &str {
    node.labels.first().map(String::as_str).unwrap_or("")
}

/// At most this many groups are offered; more is a name, not a grouping.
const MAX_GROUPS: usize = 60;

/// `node`'s value of `key` as a group id, when it is a plain value.
fn group_value(node: &GraphNode, key: &str) -> Option<String> {
    match node.properties.get(key)? {
        Json::String(s) if !s.is_empty() => Some(s.clone()),
        v @ (Json::Number(_) | Json::Bool(_)) => Some(v.to_string()),
        _ => None,
    }
}

/// Node properties worth grouping by: plain values that several nodes
/// share, fewer distinct ones than nodes. Provenance is left out.
fn group_keys<'a>(nodes: impl Iterator<Item = &'a GraphNode>) -> Vec<String> {
    let mut seen: BTreeMap<&str, (usize, BTreeSet<String>)> = BTreeMap::new();
    let mut total = 0;
    for node in nodes {
        total += 1;
        for key in node.properties.keys().filter(|k| !k.starts_with("khonsu_")) {
            if let Some(value) = group_value(node, key) {
                let (count, values) = seen.entry(key).or_default();
                *count += 1;
                if values.len() <= MAX_GROUPS {
                    values.insert(value);
                }
            }
        }
    }
    seen.into_iter()
        .filter(|(_, (count, values))| {
            values.len() >= 2 && values.len() <= MAX_GROUPS && values.len() * 2 <= *count && *count * 2 >= total
        })
        .map(|(key, _)| key.to_string())
        .collect()
}

/// The component's node for `node`, colored like its label in the sidebar,
/// or by its group when grouping by `group` and it has a value for it.
fn to_node(node: &GraphNode, preferred: Option<&str>, group: Option<&str>, cx: &App) -> Node {
    let label = first_label(node);
    let converted = Node::new(node.id.to_string(), caption(node, preferred), label.to_string());
    match group.and_then(|key| group_value(node, key)) {
        Some(value) => converted.group(value),
        None => converted.color(palette::label_color(label, cx)),
    }
}

fn to_edge(edge: &GraphEdge) -> Edge {
    Edge::new(edge.source.to_string(), edge.target.to_string(), edge.rel_type.clone())
        .property(EDGE_ID, edge.id.to_string())
}

fn edge_id(edge: &Edge) -> Option<i64> {
    edge.properties.iter().find(|(k, _)| k.as_ref() == EDGE_ID)?.1.parse().ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    Node(i64),
    Edge(i64),
}

pub struct GraphCanvas {
    graph: Entity<NetworkGraph>,
    /// Everything on the canvas, by HawDB id, for the inspector.
    nodes: HashMap<i64, GraphNode>,
    edges: HashMap<i64, GraphEdge>,
    selected: Option<Selected>,
    /// Nodes the result held beyond [`NODE_LIMIT`].
    hidden: usize,
    expanding: bool,
    /// The node property shown as each node's caption, before the usual
    /// name-like ones: a dashboard `graph`'s `caption`.
    caption_key: Option<String>,
    /// The node property nodes are grouped by, if any.
    group_key: Option<String>,
    /// Which captions are drawn, and whether a hovered or picked node fades
    /// the rest: a dashboard `graph`'s `labels` and `highlight`.
    label_mode: LabelMode,
    focus_dimming: bool,
    /// A dashboard panel shows no inspector: its width is the grid's.
    compact: bool,
    /// Drawn over the whole window below the title bar, instead of in place.
    maximized: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    /// Offers the button that fills the window; a dashboard panel has its
    /// own, which enlarges the panel within the dashboard.
    can_fill_window: bool,
    _subscriptions: Vec<Subscription>,
}

/// What a canvas tells its host: a node was selected, for a dashboard filter
/// to pick its property.
pub enum CanvasEvent {
    NodeSelected(GraphNode),
}

impl EventEmitter<CanvasEvent> for GraphCanvas {}

impl GraphCanvas {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let graph = cx.new(|cx| NetworkGraph::new(cx).with_label_mode(LabelMode::Auto));
        let subscription = cx.subscribe_in(&graph, window, Self::on_graph_event);
        Self {
            graph,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            selected: None,
            hidden: 0,
            expanding: false,
            caption_key: None,
            group_key: None,
            label_mode: LabelMode::Auto,
            focus_dimming: true,
            compact: false,
            maximized: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            can_fill_window: true,
            _subscriptions: vec![subscription],
        }
    }

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
        self.compact = compact;
        self.can_fill_window = !compact;
        cx.notify();
    }

    /// Group the nodes on the canvas by `key`, or stop grouping. The layout
    /// starts over, since group members are drawn together.
    pub fn set_group_key(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.group_key == key {
            return;
        }
        self.group_key = key;
        self.selected = None;
        let nodes: Vec<Node> = self
            .nodes
            .values()
            .map(|n| to_node(n, self.caption_key.as_deref(), self.group_key.as_deref(), cx))
            .collect();
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
    fn fit_once_laid_out(&self, window: &mut Window) {
        let graph = self.graph.clone();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, cx| {
                graph.update(cx, |graph, cx| graph.fit_view(cx));
            });
        });
    }

    fn color_by(&self) -> ColorBy {
        if self.group_key.is_some() {
            ColorBy::Group
        } else {
            ColorBy::Category
        }
    }

    /// Show a new result. Relationship endpoints the result left out are
    /// fetched so every edge has both ends on the canvas.
    pub fn set_data(&mut self, data: GraphData, window: &mut Window, cx: &mut Context<Self>) {
        self.nodes.clear();
        self.edges.clear();
        self.selected = None;
        self.hidden = data.nodes.len().saturating_sub(NODE_LIMIT);
        let missing = data.dangling_endpoints();
        let mut nodes = data.nodes;
        nodes.truncate(NODE_LIMIT);
        let edges = data.edges;
        if missing.is_empty() || missing.len() > 500 || self.hidden > 0 {
            self.load(nodes, edges, cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let extra = smol::unblock(move || crate::db::nodes_by_id(&missing)).await;
            this.update(cx, |this, cx| {
                if let Ok(extra) = extra {
                    nodes.extend(extra.nodes);
                }
                this.load(nodes, edges, cx);
            })
            .ok();
        })
        .detach();
    }

    fn load(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &mut Context<Self>) {
        let data = self.admit(nodes, edges, cx);
        let color_by = self.color_by();
        let label_mode = self.label_mode;
        self.graph.update(cx, |graph, cx| {
            // Auto culls overlapping captions, even on small graphs fitted
            // into narrow dashboard panels; a dashboard may choose otherwise.
            graph.set_label_mode(label_mode, cx);
            graph.set_color_by(color_by, cx);
            graph.set_data(data, cx);
        });
        cx.notify();
    }

    /// Record the nodes and edges not on the canvas yet and convert them.
    /// Edges whose endpoints are missing are dropped.
    fn admit(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &App) -> Graph {
        palette::register_labels(nodes.iter().map(first_label).filter(|l| !l.is_empty()));
        let mut fresh_nodes = Vec::new();
        for node in nodes {
            if !self.nodes.contains_key(&node.id) {
                fresh_nodes.push(to_node(&node, self.caption_key.as_deref(), self.group_key.as_deref(), cx));
                self.nodes.insert(node.id, node);
            }
        }
        let mut fresh_edges = Vec::new();
        for edge in edges {
            if self.nodes.contains_key(&edge.source)
                && self.nodes.contains_key(&edge.target)
                && !self.edges.contains_key(&edge.id)
            {
                fresh_edges.push(to_edge(&edge));
                self.edges.insert(edge.id, edge);
            }
        }
        Graph::new(fresh_nodes, fresh_edges)
    }

    fn on_graph_event(
        &mut self,
        _: &Entity<NetworkGraph>,
        event: &NetworkGraphEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            NetworkGraphEvent::SelectionChanged(selection) => {
                self.selected = match selection {
                    Selection::Node(node) => node.id.parse().ok().map(Selected::Node),
                    Selection::Edge(edge) => edge_id(edge).map(Selected::Edge),
                    Selection::EdgeBundle(edges) => edges.first().and_then(edge_id).map(Selected::Edge),
                    _ => None,
                };
                if let Some(Selected::Node(id)) = self.selected {
                    if let Some(node) = self.nodes.get(&id) {
                        cx.emit(CanvasEvent::NodeSelected(node.clone()));
                    }
                }
                cx.notify();
            }
            NetworkGraphEvent::NodeActivated(node) => {
                if let Ok(id) = node.id.parse() {
                    self.expand(id, window, cx);
                }
            }
            _ => {}
        }
    }

    /// Add `id`'s neighbors around it.
    fn expand(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.expanding {
            return;
        }
        self.expanding = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || crate::db::neighbors(id)).await;
            this.update_in(cx, |this, window, cx| {
                this.expanding = false;
                match result {
                    Ok(data) => {
                        let fresh = data.nodes.iter().filter(|n| !this.nodes.contains_key(&n.id)).count();
                        if this.nodes.len() + fresh > NODE_LIMIT {
                            window.push_notification(
                                Notification::warning(trf("canvas.limit", &[&NODE_LIMIT.to_string()])),
                                cx,
                            );
                        } else {
                            let data = this.admit(data.nodes, data.edges, cx);
                            let anchor = id.to_string();
                            this.graph.update(cx, |graph, cx| {
                                graph.merge(data, Some(&anchor), cx);
                            });
                        }
                    }
                    Err(e) => window.push_notification(Notification::error(e.message), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let mut rows: Vec<(&String, &Json)> = properties.iter().collect();
        // Provenance last: it says where a value came from, not what it is.
        rows.sort_by_key(|(k, _)| (k.starts_with("khonsu_"), k.to_string()));
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.theme().muted_foreground;
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
                                .label(tr("canvas.expand"))
                                .loading(self.expanding)
                                .on_click(cx.listener(move |this, _, window, cx| this.expand(id, window, cx))),
                        )
                    },
                )
                .into_any_element(),
        )
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let halo = theme.background;
        let muted = theme.muted_foreground;
        let border = theme.border;

        let mut legend: BTreeMap<String, usize> = BTreeMap::new();
        for label in self.nodes.values().map(first_label).filter(|l| !l.is_empty()) {
            *legend.entry(label.to_string()).or_default() += 1;
        }

        let group_keys = group_keys(self.nodes.values());
        let group_key = self.group_key.clone();
        let this = cx.entity().downgrade();

        let note = if self.hidden > 0 {
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
                                        .children(legend.into_iter().map(|(label, count)| {
                                            h_flex()
                                                .gap_1p5()
                                                .items_center()
                                                .child(div().size_2p5().rounded_full().bg(palette::label_color(&label, cx)))
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
                            .text_color(muted)
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

#[cfg(test)]
mod tests {
    use super::{edge_id, group_keys, to_edge};
    use khonsu_core::{GraphEdge, GraphNode};
    use serde_json::json;

    fn person(id: i64, name: &str, community: i64, city: &str) -> GraphNode {
        GraphNode {
            id,
            labels: vec!["Person".into()],
            properties: [
                ("name".to_string(), json!(name)),
                ("community".to_string(), json!(community)),
                ("city".to_string(), json!(city)),
                ("khonsu_source".to_string(), json!("people")),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn shared_plain_values_are_offered_for_grouping_and_names_are_not() {
        let nodes = [
            person(1, "Ada", 1, "London"),
            person(2, "Bob", 1, "London"),
            person(3, "Cy", 2, "Paris"),
            person(4, "Di", 2, "Paris"),
        ];
        assert_eq!(group_keys(nodes.iter()), vec!["city".to_string(), "community".to_string()]);
    }

    #[test]
    fn edge_round_trips_its_id() {
        let edge = GraphEdge {
            id: 42,
            source: 1,
            target: 2,
            rel_type: "KNOWS".into(),
            properties: Default::default(),
        };
        let converted = to_edge(&edge);
        assert_eq!(converted.source.as_ref(), "1");
        assert_eq!(converted.target.as_ref(), "2");
        assert_eq!(edge_id(&converted), Some(42));
    }
}

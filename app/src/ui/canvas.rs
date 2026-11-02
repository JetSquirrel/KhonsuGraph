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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_graph::{ColorBy, Edge, GraphData as Graph, LabelMode, NetworkGraph, NetworkGraphEvent, Node, Selection};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt, TITLE_BAR_HEIGHT};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::evidence::{self, Evidence};
use khonsu_core::explore::{Direction, Exploration};
use khonsu_core::exposure::{Status, Verdict};
use khonsu_core::settings::{self, Source};
use khonsu_core::{GraphData, GraphEdge, GraphNode};
use serde_json::Value as Json;

use crate::i18n::{tr, trf};
use crate::ui::explorer::{self, Explorer, View};
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
    explorer: Explorer,
    /// Configured sources, read when a selection changes, for the inspector's
    /// evidence.
    sources: Vec<Source>,
    /// The canvas shows only one path of the last walk.
    path_view: bool,
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
            explorer: Explorer::default(),
            sources: Vec::new(),
            path_view: false,
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
        self.explorer.forget_walks();
        self.set_path_view(false, cx);
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
                self.sources = settings::load_sources();
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
                    self.load_relationship_types(window, cx);
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

    /// Offer the graph's relationship types as filters, once a node is picked.
    fn load_relationship_types(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compact || !self.explorer.available.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let schema = smol::unblock(crate::db::schema).await;
            this.update(cx, |this, cx| {
                if let Ok(schema) = schema {
                    this.explorer.available = schema.relationship_types.into_iter().map(|rel| rel.name).collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// By ID, so that a view comes back in the order it would have anyway.
    fn view(&self) -> View {
        let mut nodes: Vec<GraphNode> = self.nodes.values().cloned().collect();
        let mut edges: Vec<GraphEdge> = self.edges.values().cloned().collect();
        nodes.sort_by_key(|node| node.id);
        edges.sort_by_key(|edge| edge.id);
        View {
            nodes,
            edges,
            walk: self.explorer.walk.clone(),
            selected: self.selected_node(),
        }
    }

    fn selected_node(&self) -> Option<i64> {
        match self.selected {
            Some(Selected::Node(id)) => Some(id),
            _ => None,
        }
    }

    /// Walk from the chosen starts and add what it finds to the canvas. A walk
    /// that would not fit adds nothing and says so, as expanding does.
    fn explore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.explorer.running.is_some() {
            return;
        }
        let Some(request) = self.explorer.request(self.selected_node()) else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.explorer.running = Some(cancel.clone());
        self.explorer.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let anchor = request.starts[0].to_string();
            let result = smol::unblock(move || crate::db::explore(&request, &cancel)).await;
            this.update_in(cx, |this, window, cx| {
                this.explorer.running = None;
                match result {
                    Ok(walk) => this.show_walk(walk, &anchor, window, cx),
                    Err(error) => this.explorer.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_walk(&mut self, walk: Exploration, anchor: &str, window: &mut Window, cx: &mut Context<Self>) {
        let fresh = walk.graph.nodes.iter().filter(|node| !self.nodes.contains_key(&node.id)).count();
        if self.nodes.len() + fresh > NODE_LIMIT {
            window.push_notification(Notification::warning(trf("canvas.limit", &[&NODE_LIMIT.to_string()])), cx);
            return;
        }
        self.explorer.push_view(self.view());
        self.set_path_view(false, cx);
        let data = self.admit(walk.graph.nodes.clone(), walk.graph.edges.clone(), cx);
        self.graph.update(cx, |graph, cx| graph.merge(data, Some(anchor), cx));
        self.explorer.walk = Some(walk);
    }

    /// Evaluate the exposure rules over the whole graph, not just the canvas.
    fn check_exposure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.explorer.checking_exposure {
            return;
        }
        self.explorer.checking_exposure = true;
        self.explorer.exposure_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(crate::db::exposure).await;
            this.update(cx, |this, cx| {
                this.explorer.checking_exposure = false;
                match result {
                    Ok(report) => this.explorer.exposure = Some(report),
                    Err(error) => this.explorer.exposure_error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show a finding's candidate path, as a walk Path Explorer can go on from.
    fn show_finding(&mut self, finding_id: &str, target: i64, cx: &mut Context<Self>) {
        let report = self.explorer.exposure.as_ref();
        let Some(walk) = report.and_then(|report| report.findings.iter().find(|finding| finding.finding_id == finding_id)).and_then(|finding| finding.exploration()) else {
            return;
        };
        self.explorer.push_view(self.view());
        self.replace_canvas(walk.graph.nodes.clone(), walk.graph.edges.clone(), cx);
        self.explorer.walk = Some(walk);
        self.set_path_view(true, cx);
        let id = target.to_string();
        self.graph.update(cx, |graph, cx| graph.select_node(Some(&id), false, cx));
    }

    /// Narrow the canvas to the path the last walk took to `target`.
    fn show_path(&mut self, target: i64, cx: &mut Context<Self>) {
        let Some(path) = self.explorer.walk.as_ref().and_then(|walk| explorer::path_graph(walk, target)) else {
            return;
        };
        self.explorer.push_view(self.view());
        self.replace_canvas(path.nodes, path.edges, cx);
        self.set_path_view(true, cx);
        let id = target.to_string();
        self.graph.update(cx, |graph, cx| graph.select_node(Some(&id), false, cx));
    }

    /// Return to the canvas as it was before the last walk or path.
    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.explorer.pop_view() {
            self.explorer.walk = view.walk;
            self.explorer.error = None;
            self.replace_canvas(view.nodes, view.edges, cx);
            self.set_path_view(false, cx);
            // Back where the person was, with the panel still in reach.
            let id = view.selected.map(|id| id.to_string());
            self.graph.update(cx, |graph, cx| graph.select_node(id.as_deref(), false, cx));
        }
    }

    /// On a path every node is the point, so a selected one must not fade the
    /// rest; elsewhere the canvas's usual setting applies.
    fn set_path_view(&mut self, on: bool, cx: &mut Context<Self>) {
        self.path_view = on;
        let dimming = self.focus_dimming && !on;
        self.graph.update(cx, |graph, cx| graph.set_focus_dimming(dimming, cx));
    }

    fn replace_canvas(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &mut Context<Self>) {
        self.nodes.clear();
        self.edges.clear();
        self.selected = None;
        self.hidden = 0;
        self.load(nodes, edges, cx);
    }

    /// Where the selected record came from, with what is missing stated.
    fn render_evidence(&self, record: Evidence, cx: &mut Context<Self>) -> AnyElement {
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

    /// What the rules say about a selected instance, and why. Not shown for
    /// anything else: the rules are only about instances.
    fn render_exposure(&self, id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let node = self.nodes.get(&id).filter(|node| node.labels.iter().any(|label| label == "Instance"))?;
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let mono = cx.theme().mono_font_family.clone();
        let explorer = &self.explorer;
        let findings = explorer.exposure.as_ref().map(|report| explorer::findings_for(report, node));

        let mut list = Vec::new();
        for (ix, finding) in findings.iter().flatten().enumerate() {
            let title = explorer::rule_key(&finding.rule_id).map_or_else(|| finding.rule_id.clone(), |key| tr(key).to_string());
            let copy = serde_json::to_string_pretty(finding).expect("a finding serializes");
            let finding_id = finding.finding_id.clone();
            // Offered again only when another path is showing: Back would otherwise
            // step through the same view twice.
            let showing = self.path_view && explorer.walk.as_ref().is_some_and(|walk| walk.hops == finding.hops);
            let drawable = !finding.hops.is_empty() && !showing;
            let conditions = finding.conditions.iter().map(|condition| {
                let statement = explorer::condition_key(&condition.id).map_or_else(|| condition.statement.clone(), |key| tr(key).to_string());
                let color = if condition.status == Status::Met { muted } else { warning };
                let fields = condition.fields.iter().filter_map(|field| {
                    let value = field.value.known()?;
                    Some(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .font_family(mono.clone())
                            .text_color(muted)
                            .child(format!("{} = {}", field.aws_field, crate::ui::results::cell_text(value))),
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
            let verdict_color = if finding.verdict == Verdict::NotIndicated { muted } else { warning };
            list.push(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .child(div().text_xs().font_semibold().text_color(verdict_color).child(tr(explorer::verdict_key(finding.verdict))))
                    .child(div().text_xs().whitespace_normal().child(title))
                    .child(div().text_xs().font_family(mono.clone()).whitespace_normal().text_color(muted).child(finding.rule_id.clone()))
                    .children(conditions.collect::<Vec<_>>())
                    .when_some(finding.path.as_ref(), |this, path| {
                        this.child(div().pt_1().text_xs().font_semibold().child(tr("explore.path")))
                            .children(path.hops.iter().map(|hop| div().text_xs().whitespace_normal().font_family(mono.clone()).child(hop.clone())))
                    })
                    .child(div().pt_1().text_xs().font_semibold().child(tr("exposure.assumptions")))
                    .children(finding.assumptions.iter().map(|text| div().text_xs().whitespace_normal().text_color(muted).child(text.clone())))
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .when(drawable, |this| {
                                this.child(
                                    Button::new(("exposure-path", ix))
                                        .xsmall()
                                        .outline()
                                        .label(tr("exposure.show_path"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.show_finding(&finding_id, id, cx))),
                                )
                            })
                            .child(
                                Button::new(("exposure-copy", ix))
                                    .xsmall()
                                    .outline()
                                    .label(tr("exposure.copy"))
                                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                            ),
                    ),
            );
        }

        let checked = explorer.exposure.is_some();
        let complete = explorer.exposure.as_ref().is_none_or(|report| report.complete);
        let none = findings.as_ref().is_some_and(Vec::is_empty);
        Some(
            v_flex()
                .gap_1()
                .child(div().text_sm().font_semibold().child(tr("exposure.title")))
                .child(
                    Button::new("exposure-check")
                        .xsmall()
                        .outline()
                        .label(tr(if checked { "exposure.recheck" } else { "exposure.check" }))
                        .loading(explorer.checking_exposure)
                        .on_click(cx.listener(|this, _, window, cx| this.check_exposure(window, cx))),
                )
                .when_some(explorer.exposure_error.clone(), |this, error| {
                    this.child(div().text_xs().whitespace_normal().text_color(warning).child(trf("exposure.failed", &[&error])))
                })
                .when(checked, |this| this.child(div().text_xs().whitespace_normal().text_color(muted).child(tr("exposure.claim"))))
                .when(!complete, |this| this.child(div().text_xs().whitespace_normal().text_color(warning).child(tr("exposure.incomplete"))))
                .when(none, |this| this.child(div().text_xs().whitespace_normal().text_color(muted).child(tr("exposure.none"))))
                .children(list)
                .into_any_element(),
        )
    }

    fn selected_evidence(&self, selected: Selected) -> Option<Evidence> {
        match selected {
            Selected::Node(id) => self.nodes.get(&id).map(|node| evidence::for_node(node, &self.sources)),
            Selected::Edge(id) => {
                let edge = self.edges.get(&id)?;
                let ends: Vec<GraphNode> = [edge.source, edge.target].iter().filter_map(|id| self.nodes.get(id).cloned()).collect();
                Some(evidence::for_edge(edge, &ends, &self.sources))
            }
        }
    }

    fn render_explorer(&self, node: i64, cx: &mut Context<Self>) -> AnyElement {
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
        let exposure = self.selected_node().and_then(|id| self.render_exposure(id, cx));
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
                .when_some(exposure, |this, exposure| {
                    this.child(exposure).child(div().border_b_1().border_color(cx.theme().border))
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

impl Render for GraphCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let halo = theme.background;
        let muted = theme.muted_foreground;
        let warning = theme.warning;
        let border = theme.border;

        let mut legend: BTreeMap<String, usize> = BTreeMap::new();
        for label in self.nodes.values().map(first_label).filter(|l| !l.is_empty()) {
            *legend.entry(label.to_string()).or_default() += 1;
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

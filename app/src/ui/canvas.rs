//! The graph canvas: nodes and relationships of a result, laid out by a small
//! force simulation and drawn with GPUI. Edges are painted paths; nodes are
//! positioned elements so their captions use ordinary text layout.
//!
//! Drag the background to pan, scroll to zoom, drag a node to move it. Click
//! a node or relationship to inspect it; double-click a node to add its
//! neighbors around it. Expanding never moves nodes already on the canvas.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable, StyledExt, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::{GraphData, GraphEdge, GraphNode};
use serde_json::Value as Json;

use crate::i18n::{tr, trf};
use crate::ui::palette;

/// Above this many nodes the canvas shows the first ones and says so.
pub const NODE_LIMIT: usize = 2000;
const NODE_RADIUS: f32 = 13.0;
const CAPTION_KEYS: &[&str] = &["name", "title", "label", "display_name", "id", "key", "khonsu_id"];

fn caption(node: &GraphNode) -> String {
    for key in CAPTION_KEYS {
        match node.properties.get(*key) {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    Node(i64),
    Edge(i64),
}

#[derive(Clone, Copy)]
enum Drag {
    Pan { start: Point<Pixels>, origin: Point<f32> },
    Node { id: i64, last: Point<Pixels> },
}

pub struct GraphCanvas {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    index: HashMap<i64, usize>,
    positions: Vec<Point<f32>>,
    /// Screen offset of the world origin, in pixels.
    pan: Point<f32>,
    zoom: f32,
    selected: Option<Selected>,
    drag: Option<Drag>,
    /// Nodes the result held beyond [`NODE_LIMIT`].
    hidden: usize,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Set until the first paint knows the canvas size and can fit the graph.
    needs_fit: bool,
    expanding: bool,
}

impl GraphCanvas {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            index: HashMap::new(),
            positions: Vec::new(),
            pan: point(0., 0.),
            zoom: 1.0,
            selected: None,
            drag: None,
            hidden: 0,
            bounds: Rc::new(Cell::new(None)),
            needs_fit: true,
            expanding: false,
        }
    }

    /// Show a new result. Relationship endpoints the result left out are
    /// fetched so every edge has both ends on the canvas.
    pub fn set_data(&mut self, data: GraphData, window: &mut Window, cx: &mut Context<Self>) {
        self.nodes.clear();
        self.edges.clear();
        self.index.clear();
        self.positions.clear();
        self.selected = None;
        self.hidden = data.nodes.len().saturating_sub(NODE_LIMIT);
        let missing = data.dangling_endpoints();
        let mut nodes = data.nodes;
        nodes.truncate(NODE_LIMIT);
        for node in nodes {
            self.add_node(node, None);
        }
        let edges = data.edges;
        if missing.is_empty() || missing.len() > 500 || self.hidden > 0 {
            self.add_edges(edges);
            self.relayout();
            cx.notify();
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let extra = smol::unblock(move || crate::db::nodes_by_id(&missing)).await;
            this.update(cx, |this, cx| {
                if let Ok(extra) = extra {
                    for node in extra.nodes {
                        this.add_node(node, None);
                    }
                }
                this.add_edges(edges);
                this.relayout();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_node(&mut self, node: GraphNode, at: Option<Point<f32>>) -> bool {
        if self.index.contains_key(&node.id) {
            return false;
        }
        let position = at.unwrap_or_else(|| {
            // Seed on a spiral, deterministic so a result lays out the same way twice.
            let i = self.nodes.len() as f32;
            let angle = i * 2.399_963;
            let radius = 30.0 * i.sqrt();
            point(radius * angle.cos(), radius * angle.sin())
        });
        if let Some(label) = node.labels.first() {
            palette::register_labels([label.as_str()]);
        }
        self.index.insert(node.id, self.nodes.len());
        self.nodes.push(node);
        self.positions.push(position);
        true
    }

    fn add_edges(&mut self, edges: Vec<GraphEdge>) {
        let mut known: BTreeMap<i64, ()> = self.edges.iter().map(|e| (e.id, ())).collect();
        for edge in edges {
            if self.index.contains_key(&edge.source)
                && self.index.contains_key(&edge.target)
                && known.insert(edge.id, ()).is_none()
            {
                self.edges.push(edge);
            }
        }
    }

    /// Lay out every node again and fit the result in view.
    fn relayout(&mut self) {
        let fixed = vec![false; self.positions.len()];
        let links: Vec<(usize, usize)> = self
            .edges
            .iter()
            .filter_map(|e| Some((*self.index.get(&e.source)?, *self.index.get(&e.target)?)))
            .collect();
        let iterations = iterations_for(self.positions.len());
        force_layout(&mut self.positions, &links, &fixed, iterations);
        self.needs_fit = true;
    }

    fn fit(&mut self, bounds: Bounds<Pixels>) {
        if self.positions.is_empty() {
            return;
        }
        let (mut min, mut max) = (point(f32::MAX, f32::MAX), point(f32::MIN, f32::MIN));
        for p in &self.positions {
            min = point(min.x.min(p.x), min.y.min(p.y));
            max = point(max.x.max(p.x), max.y.max(p.y));
        }
        let width = f32::from(bounds.size.width).max(1.0);
        let height = f32::from(bounds.size.height).max(1.0);
        let margin = 60.0;
        let span_x = (max.x - min.x).max(1.0);
        let span_y = (max.y - min.y).max(1.0);
        self.zoom = ((width - 2.0 * margin) / span_x).min((height - 2.0 * margin) / span_y).clamp(0.1, 1.6);
        let center = point((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
        self.pan = point(width / 2.0 - center.x * self.zoom, height / 2.0 - center.y * self.zoom);
    }

    fn to_screen(&self, p: Point<f32>) -> Point<f32> {
        point(p.x * self.zoom + self.pan.x, p.y * self.zoom + self.pan.y)
    }

    fn local(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self.bounds.get().map(|b| b.origin).unwrap_or_default();
        point(f32::from(position.x - origin.x), f32::from(position.y - origin.y))
    }

    fn node_at(&self, screen: Point<f32>) -> Option<i64> {
        let radius = NODE_RADIUS * self.zoom.max(0.6) + 2.0;
        self.positions.iter().enumerate().rev().find_map(|(ix, p)| {
            let s = self.to_screen(*p);
            ((s.x - screen.x).powi(2) + (s.y - screen.y).powi(2) <= radius * radius).then(|| self.nodes[ix].id)
        })
    }

    fn edge_at(&self, screen: Point<f32>) -> Option<i64> {
        self.edges.iter().find_map(|edge| {
            let a = self.to_screen(self.positions[self.index[&edge.source]]);
            let b = self.to_screen(self.positions[self.index[&edge.target]]);
            (distance_to_segment(screen, a, b) < 5.0).then_some(edge.id)
        })
    }

    /// Add `id`'s neighbors on a ring around it.
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
                        let fresh: Vec<GraphNode> =
                            data.nodes.into_iter().filter(|n| !this.index.contains_key(&n.id)).collect();
                        if this.nodes.len() + fresh.len() > NODE_LIMIT {
                            window.push_notification(
                                Notification::warning(trf("canvas.limit", &[&NODE_LIMIT.to_string()])),
                                cx,
                            );
                        } else {
                            let center = this.index.get(&id).map(|&ix| this.positions[ix]).unwrap_or_default();
                            let ring = 90.0 + fresh.len() as f32 * 7.0;
                            let count = fresh.len().max(1) as f32;
                            for (i, node) in fresh.into_iter().enumerate() {
                                let angle = std::f32::consts::TAU * i as f32 / count;
                                this.add_node(node, Some(point(center.x + ring * angle.cos(), center.y + ring * angle.sin())));
                            }
                            this.add_edges(data.edges);
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

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.local(event.position);
        if let Some(id) = self.node_at(at) {
            self.selected = Some(Selected::Node(id));
            if event.click_count >= 2 {
                self.expand(id, window, cx);
            } else {
                self.drag = Some(Drag::Node { id, last: event.position });
            }
        } else if let Some(id) = self.edge_at(at) {
            self.selected = Some(Selected::Edge(id));
        } else {
            self.selected = None;
            self.drag = Some(Drag::Pan {
                start: event.position,
                origin: self.pan,
            });
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        match drag {
            Drag::Pan { start, origin } => {
                self.pan = point(
                    origin.x + f32::from(event.position.x - start.x),
                    origin.y + f32::from(event.position.y - start.y),
                );
            }
            Drag::Node { id, last } => {
                if let Some(&ix) = self.index.get(&id) {
                    let p = &mut self.positions[ix];
                    p.x += f32::from(event.position.x - last.x) / self.zoom;
                    p.y += f32::from(event.position.y - last.y) / self.zoom;
                }
                self.drag = Some(Drag::Node { id, last: event.position });
            }
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = f32::from(event.delta.pixel_delta(px(16.)).y);
        let at = self.local(event.position);
        let factor = (1.0 + delta / 400.0).clamp(0.5, 2.0);
        let zoom = (self.zoom * factor).clamp(0.05, 4.0);
        // Keep the point under the pointer still.
        let world = point((at.x - self.pan.x) / self.zoom, (at.y - self.pan.y) / self.zoom);
        self.zoom = zoom;
        self.pan = point(at.x - world.x * zoom, at.y - world.y * zoom);
        cx.notify();
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.selected?;
        let (title, color, subtitle, properties) = match selected {
            Selected::Node(id) => {
                let node = &self.nodes[*self.index.get(&id)?];
                let label = node.labels.join(":");
                (
                    format!("{label} #{id}"),
                    Some(palette::label_color(node.labels.first().map(String::as_str).unwrap_or(""), cx)),
                    None,
                    &node.properties,
                )
            }
            Selected::Edge(id) => {
                let edge = self.edges.iter().find(|e| e.id == id)?;
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
                .when(matches!(selected, Selected::Node(_)), |this| {
                    let Selected::Node(id) = selected else { unreachable!() };
                    this.child(
                        Button::new("expand-node")
                            .small()
                            .outline()
                            .icon(IconName::Plus)
                            .label(tr("canvas.expand"))
                            .loading(self.expanding)
                            .on_click(cx.listener(move |this, _, window, cx| this.expand(id, window, cx))),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_fit {
            if let Some(bounds) = self.bounds.get() {
                self.fit(bounds);
                self.needs_fit = false;
            } else {
                cx.notify();
            }
        }
        let theme = cx.theme();
        let edge_color = theme.muted_foreground.opacity(0.7);
        let selected_color = theme.primary;
        let text_color = theme.foreground;
        let halo = theme.background;
        let muted = theme.muted_foreground;
        let border = theme.border;

        let zoom = self.zoom;
        let radius = NODE_RADIUS * zoom.clamp(0.6, 1.6);
        let show_captions = zoom > 0.45 || self.nodes.len() < 60;
        let show_edge_labels = zoom > 0.7 && self.edges.len() <= 300;

        let screen: Vec<Point<f32>> = self.positions.iter().map(|p| self.to_screen(*p)).collect();
        let segments: Vec<(Point<f32>, Point<f32>, bool)> = self
            .edges
            .iter()
            .map(|e| {
                (
                    screen[self.index[&e.source]],
                    screen[self.index[&e.target]],
                    self.selected == Some(Selected::Edge(e.id)),
                )
            })
            .collect();
        let bounds_cell = self.bounds.clone();
        let edges_canvas = canvas(
            move |bounds, _, _| bounds_cell.set(Some(bounds)),
            move |bounds, _, window, _| {
                let origin = bounds.origin;
                for (a, b, selected) in &segments {
                    let color = if *selected { selected_color } else { edge_color };
                    paint_edge(window, origin, *a, *b, radius, *selected, color);
                }
            },
        )
        .absolute()
        .size_full();

        let labels: Vec<AnyElement> = if show_edge_labels {
            self.edges
                .iter()
                .map(|e| {
                    let (a, b) = (screen[self.index[&e.source]], screen[self.index[&e.target]]);
                    div()
                        .absolute()
                        .left(px((a.x + b.x) / 2.0 - 40.0))
                        .top(px((a.y + b.y) / 2.0 - 8.0))
                        .w(px(80.0))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(halo.opacity(0.85))
                                .text_size(px(9.0))
                                .text_color(muted)
                                .child(e.rel_type.clone()),
                        )
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };

        let nodes: Vec<AnyElement> = self
            .nodes
            .iter()
            .zip(&screen)
            .map(|(node, s)| {
                let color = palette::label_color(node.labels.first().map(String::as_str).unwrap_or(""), cx);
                let selected = self.selected == Some(Selected::Node(node.id));
                v_flex()
                    .absolute()
                    .left(px(s.x - 70.0))
                    .top(px(s.y - radius))
                    .w(px(140.0))
                    .items_center()
                    .child(
                        div()
                            .size(px(radius * 2.0))
                            .rounded_full()
                            .bg(color)
                            .border_2()
                            .border_color(if selected { selected_color } else { halo }),
                    )
                    .when(show_captions, |this| {
                        this.child(
                            div()
                                .mt_0p5()
                                .px_1()
                                .rounded_sm()
                                .bg(halo.opacity(0.8))
                                .text_size(px(11.0))
                                .text_color(text_color)
                                .whitespace_nowrap()
                                .child(caption(node)),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let mut legend: Vec<String> = self.nodes.iter().filter_map(|n| n.labels.first().cloned()).collect();
        legend.sort();
        legend.dedup();

        let note = if self.hidden > 0 {
            trf("canvas.truncated", &[&self.nodes.len().to_string(), &(self.nodes.len() + self.hidden).to_string()])
        } else {
            tr("canvas.hint").to_string()
        };

        h_flex()
            .size_full()
            .child(
                div()
                    .id("graph-canvas")
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .bg(theme.secondary.opacity(0.35))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag = None))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(edges_canvas)
                    .children(labels)
                    .children(nodes)
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
                                        if let Some(bounds) = this.bounds.get() {
                                            this.fit(bounds);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("canvas-relayout")
                                    .xsmall()
                                    .outline()
                                    .label(tr("canvas.relayout"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.relayout();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(!legend.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .absolute()
                                .top_2()
                                .right_2()
                                .p_2()
                                .gap_1()
                                .rounded_md()
                                .border_1()
                                .border_color(border)
                                .bg(halo)
                                .text_xs()
                                .children(legend.into_iter().map(|label| {
                                    h_flex()
                                        .gap_1p5()
                                        .items_center()
                                        .child(div().size_2p5().rounded_full().bg(palette::label_color(&label, cx)))
                                        .child(label)
                                })),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .bottom_2()
                            .left_2()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(halo)
                            .text_xs()
                            .text_color(muted)
                            .child(note),
                    ),
            )
            .children(self.render_inspector(cx))
    }
}

fn paint_edge(
    window: &mut Window,
    origin: Point<Pixels>,
    a: Point<f32>,
    b: Point<f32>,
    radius: f32,
    selected: bool,
    color: Hsla,
) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length < radius * 2.0 {
        return;
    }
    let (ux, uy) = (dx / length, dy / length);
    let start = point(a.x + ux * radius, a.y + uy * radius);
    let tip = point(b.x - ux * radius, b.y - uy * radius);
    let at = |p: Point<f32>| point(origin.x + px(p.x), origin.y + px(p.y));

    let mut line = PathBuilder::stroke(px(if selected { 2.4 } else { 1.3 }));
    line.move_to(at(start));
    line.line_to(at(tip));
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
    let size = 7.0;
    let base = point(tip.x - ux * size, tip.y - uy * size);
    let left = point(base.x - uy * size * 0.5, base.y + ux * size * 0.5);
    let right = point(base.x + uy * size * 0.5, base.y - ux * size * 0.5);
    let mut arrow = PathBuilder::fill();
    arrow.move_to(at(tip));
    arrow.line_to(at(left));
    arrow.line_to(at(right));
    arrow.close();
    if let Ok(path) = arrow.build() {
        window.paint_path(path, color);
    }
}

fn distance_to_segment(p: Point<f32>, a: Point<f32>, b: Point<f32>) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length2 = dx * dx + dy * dy;
    if length2 == 0.0 {
        return ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt();
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / length2).clamp(0.0, 1.0);
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()
}

fn iterations_for(count: usize) -> usize {
    match count {
        0..=200 => 300,
        201..=600 => 120,
        _ => 40,
    }
}

/// Fruchterman–Reingold. Deterministic for a given seed layout; nodes marked
/// `fixed` attract and repel others but do not move.
pub fn force_layout(positions: &mut [Point<f32>], links: &[(usize, usize)], fixed: &[bool], iterations: usize) {
    let n = positions.len();
    if n < 2 {
        return;
    }
    let k = 80.0_f32;
    let mut temperature = 10.0 * (n as f32).sqrt();
    let cooling = temperature / iterations.max(1) as f32;
    let mut displacement = vec![point(0.0_f32, 0.0_f32); n];
    for _ in 0..iterations {
        displacement.iter_mut().for_each(|d| *d = point(0.0, 0.0));
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (positions[i].x - positions[j].x, positions[i].y - positions[j].y);
                let d2 = (dx * dx + dy * dy).max(0.01);
                let force = k * k / d2;
                displacement[i].x += dx * force;
                displacement[i].y += dy * force;
                displacement[j].x -= dx * force;
                displacement[j].y -= dy * force;
            }
        }
        for &(a, b) in links {
            if a == b {
                continue;
            }
            let (dx, dy) = (positions[a].x - positions[b].x, positions[a].y - positions[b].y);
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            let force = d / k;
            displacement[a].x -= dx * force;
            displacement[a].y -= dy * force;
            displacement[b].x += dx * force;
            displacement[b].y += dy * force;
        }
        for i in 0..n {
            if fixed[i] {
                continue;
            }
            // A weak pull to the middle keeps disconnected parts nearby.
            displacement[i].x -= positions[i].x * 0.02;
            displacement[i].y -= positions[i].y * 0.02;
            let d = displacement[i];
            let length = (d.x * d.x + d.y * d.y).sqrt().max(0.01);
            let step = length.min(temperature);
            positions[i].x += d.x / length * step;
            positions[i].y += d.y / length * step;
        }
        temperature = (temperature - cooling).max(0.5);
    }
}

#[cfg(test)]
mod tests {
    use super::{distance_to_segment, force_layout};
    use gpui_kit::point;

    #[test]
    fn layout_separates_nodes_and_keeps_links_short() {
        let mut positions: Vec<_> = (0..6).map(|i| point(i as f32, 0.0)).collect();
        let links = [(0, 1), (1, 2), (3, 4), (4, 5)];
        force_layout(&mut positions, &links, &[false; 6], 300);
        let dist = |a: usize, b: usize| ((positions[a].x - positions[b].x).powi(2) + (positions[a].y - positions[b].y).powi(2)).sqrt();
        for i in 0..6 {
            for j in (i + 1)..6 {
                assert!(dist(i, j) > 20.0, "{i} and {j} overlap: {positions:?}");
                assert!(positions[i].x.is_finite() && positions[i].y.is_finite());
            }
        }
        assert!(dist(0, 1) < dist(0, 5) * 1.5);
    }

    #[test]
    fn segment_distance() {
        assert_eq!(distance_to_segment(point(0.0, 5.0), point(-10.0, 0.0), point(10.0, 0.0)), 5.0);
        assert_eq!(distance_to_segment(point(13.0, 4.0), point(-10.0, 0.0), point(10.0, 0.0)), 5.0);
    }
}

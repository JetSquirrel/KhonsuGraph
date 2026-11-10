//! Putting a result on the canvas, and adding to it: missing endpoints and
//! neighbors on a double-click.

use super::*;

impl GraphCanvas {
    /// Show a new result. Relationship endpoints the result left out are
    /// fetched so every edge has both ends on the canvas.
    pub fn set_data(&mut self, data: GraphData, window: &mut Window, cx: &mut Context<Self>) {
        self.explorer.forget_walks();
        self.discover_analyses(window, cx);
        self.set_path_view(false, cx);
        self.nodes.clear();
        self.edges.clear();
        self.color_slots.clear();
        self.size_range = None;
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

    pub(super) fn load(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &mut Context<Self>) {
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
    pub(super) fn admit(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &App) -> Graph {
        palette::register_labels(nodes.iter().map(first_label).filter(|l| !l.is_empty()));
        self.note_styled(&nodes);
        let mut fresh_nodes = Vec::new();
        for node in nodes {
            if !self.nodes.contains_key(&node.id) {
                fresh_nodes.push(to_node(&node, &self.look(), cx));
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

    pub(super) fn on_graph_event(
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
    pub(super) fn expand(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn load_relationship_types(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}

//! Walks, paths, plugin analyses and saved investigations.

use super::*;

impl GraphCanvas {
    /// By ID, so that a view comes back in the order it would have anyway.
    pub(super) fn view(&self) -> View {
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

    pub(super) fn selected_node(&self) -> Option<i64> {
        match self.selected {
            Some(Selected::Node(id)) => Some(id),
            _ => None,
        }
    }

    /// Walk from the chosen starts and add what it finds to the canvas. A walk
    /// that would not fit adds nothing and says so, as expanding does.
    pub(super) fn explore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn show_walk(&mut self, walk: Exploration, anchor: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    pub fn set_query(&mut self, query: Option<SavedQuery>) {
        self.query = query;
    }

    /// Save the walk, filters and chosen path to a file the person picks. The
    /// graph is not touched.
    pub(super) fn save_investigation(&mut self, node: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut request) = self.explorer.request(Some(node)) else {
            return;
        };
        // After a walk, its starts and the path to this node are what is on
        // screen; the selection only says which path.
        let walk = self.explorer.walk.as_ref();
        let target = walk.filter(|walk| walk.path_to(node).is_some_and(|path| !path.is_empty())).map(|_| node);
        if let Some(walk) = walk.filter(|walk| !walk.starts.is_empty()) {
            request.starts = walk.starts.clone();
        }
        let path_refs = self.explorer.exact_path_to(node);
        let query = self.query.clone();
        let suggested = format!("investigation.{EXTENSION}");
        let directory = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf()).unwrap_or_else(std::env::temp_dir);
        let rx = cx.prompt_for_new_path(&directory, Some(&suggested));
        window
            .spawn(cx, async move |cx| {
                let Ok(Ok(Some(path))) = rx.await else {
                    return;
                };
                let written = smol::unblock({
                    let path = path.clone();
                    move || {
                        let capture = Capture {
                            name: path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default(),
                            query,
                            walk: Some(request),
                            target,
                            path_refs,
                            dashboard: None,
                        };
                        let saved = crate::db::capture_investigation(capture).map_err(|error| error.message)?;
                        // The dialog has already asked about replacing a file.
                        saved.save(&path, true).map_err(|error| error.message)
                    }
                })
                .await;
                cx.update(|window, cx| match written {
                    Ok(()) => window.push_notification(Notification::success(trf("investigation.saved", &[&path.display().to_string()])), cx),
                    Err(error) => window.push_notification(Notification::error(trf("investigation.save_failed", &[&error])), cx),
                })
                .ok();
            })
            .detach();
    }

    /// Show a reopened investigation: its filters, walk and chosen path, and
    /// everything that differs from when it was saved.
    pub fn restore_investigation(&mut self, saved: &Investigation, reopened: Reopened, window: &mut Window, cx: &mut Context<Self>) {
        let graph = reopened.walk.as_ref().map(|walk| walk.graph.clone()).unwrap_or_default();
        self.set_data(graph, window, cx);
        self.query = saved.query.clone();
        if let Some(plan) = &saved.walk {
            self.explorer.direction = plan.direction;
            self.explorer.types = plan.types.iter().cloned().collect();
            self.explorer.max_hops = plan.max_hops.clamp(1, khonsu_core::explore::MAX_HOPS);
        }
        self.explorer.restored = Some(explorer::Restored::new(saved, &reopened));
        self.explorer.walk = reopened.walk;
        self.load_relationship_types(window, cx);
        if reopened.exact_path {
            // The canvas already holds that path and nothing else.
            self.set_path_view(true, cx);
        } else if let Some(target) = reopened.target.filter(|_| reopened.path.is_some()) {
            self.show_path(target, cx);
        }
        cx.notify();
    }

    /// Ask the plugins what they offer. A plugin not approved as it is now is
    /// not started to find out.
    pub(super) fn discover_analyses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let analyses = smol::unblock(khonsu_core::plugin_analysis::offered).await;
            this.update(cx, |this, cx| {
                this.explorer.analyses = analyses;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run an analysis over the whole graph, not just the canvas, or stop it if
    /// it is running.
    pub(super) fn check_analysis(&mut self, offered: Offered, window: &mut Window, cx: &mut Context<Self>) {
        let key = explorer::analysis_key(&offered);
        if let Some(cancel) = self.explorer.checking.remove(&key) {
            cancel.store(true, Ordering::Relaxed);
            cx.notify();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.explorer.checking.insert(key.clone(), cancel.clone());
        self.explorer.analysis_errors.remove(&key);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let stop = cancel.clone();
            let result = smol::unblock(move || crate::db::run_analysis(&offered, &stop)).await;
            this.update(cx, |this, cx| {
                // Stopped, or the canvas moved on: this says nothing about it now.
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                this.explorer.checking.remove(&key);
                match result {
                    Ok(Some(report)) => {
                        this.explorer.reports.insert(key, report);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.explorer.analysis_errors.insert(key, error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show a finding's candidate path, as a walk Path Explorer can go on from.
    pub(super) fn show_finding(&mut self, key: &str, finding_id: &str, target: i64, cx: &mut Context<Self>) {
        let report = self.explorer.reports.get(key);
        let finding = report.and_then(|report| report.findings.iter().find(|finding| finding.finding_id == finding_id));
        let hint = finding.and_then(|finding| finding.walk.clone());
        let saved_as = finding.and_then(|finding| Some((finding.hops.clone(), finding.path_refs.clone()?)));
        let Some(walk) = finding.and_then(|finding| finding.exploration()) else {
            return;
        };
        self.explorer.push_view(self.view());
        self.replace_canvas(walk.graph.nodes.clone(), walk.graph.edges.clone(), cx);
        self.explorer.walk = Some(walk);
        self.explorer.finding_path = saved_as;
        // The filters that find this path again, so that saving or walking on
        // from here means the same thing.
        if let Some(hint) = hint {
            self.explorer.direction = hint.direction;
            self.explorer.types = hint.types.into_iter().collect();
            self.explorer.max_hops = hint.max_hops.clamp(1, khonsu_core::explore::MAX_HOPS);
        }
        self.set_path_view(true, cx);
        let id = target.to_string();
        self.graph.update(cx, |graph, cx| graph.select_node(Some(&id), false, cx));
    }

    /// Narrow the canvas to the path the last walk took to `target`.
    pub(super) fn show_path(&mut self, target: i64, cx: &mut Context<Self>) {
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
    pub(super) fn go_back(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn set_path_view(&mut self, on: bool, cx: &mut Context<Self>) {
        self.path_view = on;
        let dimming = self.focus_dimming && !on;
        self.graph.update(cx, |graph, cx| graph.set_focus_dimming(dimming, cx));
    }

    pub(super) fn replace_canvas(&mut self, nodes: Vec<GraphNode>, edges: Vec<GraphEdge>, cx: &mut Context<Self>) {
        self.nodes.clear();
        self.edges.clear();
        self.selected = None;
        self.hidden = 0;
        self.load(nodes, edges, cx);
    }
}

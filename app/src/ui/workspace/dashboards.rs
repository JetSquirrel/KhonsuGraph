//! Dashboard tabs: opening a `.dash` spec, agents' presence on it, saving
//! it, and the snapshot agents read.

use super::*;

impl Workspace {
    pub(super) fn push_dashboard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = crate::spec::tabs::title_for(&path);
        let host = cx.new(|cx| Dashboard::new(id, path.clone(), cx));
        let events = cx.subscribe_in(&host, window, |this, _, event, window, cx| match event {
            DashboardEvent::OpenSql { sql } => {
                // A dashboard query opened for editing: Cypher unless it
                // reads like SQL.
                let language = if sql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
                    Language::Sql
                } else {
                    Language::Cypher
                };
                this.add_tab(language, Some(sql), window, cx);
            }
        });
        self.tabs.push(WorkspaceTab::Dashboard(DashboardTab {
            id,
            title: title.into(),
            path,
            host,
            _events: events,
        }));
    }

    /// Open a `.dash` spec as a tab, or bring its tab forward when one is
    /// open already.
    pub fn open_dashboard(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // A graph opened just before, by `khonsu open GRAPH DASH` or at startup,
        // is still opening: wait for it, or the dashboard would be dropped for
        // want of a graph that is about to be there.
        if self.state.read(cx).is_opening() {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                this.update_in(cx, |this, window, cx| this.open_dashboard(path, window, cx)).ok();
            })
            .detach();
            return;
        }
        if self.state.read(cx).graph.is_none() {
            window.push_notification(Notification::error(tr("dashboard.needs_graph")), cx);
            return;
        }
        let path = std::path::absolute(&path).unwrap_or(path);
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| tab.as_dashboard().is_some_and(|d| same_file(&d.path, &path)))
        {
            self.activate_tab(ix, window, cx);
            return;
        }
        self.leave_home(cx);
        self.push_dashboard(path, window, cx);
        self.active = self.tabs.len() - 1;
        self.remember_tabs(cx);
        cx.notify();
    }

    pub fn pick_dashboard_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("dashboard.picker.prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if crate::spec::tabs::is_spec(&path) {
                    this.open_dashboard(path, window, cx);
                } else {
                    window.push_notification(trf("dashboard.picker.not_a_spec", &[&path.to_string_lossy()]), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// An agent's presence on a spec reaches every tab showing it.
    pub fn apply_presence(&mut self, update: &crate::spec::presence::Update, cx: &mut Context<Self>) {
        let path = PathBuf::from(&update.path);
        for tab in &self.tabs {
            if let WorkspaceTab::Dashboard(tab) = tab {
                if same_file(&tab.path, &path) {
                    tab.host.update(cx, |dashboard, cx| dashboard.apply_presence(update, cx));
                }
            }
        }
        cx.notify();
    }

    pub(super) fn save_active_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.tabs.get(self.active).and_then(WorkspaceTab::as_dashboard).map(|t| t.host.clone()) else {
            return;
        };
        if let Err(error) = host.update(cx, |dashboard, cx| dashboard.save(cx)) {
            window.push_notification(Notification::error(error), cx);
        }
    }

    /// What the window shows, for `khonsu open --state`: each tab's query and
    /// result summary, or its dashboard's.
    pub fn snapshot(&self, only: Option<&std::path::Path>, active_only: bool, cx: &App) -> serde_json::Value {
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, _)| !active_only || *ix == self.active)
            .filter(|(_, tab)| match (only, tab) {
                (Some(path), WorkspaceTab::Dashboard(d)) => same_file(&d.path, path),
                (Some(_), WorkspaceTab::Query(_)) => false,
                (None, _) => true,
            })
            .map(|(ix, tab)| match tab {
                WorkspaceTab::Query(t) => serde_json::json!({
                    "kind": "query",
                    "active": ix == self.active,
                    "language": t.language,
                    "text": t.editor.read(cx).value().to_string(),
                    "result": t.results.read(cx).summary(),
                }),
                WorkspaceTab::Dashboard(t) => serde_json::json!({
                    "kind": "dashboard",
                    "active": ix == self.active,
                    "path": t.path.display().to_string(),
                    "dashboard": t.host.read(cx).summary(),
                }),
            })
            .collect();
        serde_json::json!({ "home": self.state.read(cx).showing_home(), "tabs": tabs })
    }
}

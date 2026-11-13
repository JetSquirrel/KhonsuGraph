//! Query tabs: making, restoring and remembering them per graph, and
//! moving between them.

use super::*;

/// Tabs as remembered between launches, per graph: a query's text, or the
/// path of a dashboard spec.
#[derive(Serialize, Deserialize)]
struct SavedTab {
    #[serde(default = "cypher")]
    language: Language,
    #[serde(default)]
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dashboard: Option<PathBuf>,
}

fn cypher() -> Language {
    Language::Cypher
}

fn tabs_key(graph: &std::path::Path) -> String {
    format!("tabs:{}", graph.display())
}

impl Workspace {
    pub(super) fn new_tab(&mut self, language: Language, text: &str, window: &mut Window, cx: &mut Context<Self>) -> QueryTab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let number = self.tabs.iter().filter_map(WorkspaceTab::as_query).map(|tab| tab.number).max().unwrap_or(0) + 1;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                // HawDB's Cypher shares most of its keywords with SQL; the SQL
                // grammar is the closest highlighting the toolkit ships.
                .language("sql")
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
        });
        editor.update(cx, |editor, cx| editor.set_value(text.to_string(), window, cx));
        // ⌘↵ inside the editor arrives as PressEnter (see DuckLocal): undo the
        // newline it inserted, then run.
        let subscription = cx.subscribe_in(&editor, window, |this, editor, event, window, cx| {
            match event {
                InputEvent::Change => {
                    this.remember_tabs(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { secondary: true, .. } => {
                    remove_secondary_enter_newline(editor, window, cx);
                    this.run_active(window, cx);
                }
                _ => {}
            }
        });
        self._subscriptions.push(subscription);
        let results = cx.new(|cx| ResultsPanel::new(self.state.clone(), window, cx));
        QueryTab {
            id,
            number,
            language,
            editor,
            results,
        }
    }

    /// The open graph changed: bring back its tabs, or start one. The last
    /// graph's were remembered as they changed and come back when it is
    /// opened again; a switch says so, since they leave the window.
    pub(super) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.state.read(cx).graph.as_ref().map(|g| g.path.clone());
        if let (Some(previous), Some(_)) = (self.tabs_graph.as_ref().filter(|p| Some(*p) != next.as_ref()), &next) {
            let kept = self
                .tabs
                .iter()
                .filter(|t| match t {
                    WorkspaceTab::Query(q) => is_work(q.language, &q.editor.read(cx).value()),
                    WorkspaceTab::Dashboard(_) => true,
                })
                .count();
            if kept > 0 {
                let name = previous.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                window.push_notification(trf("workspace.tabs_kept", &[&name, &kept.to_string()]), cx);
            }
        }
        self.tabs.clear();
        self.active = 0;
        self.tabs_graph = next;
        let Some(graph) = self.state.read(cx).graph.clone() else {
            cx.notify();
            return;
        };
        let saved: Vec<SavedTab> = crate::prefs::get(&tabs_key(&graph.path))
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        // Nothing to come back to: start from what's in the graph.
        let home = !saved.iter().any(|tab: &SavedTab| tab.dashboard.is_some() || is_work(tab.language, &tab.text));
        self.state.update(cx, |state, cx| state.set_home(home, cx));
        let mut problems = Vec::new();
        for saved in saved {
            match saved.dashboard {
                Some(path) if path.is_file() => self.push_dashboard(path, window, cx),
                Some(path) => problems.push(path.display().to_string()),
                None => {
                    let tab = self.new_tab(saved.language, &saved.text, window, cx);
                    self.tabs.push(WorkspaceTab::Query(tab));
                }
            }
        }
        if !self.tabs.iter().any(|t| t.as_query().is_some()) {
            let tab = self.new_tab(Language::Cypher, default_text(Language::Cypher), window, cx);
            self.tabs.insert(0, WorkspaceTab::Query(tab));
        }
        self.active = 0;
        // A remembered dashboard that is gone is named rather than dropped
        // silently.
        for path in problems {
            window.push_notification(trf("dashboard.restore.gone_path", &[&path]), cx);
        }
        if !home {
            self.focus_active_editor(window, cx);
        }
        cx.notify();
    }

    pub(super) fn remember_tabs(&self, cx: &App) {
        let Some(graph) = self.tabs_graph.as_ref() else {
            return;
        };
        let saved: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| match t {
                WorkspaceTab::Query(t) => SavedTab {
                    language: t.language,
                    text: t.editor.read(cx).value().to_string(),
                    dashboard: None,
                },
                WorkspaceTab::Dashboard(t) => SavedTab {
                    language: Language::Cypher,
                    text: String::new(),
                    dashboard: Some(t.path.clone()),
                },
            })
            .collect();
        if let Ok(json) = serde_json::to_string(&saved) {
            let _ = crate::prefs::set(&tabs_key(graph), &json);
        }
    }

    pub fn add_tab(&mut self, language: Language, text: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).graph.is_none() {
            return;
        }
        self.leave_home(cx);
        let tab = self.new_tab(language, text.unwrap_or(""), window, cx);
        self.tabs.push(WorkspaceTab::Query(tab));
        self.active = self.tabs.len() - 1;
        self.remember_tabs(cx);
        self.focus_active_editor(window, cx);
        cx.notify();
    }

    /// A new tab with the saved query in its editor, not run, and the reopened
    /// walk on its canvas.
    pub fn show_investigation(&mut self, saved: &Investigation, reopened: Reopened, window: &mut Window, cx: &mut Context<Self>) {
        let (language, text) = match &saved.query {
            Some(query) if query.language == "sql" => (Language::Sql, query.text.as_str()),
            Some(query) => (Language::Cypher, query.text.as_str()),
            None => (Language::Cypher, ""),
        };
        self.add_tab(language, Some(text), window, cx);
        if let Some(WorkspaceTab::Query(tab)) = self.tabs.last() {
            tab.results.clone().update(cx, |results, cx| results.show_investigation(saved, reopened, window, cx));
        }
    }
}

impl Workspace {
    /// Put `text` in the active tab when it is empty and of the same
    /// language, otherwise in a new tab. Used by the sidebar and history.
    pub fn insert_query(&mut self, language: Language, text: &str, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_home(cx);
        let reuse = self
            .tabs
            .get(self.active)
            .and_then(WorkspaceTab::as_query)
            .filter(|t| t.language == language && t.editor.read(cx).value().trim().is_empty())
            .map(|t| t.editor.clone());
        match reuse {
            Some(editor) => editor.update(cx, |e, cx| e.set_value(text.to_string(), window, cx)),
            None => self.add_tab(language, Some(text), window, cx),
        }
        self.remember_tabs(cx);
        if run {
            self.run_active(window, cx);
        }
        cx.notify();
    }

    pub fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        // The last query tab stays: a dashboard alone has nowhere to send a query.
        let queries = self.tabs.iter().filter(|t| t.as_query().is_some()).count();
        let closing_query = self.tabs.iter().any(|t| t.id() == id && t.as_query().is_some());
        if self.tabs.len() <= 1 || (closing_query && queries <= 1) {
            return;
        }
        if let Some(ix) = self.tabs.iter().position(|t| t.id() == id) {
            self.tabs.remove(ix);
            if self.active >= self.tabs.len() || self.active > ix {
                self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
            }
            self.remember_tabs(cx);
            self.focus_active_editor(window, cx);
            cx.notify();
        }
    }

    pub fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(self.active).map(WorkspaceTab::id) {
            self.close_tab(id, window, cx);
        }
    }

    pub fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.leave_home(cx);
        self.active = index.min(self.tabs.len() - 1);
        self.focus_active_editor(window, cx);
        cx.notify();
    }

    pub fn cycle_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n == 0 {
            return;
        }
        let next = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        self.activate_tab(next, window, cx);
    }

    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut App) {
        if let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) {
            tab.editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
    }
}

//! Running and explaining the active tab's query.

use super::*;

/// Statements that may change labels, types or tables: the sidebar reloads
/// after them.
const WRITE_WORDS: &[&str] = &["CREATE", "MERGE", "DELETE", "SET", "INSERT", "DROP", "ALTER", "UPDATE", "REMOVE"];

impl Workspace {
    pub fn run_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.state.read(cx).is_opening() {
            return;
        }
        // Whatever asked for this should see the result.
        self.leave_home(cx);
        let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) else {
            return;
        };
        let text = selected_or_all(&tab.editor, cx);
        if text.trim().is_empty() {
            return;
        }
        let language = tab.language;
        let results = tab.results.clone();
        self.running = true;
        let saved_query = SavedQuery {
            language: if language == Language::Sql { "sql" } else { "cypher" }.into(),
            text: text.trim().to_string(),
        };
        results.update(cx, |r, cx| {
            r.set_query(Some(saved_query), cx);
            r.set_running(cx);
        });
        cx.notify();
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let run_text = text.clone();
            let (result, graph) = smol::unblock(move || {
                let result = crate::db::run(language, &run_text);
                let graph = crate::db::with_graph(|g| Ok(g.path().to_path_buf())).ok();
                if let Some(graph) = &graph {
                    let _ = settings::push_history(HistoryEntry {
                        language,
                        text: run_text.trim().to_string(),
                        graph: graph.clone(),
                        at: util::unix_now(),
                        ok: result.is_ok(),
                    });
                }
                (result, graph)
            })
            .await;
            let _ = graph;
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                let writes = changes_schema(&text);
                match result {
                    Ok(result) => {
                        let stats = QueryStats {
                            elapsed_ms: result.elapsed_ms,
                            rows: result.row_count,
                            nodes: result.graph.as_ref().map_or(0, |g| g.nodes.len()),
                            edges: result.graph.as_ref().map_or(0, |g| g.edges.len()),
                        };
                        let is_plan = result.columns == ["plan"] && text.trim_start().to_uppercase().starts_with("EXPLAIN");
                        results.update(cx, |r, cx| {
                            if is_plan {
                                let plan = result.rows.iter().filter_map(|r| r[0].as_str()).collect::<Vec<_>>().join("\n");
                                r.set_plan(plan, cx);
                            } else {
                                r.set_result(result, window, cx);
                            }
                        });
                        state.update(cx, |s, cx| s.set_last_query(Some(stats), cx));
                    }
                    Err(error) => {
                        results.update(cx, |r, cx| r.set_error(error, cx));
                        state.update(cx, |s, cx| s.set_last_query(None, cx));
                    }
                }
                state.update(cx, |s, cx| s.reload_lists(cx));
                if writes {
                    crate::ui::reload_schema(state.clone(), window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn explain_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.state.read(cx).is_opening() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active).and_then(WorkspaceTab::as_query) else {
            return;
        };
        let text = selected_or_all(&tab.editor, cx);
        if text.trim().is_empty() {
            return;
        }
        let language = tab.language;
        let results = tab.results.clone();
        self.running = true;
        results.update(cx, |r, cx| r.set_running(cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let plan = smol::unblock(move || crate::db::explain(language, &text)).await;
            this.update(cx, |this, cx| {
                this.running = false;
                results.update(cx, |r, cx| match plan {
                    Ok(plan) => r.set_plan(plan, cx),
                    Err(e) => r.set_error(e, cx),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

pub(super) fn selected_or_all(editor: &Entity<EditorState>, cx: &App) -> String {
    let editor = editor.read(cx);
    let selected = editor.selected_text().to_string();
    if selected.trim().is_empty() {
        editor.value().to_string()
    } else {
        selected
    }
}

pub(super) fn changes_schema(text: &str) -> bool {
    let upper = text.to_uppercase();
    upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| WRITE_WORDS.contains(&word))
}

/// See DuckLocal: ⌘↵ in the editor first inserts a newline; take it back.
pub(super) fn remove_secondary_enter_newline(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let range = editor.update(cx, |editor, _| {
        let cursor = editor.cursor();
        let text = editor.text();
        let mut start = cursor;
        let mut chars = text.chars_at(cursor);
        let mut prev = chars.prev();
        while matches!(prev, Some(' ' | '\t')) {
            start -= 1;
            prev = chars.prev();
        }
        if prev != Some('\n') {
            return None;
        }
        Some(start - 1..cursor)
    });
    if let Some(range) = range {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace("", window, cx);
        });
    }
}

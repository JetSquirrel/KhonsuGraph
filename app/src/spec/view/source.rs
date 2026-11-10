//! The spec's source view: the editor, saving, and watching the file for edits
//! made elsewhere.

use super::*;

impl Dashboard {
    pub fn is_showing_source(&self) -> bool {
        self.showing_source
    }

    /// The buffer holds edits the file does not: the editor differs from the
    /// text as last read from (or written to) disk.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match (&self.source_editor, &self.source) {
            (Some(editor), Some(Ok(text))) => editor.read(cx).value().as_str() != text.as_str(),
            _ => false,
        }
    }

    /// Swap the body between the plots and the spec's source. Opening the
    /// source re-reads the file — unless the buffer holds unsaved edits,
    /// which a view switch must never throw away.
    pub fn toggle_source(&mut self, cx: &mut Context<Self>) {
        self.showing_source = !self.showing_source;
        if self.showing_source && !self.is_dirty(cx) {
            self.read_source();
        }
        cx.notify();
    }

    /// Write the editor's text back to the spec file and reload. A save whose
    /// spec no longer validates keeps the last working dashboard up, per the
    /// reload rule. Returns why the write itself failed, for a notification.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let (Some(editor), Some(Ok(_))) = (&self.source_editor, &self.source) else {
            return Ok(());
        };
        let text = editor.read(cx).value().to_string();
        std::fs::write(&self.path, &text).map_err(|error| {
            trf(
                "dashboard.save_failed",
                &[&self.path.to_string_lossy(), &error.to_string()],
            )
        })?;
        self.source_version += 1;
        self.source = Some(Ok(text));
        // The editor already holds what was written: mark the versions in
        // sync rather than pushing the text back and resetting the cursor.
        self.editor_version = self.source_version;
        self.known_stamp = FileStamp::capture(&self.path);
        self.conflict = false;
        self.reload(cx);
        Ok(())
    }

    /// Poll the spec file and reload when an external change settles. A dirty
    /// buffer is never overwritten: the change is a conflict banner instead,
    /// and our own saves are filtered out by their stamp.
    pub(super) fn watch(&mut self, cx: &mut Context<Self>) {
        let watched = self.path.clone();
        let comments_path = comments::path_for(&self.path);
        let watcher = cx.spawn(async move |this, cx| {
            let mut stamp = FileStamp::capture(&watched);
            let mut debounce = Debounce::new();
            // The comments file is followed too, so an agent's reply shows
            // on its plot without a reload.
            let mut comments_stamp = FileStamp::capture(&comments_path);
            let mut comments_debounce = Debounce::new();
            loop {
                smol::Timer::after(POLL_INTERVAL).await;
                let next = FileStamp::capture(&comments_path);
                let changed = next != comments_stamp;
                comments_stamp = next;
                if comments_debounce.observe(Instant::now(), changed)
                    && this
                        .update(cx, |this, cx| {
                            this.load_comments();
                            cx.notify();
                        })
                        .is_err()
                {
                    break;
                }
                let next = FileStamp::capture(&watched);
                let changed = next != stamp;
                stamp = next.clone();
                if !debounce.observe(Instant::now(), changed) {
                    continue;
                }
                if this
                    .update(cx, |this, cx| this.external_changed(next, cx))
                    .is_err()
                {
                    // The tab is gone, so there is nothing to reload into.
                    break;
                }
            }
        });
        self.watcher = Some(watcher);
    }

    /// The file moved on disk. A clean buffer just follows it; a dirty one is
    /// told about the conflict and keeps its edits.
    pub(super) fn external_changed(&mut self, stamp: FileStamp, cx: &mut Context<Self>) {
        if stamp == self.known_stamp {
            // Our own save, coming back around the poll.
            return;
        }
        self.known_stamp = stamp;
        if self.is_dirty(cx) {
            self.conflict = true;
            cx.notify();
            return;
        }
        self.conflict = false;
        self.read_source();
        self.external_reload = true;
        self.reload(cx);
    }

    /// Read the spec file for the source view; a failure is kept as the
    /// message the view shows instead of the source.
    pub(super) fn read_source(&mut self) {
        self.source_version += 1;
        self.source = Some(std::fs::read_to_string(&self.path).map_err(|error| {
            trf(
                "dashboard.source.unreadable",
                &[&self.path.to_string_lossy(), &error.to_string()],
            )
        }));
    }

    /// The spec itself, in the same editor the query tabs use — editable,
    /// with completion, and never reset under the user's hands: a re-read
    /// pushes to it only when the text actually differs. A file that cannot
    /// be read shows its error here rather than replacing the tab.
    pub(super) fn render_source(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let body = match &self.source {
            Some(Ok(_)) => {
                if self.source_editor.is_none() {
                    self.source_editor = Some(cx.new(|cx| {
                        let mut state = EditorState::new(window, cx)
                            .language(crate::spec::highlight::LANGUAGE)
                            .line_number(true)
                            .tab_size(TabSize {
                                tab_size: 2,
                                hard_tabs: false,
                            });
                        state.lsp_mut().completion_provider = Some(Rc::new(SpecCompletionProvider));
                        // Installed before the editor first renders, so the
                        // component's tree-sitter factory — which has no
                        // `.dash` grammar — never takes the slot.
                        state.set_highlighter_factory(crate::spec::highlight::factory(), cx);
                        state
                    }));
                }
                let editor = self.source_editor.clone().expect("built just above");
                if self.editor_version != self.source_version {
                    let text = match &self.source {
                        Some(Ok(text)) => text.clone(),
                        _ => unreachable!("matched above"),
                    };
                    // Pushing back what the editor already shows would only
                    // reset its cursor — after a save, that is exactly the
                    // text on disk.
                    if editor.read(cx).value().as_str() != text.as_str() {
                        editor.update(cx, |state, cx| state.set_value(text, window, cx));
                    }
                    self.editor_version = self.source_version;
                }
                div()
                    .size_full()
                    .child(Editor::new(&editor).h(relative(1.)).bordered(false))
                    .into_any_element()
            }
            Some(Err(message)) => div()
                .size_full()
                .p_3()
                .child(
                    Alert::error("dashboard-source-error", message.clone())
                        .title(tr("dashboard.spec_failed")),
                )
                .into_any_element(),
            // toggle_source reads before flipping, so this is one frame at most.
            None => self.render_loading(cx),
        };
        v_flex().size_full().child(body).into_any_element()
    }

    /// The file changed on disk while the buffer holds unsaved edits: said
    /// out loud, above whichever view is up, until a save resolves it.
    pub(super) fn render_conflict_warning(&self, cx: &App) -> Option<impl IntoElement> {
        if !self.conflict {
            return None;
        }
        Some(
            v_flex()
                .flex_none()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().warning.alpha(0.12))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr("dashboard.conflict")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr("dashboard.conflict.hint")),
                ),
        )
    }
}

/// Spec completion for the source editor: a thin gpui shell over
/// [`complete::complete`], wired exactly like the SQL provider — the rope
/// gives the prefix and the replace range, the pure function gives the
/// candidates.
pub(super) struct SpecCompletionProvider;

/// Don't pop the menu up before an identifier character or a `.` exists.
pub(super) const MAX_PREFIX_SCAN: usize = 64;

impl CompletionProvider for SpecCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        // Identifier prefix immediately before the cursor, as in the SQL
        // provider; after `query.` it is empty and every name matches.
        let mut prefix_len = 0usize;
        while prefix_len < MAX_PREFIX_SCAN {
            let pos = offset.saturating_sub(prefix_len + 1);
            match text.get_char(pos).ok() {
                Some(c) if c.is_alphanumeric() || c == '_' => prefix_len += 1,
                _ => break,
            }
            if pos == 0 {
                break;
            }
        }
        let start = offset - prefix_len;
        let prefix_lower = text.slice(start..offset).to_string().to_lowercase();

        // Columns count characters here and in `complete` alike: the rope's
        // own position conversion keeps the two sides agreeing.
        let position = text.offset_to_position(offset);
        let candidates = complete::complete(
            &text.to_string(),
            position.line as usize + 1,
            position.character as usize + 1,
        );

        let replace_range = lsp_types::Range {
            start: text.offset_to_position(start),
            end: position,
        };
        let items: Vec<CompletionItem> = candidates
            .into_iter()
            .filter(|candidate| starts_with_ignore_case(&candidate.label, &prefix_lower))
            .enumerate()
            .map(|(index, candidate)| {
                let kind = match candidate.kind {
                    CompletionKind::Block => CompletionItemKind::KEYWORD,
                    CompletionKind::Attribute => CompletionItemKind::PROPERTY,
                    CompletionKind::Value => CompletionItemKind::ENUM_MEMBER,
                    CompletionKind::Reference => CompletionItemKind::REFERENCE,
                };
                CompletionItem {
                    label: candidate.label,
                    kind: Some(kind),
                    detail: candidate.detail,
                    sort_text: Some(format!("{index:02}")),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: replace_range,
                        new_text: candidate.insert_text,
                    })),
                    ..Default::default()
                }
            })
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .map(|c| c.is_alphanumeric() || c == '_' || c == '.')
            .unwrap_or(false)
    }
}

//! Data sources: choosing a plugin, confirming the permissions it declares,
//! configuring it from its own `describe`, testing, previewing and importing,
//! and syncing configured sources with progress and cancel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::plugin::{self, PluginInfo, Preview, SyncEvent, SyncReport};
use khonsu_core::protocol::{ConfigField, Describe, FieldKind, Permissions, Request, SyncMode, PROTOCOL_VERSION};
use khonsu_core::settings::{self, Source};
use khonsu_core::secrets;
use serde_json::{Map, Value};

use crate::i18n::{tr, trf};
use crate::state::AppState;

const DIALOG_WIDTH: f32 = 640.;

/// Set to stop the running sync or preview; the plugin process is killed.
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// The prompt an empty graph shows where results would be.
pub fn empty_graph_prompt(state: Entity<AppState>, cx: &App) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_3()
        .child(img("brand/logo.png").size(px(64.)).rounded_lg())
        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(tr("empty_graph.title")))
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr("empty_graph.body")),
        )
        .child(
            Button::new("empty-add-source")
                .primary()
                .icon(IconName::Plus)
                .label(tr("sources.add"))
                .on_click(move |_, window, cx| open_add(state.clone(), window, cx)),
        )
}

fn permission_lines(permissions: &Permissions) -> Vec<String> {
    let mut lines = Vec::new();
    if !permissions.network.is_empty() {
        lines.push(trf("permissions.network", &[&permissions.network.join(", ")]));
    }
    if permissions.read_files {
        lines.push(tr("permissions.read_files").to_string());
    }
    if !permissions.commands.is_empty() {
        lines.push(trf("permissions.commands", &[&permissions.commands.join(", ")]));
    }
    lines.extend(permissions.notes.iter().cloned());
    if lines.is_empty() {
        lines.push(tr("permissions.none").to_string());
    }
    lines
}

fn approved(info: &PluginInfo) -> bool {
    info.builtin || settings::is_approved(info.id(), &info.manifest.version, &info.manifest.permissions)
}

/// Pick a plugin, then configure a new source with it.
pub fn open_add(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let plugins = smol::unblock(|| {
                plugin::list()
                    .into_iter()
                    .map(|info| {
                        let describe = plugin::describe(&info).map_err(|e| e.to_string());
                        (info, describe)
                    })
                    .collect::<Vec<_>>()
            })
            .await;
            cx.update(move |window, cx| show_plugin_picker(state, plugins, window, cx)).ok();
        })
        .detach();
}

fn show_plugin_picker(
    state: Entity<AppState>,
    plugins: Vec<(PluginInfo, Result<Describe, String>)>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let cards = plugins.iter().enumerate().map(|(ix, (info, describe))| {
            let state = state.clone();
            let info_for_click = info.clone();
            let describe_for_click = describe.clone().ok();
            let disabled = describe.is_err();
            v_flex()
                .id(("plugin-card", ix))
                .w(px(190.))
                .p_3()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .when(!disabled, |this| this.cursor_pointer().hover(|s| s.border_color(cx.theme().primary)))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(info.manifest.name.clone()))
                        .when(info.builtin, |this| {
                            this.child(div().text_xs().px_1().rounded_sm().bg(cx.theme().secondary).child(tr("plugins.builtin")))
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(match describe {
                            Ok(d) => d.description.clone(),
                            Err(e) => e.clone(),
                        }),
                )
                .when(!disabled, |this| {
                    this.on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        if let Some(describe) = describe_for_click.clone() {
                            begin_configure(state.clone(), info_for_click.clone(), describe, None, window, cx);
                        }
                    })
                })
        });
        dialog
            .title(tr("sources.choose_plugin"))
            .w(crate::ui::scale::design(DIALOG_WIDTH))
            .child(h_flex().flex_wrap().gap_2().children(cards))
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(
                        Button::new("install-plugin")
                            .outline()
                            .label(tr("plugins.install"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                install_from_folder(window, cx);
                            }),
                    )
                    .child(div().flex_1())
                    .child(Button::new("cancel").outline().label(tr("common.cancel")).on_click(|_, window, cx| window.close_dialog(cx))),
            )
    });
}

/// Install a plugin from a folder holding `khonsu-plugin.json`, or a `.zip`
/// of one (macOS's picker offers both; Windows' picks folders).
pub fn install_from_folder(window: &mut Window, cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some(tr("plugins.install").into()),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let result = smol::unblock(move || plugin::install(&dir)).await;
            cx.update(|window, cx| match result {
                Ok(info) => window.push_notification(trf("plugins.installed", &[&info.manifest.name, &info.manifest.version]), cx),
                Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
            })
            .ok();
        })
        .detach();
}

/// Confirm a third-party plugin's permissions once per version, then
/// configure the source.
fn begin_configure(
    state: Entity<AppState>,
    info: PluginInfo,
    describe: Describe,
    existing: Option<Source>,
    window: &mut Window,
    cx: &mut App,
) {
    if approved(&info) {
        open_form(state, info, describe, existing, window, cx);
        return;
    }
    let lines = permission_lines(&info.manifest.permissions);
    window.open_dialog(cx, move |dialog, _, cx| {
        let (state, info, describe, existing) = (state.clone(), info.clone(), describe.clone(), existing.clone());
        dialog
            .title(tr("permissions.title"))
            .w(crate::ui::scale::design(480.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} {}", info.manifest.name, info.manifest.version)))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).whitespace_normal().child(tr("permissions.body")))
                    .children(lines.iter().map(|l| div().text_sm().child(format!("• {l}"))))
                    .child(
                        div()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(info.dir.display().to_string()),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(Button::new("cancel").outline().label(tr("common.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("approve").primary().label(tr("permissions.approve")).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        if let Err(e) = settings::approve(info.id(), &info.manifest.version, &info.manifest.permissions) {
                            window.push_notification(Notification::error(e.to_string()), cx);
                            return;
                        }
                        open_form(state.clone(), info.clone(), describe.clone(), existing.clone(), window, cx);
                    })),
            )
    });
}

/// Edit a configured source.
pub fn open_edit(state: Entity<AppState>, source: Source, window: &mut Window, cx: &mut App) {
    let plugin_id = source.plugin.clone();
    window
        .spawn(cx, async move |cx| {
            let found = smol::unblock(move || {
                let info = plugin::find(&plugin_id)?;
                let describe = plugin::describe(&info)?;
                Ok::<_, plugin::PluginError>((info, describe))
            })
            .await;
            cx.update(move |window, cx| match found {
                Ok((info, describe)) => begin_configure(state, info, describe, Some(source), window, cx),
                Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
            })
            .ok();
        })
        .detach();
}

enum FieldInput {
    Text(Entity<InputState>),
    Multiline(Entity<TextareaState>),
    Flag(bool),
}

/// The configuration form, rendered inside the dialog.
pub struct SourceForm {
    state: Entity<AppState>,
    info: PluginInfo,
    describe: Describe,
    existing: Option<Source>,
    name: Entity<InputState>,
    fields: Vec<(ConfigField, FieldInput)>,
    busy: Option<&'static str>,
    progress: Arc<Mutex<Option<String>>>,
    message: Option<(bool, String)>,
    preview: Option<Preview>,
}

fn open_form(state: Entity<AppState>, info: PluginInfo, describe: Describe, existing: Option<Source>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| SourceForm::new(state, info, describe, existing, window, cx));
    let title = form.read(cx).title();
    window.open_dialog(cx, move |dialog, _, _| {
        let (test, preview, import) = (form.clone(), form.clone(), form.clone());
        dialog
            .title(title.clone())
            .w(crate::ui::scale::design(DIALOG_WIDTH))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(Button::new("cancel-sync").ghost().label(tr("sources.stop")).on_click(|_, _, _| cancel()))
                    .child(div().flex_1())
                    .child(Button::new("close").outline().label(tr("common.close")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("test")
                            .outline()
                            .label(tr("sources.test"))
                            .on_click(move |_, window, cx| test.update(cx, |f, cx| f.test(window, cx))),
                    )
                    .child(
                        Button::new("preview")
                            .outline()
                            .label(tr("sources.preview"))
                            .on_click(move |_, window, cx| preview.update(cx, |f, cx| f.preview(window, cx))),
                    )
                    .child(
                        Button::new("import")
                            .primary()
                            .label(tr("sources.import"))
                            .on_click(move |_, window, cx| import.update(cx, |f, cx| f.import(window, cx))),
                    ),
            )
    });
}

impl SourceForm {
    fn new(
        state: Entity<AppState>,
        info: PluginInfo,
        describe: Describe,
        existing: Option<Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_name = existing.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| info.manifest.name.clone());
        let name = cx.new(|cx| InputState::new(window, cx).default_value(default_name));
        let fields = describe
            .config
            .iter()
            .map(|field| {
                let current = existing
                    .as_ref()
                    .and_then(|s| s.config.get(&field.key).cloned())
                    .or_else(|| field.default.clone());
                let input = match field.kind {
                    FieldKind::Bool => FieldInput::Flag(current.and_then(|v| v.as_bool()).unwrap_or(false)),
                    _ => {
                        let text = match current {
                            Some(Value::String(s)) => s,
                            Some(Value::Null) | None => String::new(),
                            Some(v) if field.kind == FieldKind::Json => serde_json::to_string_pretty(&v).unwrap_or_default(),
                            Some(v) => v.to_string(),
                        };
                        let stored_secret = field.secret
                            && existing.as_ref().is_some_and(|s| s.secret_keys.contains(&field.key));
                        if field.kind == FieldKind::Json && !field.secret {
                            FieldInput::Multiline(cx.new(|cx| {
                                TextareaState::new(window, cx)
                                    .rows(4)
                                    .placeholder(field.help.clone())
                                    .default_value(text)
                            }))
                        } else {
                            FieldInput::Text(cx.new(|cx| {
                                let mut input = InputState::new(window, cx);
                                if field.secret {
                                    input = input.masked(true);
                                }
                                if stored_secret {
                                    input = input.placeholder(tr("sources.secret_stored"));
                                } else if !field.help.is_empty() {
                                    input = input.placeholder(field.help.clone());
                                }
                                input.default_value(text)
                            }))
                        }
                    }
                };
                (field.clone(), input)
            })
            .collect();
        Self {
            state,
            info,
            describe,
            existing,
            name,
            fields,
            busy: None,
            progress: Arc::new(Mutex::new(None)),
            message: None,
            preview: None,
        }
    }

    fn title(&self) -> String {
        match &self.existing {
            Some(source) => trf("sources.edit_title", &[&source.name]),
            None => trf("sources.add_title", &[&self.describe.name]),
        }
    }

    /// Config and secrets as typed in, checked against the plugin's fields.
    fn values(&self, cx: &App) -> Result<(Map<String, Value>, Map<String, Value>), String> {
        let mut config = Map::new();
        let mut secret_values = Map::new();
        for (field, input) in &self.fields {
            let value = match input {
                FieldInput::Flag(on) => Value::Bool(*on),
                FieldInput::Text(_) | FieldInput::Multiline(_) => {
                    let text = match input {
                        FieldInput::Text(state) => state.read(cx).value().trim().to_string(),
                        FieldInput::Multiline(state) => state.read(cx).value().trim().to_string(),
                        FieldInput::Flag(_) => unreachable!(),
                    };
                    if text.is_empty() {
                        if field.required && !(field.secret && self.has_stored_secret(&field.key)) {
                            return Err(trf("sources.required", &[&field.label]));
                        }
                        continue;
                    }
                    match field.kind {
                        FieldKind::Json => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", field.label))?,
                        FieldKind::Number => text
                            .parse::<f64>()
                            .ok()
                            .and_then(serde_json::Number::from_f64)
                            .map(Value::Number)
                            .ok_or_else(|| trf("sources.not_a_number", &[&field.label]))?,
                        _ => Value::String(text),
                    }
                }
            };
            if field.secret {
                secret_values.insert(field.key.clone(), value);
            } else {
                config.insert(field.key.clone(), value);
            }
        }
        Ok((config, secret_values))
    }

    fn has_stored_secret(&self, key: &str) -> bool {
        self.existing.as_ref().is_some_and(|s| s.secret_keys.iter().any(|k| k == key))
    }

    fn request(&self, cx: &App) -> Result<Request, String> {
        let (config, mut secret_values) = self.values(cx)?;
        if let Some(source) = &self.existing {
            for key in &source.secret_keys {
                if !secret_values.contains_key(key) {
                    if let Ok(Some(value)) = secrets::get(&source.id, key) {
                        secret_values.insert(key.clone(), Value::String(value));
                    }
                }
            }
        }
        Ok(Request {
            protocol: PROTOCOL_VERSION,
            config,
            secrets: secret_values,
            mode: SyncMode::Full,
            cursor: None,
        })
    }

    fn start(&mut self, what: &'static str, cx: &mut Context<Self>) -> bool {
        if self.busy.is_some() {
            return false;
        }
        self.busy = Some(what);
        self.message = None;
        *self.progress.lock().unwrap() = None;
        CANCEL.store(false, Ordering::Relaxed);
        cx.notify();
        true
    }

    /// Repaint while a long task reports progress.
    fn watch_progress(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| loop {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let busy = this.update(cx, |this, cx| {
                cx.notify();
                this.busy.is_some()
            });
            if !matches!(busy, Ok(true)) {
                break;
            }
        })
        .detach();
    }

    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = match self.request(cx) {
            Ok(r) => r,
            Err(e) => {
                self.message = Some((false, e));
                cx.notify();
                return;
            }
        };
        if !self.start("test", cx) {
            return;
        }
        let info = self.info.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || plugin::test(&info, &request)).await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.message = Some(match result {
                    Ok((ok, message)) => (ok, message),
                    Err(e) => (false, e.to_string()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = match self.request(cx) {
            Ok(r) => r,
            Err(e) => {
                self.message = Some((false, e));
                cx.notify();
                return;
            }
        };
        if !self.start("preview", cx) {
            return;
        }
        let info = self.info.clone();
        let progress = self.progress.clone();
        self.watch_progress(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || {
                plugin::preview(&info, &request, &CANCEL, |event| *progress.lock().unwrap() = Some(describe_event(&event)))
            })
            .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(preview) => this.preview = Some(preview),
                    Err(e) => this.message = Some((false, e.to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Save the source (secrets to the keychain), then run a full sync.
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (config, secret_values) = match self.values(cx) {
            Ok(v) => v,
            Err(e) => {
                self.message = Some((false, e));
                cx.notify();
                return;
            }
        };
        let Some(graph) = self.state.read(cx).graph.clone() else {
            return;
        };
        let name = {
            let typed = self.name.read(cx).value().trim().to_string();
            if typed.is_empty() {
                self.describe.name.clone()
            } else {
                typed
            }
        };
        let id = self
            .existing
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| settings::new_source_id(&name));
        let mut secret_keys = self.existing.as_ref().map(|s| s.secret_keys.clone()).unwrap_or_default();
        for (key, value) in &secret_values {
            let text = value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
            if let Err(e) = secrets::set(&id, key, &text) {
                self.message = Some((false, trf("sources.keychain_failed", &[&e])));
                cx.notify();
                return;
            }
            if !secret_keys.contains(key) {
                secret_keys.push(key.clone());
            }
        }
        let source = Source {
            id,
            name,
            plugin: self.info.id().to_string(),
            graph: graph.path,
            config,
            secret_keys,
            cursor: self.existing.as_ref().and_then(|s| s.cursor.clone()),
            patterns: self.existing.as_ref().map(|s| s.patterns.clone()).unwrap_or_default(),
            last_sync: self.existing.as_ref().and_then(|s| s.last_sync.clone()),
        };
        if let Err(e) = settings::upsert_source(source.clone()) {
            self.message = Some((false, e.to_string()));
            cx.notify();
            return;
        }
        self.state.update(cx, |s, cx| s.reload_lists(cx));
        if !self.start("sync", cx) {
            return;
        }
        self.watch_progress(window, cx);
        let progress = self.progress.clone();
        let state = self.state.clone();
        state.update(cx, |s, cx| s.set_syncing(Some(source.name.clone()), cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || run_sync(&source, SyncMode::Full, &progress)).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                state.update(cx, |s, cx| {
                    s.set_syncing(None, cx);
                    s.reload_lists(cx);
                });
                match result {
                    Ok(report) => {
                        window.close_dialog(cx);
                        notify_report(&report, window, cx);
                        crate::ui::reload_schema(state.clone(), window, cx);
                    }
                    Err(e) => this.message = Some((false, e)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn describe_event(event: &SyncEvent) -> String {
    match event {
        SyncEvent::Progress { message, current: Some(c), total: Some(t) } => format!("{message} ({c}/{t})"),
        SyncEvent::Progress { message, .. } => message.clone(),
        SyncEvent::Log { message, .. } => message.clone(),
        SyncEvent::Counts { nodes, edges } => trf("sources.progress_counts", &[&nodes.to_string(), &edges.to_string()]),
    }
}

/// Sync `source` into the open graph. Holds the graph for the whole sync:
/// HawDB has one writer.
fn run_sync(source: &Source, mode: SyncMode, progress: &Mutex<Option<String>>) -> Result<SyncReport, String> {
    crate::db::with_graph(|graph| {
        if graph.mode() == khonsu_core::AccessMode::ReadOnly {
            return Err(khonsu_core::QueryError {
                kind: "read_only".into(),
                message: tr("sources.read_only").into(),
                line: None,
                column: None,
            });
        }
        plugin::sync_source(graph, source, mode, &CANCEL, |event| {
            *progress.lock().unwrap() = Some(describe_event(&event));
        })
        .map_err(|e| khonsu_core::QueryError {
            kind: "plugin".into(),
            message: match e {
                plugin::PluginError::Cancelled => tr("sources.cancelled").to_string(),
                other => other.to_string(),
            },
            line: None,
            column: None,
        })
    })
    .map_err(|e| e.message)
}

fn notify_report(report: &SyncReport, window: &mut Window, cx: &mut App) {
    let m = &report.merge;
    let mut text = trf("sources.done", &[&m.nodes_written.to_string(), &m.edges_written.to_string()]);
    let stale = m.stale_nodes_removed + m.stale_edges_removed;
    if stale > 0 {
        text.push_str(&trf("sources.done_stale", &[&stale.to_string()]));
    }
    if m.skipped_edges > 0 {
        text.push_str(&trf("sources.done_skipped", &[&m.skipped_edges.to_string()]));
        window.push_notification(Notification::warning(format!("{text}\n{}", m.warnings.join("\n"))), cx);
    } else {
        window.push_notification(Notification::success(text), cx);
    }
}

/// Sync a configured source from the sidebar.
pub fn sync(state: Entity<AppState>, source: Source, window: &mut Window, cx: &mut App) {
    CANCEL.store(false, Ordering::Relaxed);
    let progress = Arc::new(Mutex::new(None::<String>));
    state.update(cx, |s, cx| s.set_syncing(Some(source.name.clone()), cx));
    let name = source.name.clone();
    let watch = progress.clone();
    let watch_state = state.clone();
    window
        .spawn(cx, async move |cx| loop {
            smol::Timer::after(std::time::Duration::from_millis(250)).await;
            let line = watch.lock().unwrap().clone();
            let still = cx
                .update(|_, cx| {
                    watch_state.update(cx, |s, cx| {
                        if s.syncing.is_none() {
                            return false;
                        }
                        let label = match &line {
                            Some(line) => format!("{name} — {line}"),
                            None => name.clone(),
                        };
                        s.set_syncing(Some(label), cx);
                        true
                    })
                })
                .unwrap_or(false);
            if !still {
                break;
            }
        })
        .detach();
    window
        .spawn(cx, async move |cx| {
            let result = smol::unblock(move || {
                let mode = if source.cursor.is_some() { SyncMode::Incremental } else { SyncMode::Full };
                run_sync(&source, mode, &progress)
            })
            .await;
            cx.update(move |window, cx| {
                state.update(cx, |s, cx| {
                    s.set_syncing(None, cx);
                    s.reload_lists(cx);
                });
                match result {
                    Ok(report) => {
                        notify_report(&report, window, cx);
                        crate::ui::reload_schema(state.clone(), window, cx);
                    }
                    Err(e) => window.push_notification(Notification::error(e).title(tr("sources.failed")), cx),
                }
            })
            .ok();
        })
        .detach();
}

/// Remove a source and its keychain entries. Nodes it wrote stay in the graph.
pub fn remove(state: Entity<AppState>, source: Source, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let (state, source) = (state.clone(), source.clone());
        dialog
            .title(trf("sources.remove_title", &[&source.name]))
            .w(crate::ui::scale::design(440.))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).whitespace_normal().child(tr("sources.remove_body")))
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(Button::new("cancel").outline().label(tr("common.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("remove").danger().label(tr("sources.remove")).on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        for key in &source.secret_keys {
                            let _ = secrets::delete(&source.id, key);
                        }
                        if let Err(e) = settings::remove_source(&source.id) {
                            window.push_notification(Notification::error(e.to_string()), cx);
                        }
                        state.update(cx, |s, cx| s.reload_lists(cx));
                    })),
            )
    });
}

impl Render for SourceForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let progress = self.progress.lock().unwrap().clone();
        let field_rows = self.fields.iter().enumerate().map(|(ix, (field, input))| {
            let control: AnyElement = match input {
                FieldInput::Multiline(state) => Textarea::new(state).small().into_any_element(),
                FieldInput::Text(state) => {
                    let mut row = h_flex().gap_1().child(div().flex_1().child(Input::new(state).small()));
                    if field.kind == FieldKind::File {
                        let state = state.clone();
                        row = row.child(Button::new(("browse", ix)).small().outline().label(tr("sources.browse")).on_click(
                            move |_, window, cx| {
                                let rx = cx.prompt_for_paths(PathPromptOptions {
                                    files: true,
                                    directories: false,
                                    multiple: false,
                                    prompt: None,
                                });
                                let state = state.clone();
                                window
                                    .spawn(cx, async move |cx| {
                                        if let Ok(Ok(Some(paths))) = rx.await {
                                            if let Some(path) = paths.into_iter().next() {
                                                cx.update(|window, cx| {
                                                    state.update(cx, |s, cx| s.set_value(path.display().to_string(), window, cx))
                                                })
                                                .ok();
                                            }
                                        }
                                    })
                                    .detach();
                            },
                        ));
                    }
                    row.into_any_element()
                }
                FieldInput::Flag(on) => Checkbox::new(("flag", ix))
                    .checked(*on)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if let Some((_, FieldInput::Flag(value))) = this.fields.get_mut(ix) {
                            *value = *checked;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            };
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}{}", field.label, if field.required { " *" } else { "" })),
                )
                .child(control)
                .when(!field.help.is_empty() && !matches!(input, FieldInput::Text(_)) || field.kind == FieldKind::Json, |this| {
                    this.child(div().text_xs().text_color(muted).whitespace_normal().child(field.help.clone()))
                })
        });

        v_flex()
            .id("source-form")
            .gap_3()
            .max_h(crate::ui::scale::design(560.))
            .overflow_y_scroll()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(self.describe.description.clone()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(muted).child(tr("sources.name")))
                    .child(Input::new(&self.name).small()),
            )
            .children(field_rows)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(format!("{}: {}", tr("permissions.declared"), permission_lines(&self.info.manifest.permissions).join("; "))),
            )
            .when_some(self.busy, |this, busy| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(gpui_kit::component::spinner::Spinner::new().small())
                        .child(div().text_sm().child(match busy {
                            "test" => tr("sources.testing").to_string(),
                            "preview" => tr("sources.previewing").to_string(),
                            _ => tr("sources.syncing").to_string(),
                        }))
                        .children(progress.map(|p| div().text_xs().text_color(muted).child(p))),
                )
            })
            .when_some(self.message.clone(), |this, (ok, message)| {
                this.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .text_sm()
                        .whitespace_normal()
                        .font_family(cx.theme().mono_font_family.clone())
                        .bg(if ok { cx.theme().success.opacity(0.1) } else { cx.theme().danger.opacity(0.1) })
                        .text_color(if ok { cx.theme().success } else { cx.theme().danger })
                        .child(message),
                )
            })
            .when_some(self.preview.clone(), |this, preview| {
                let mut chips: Vec<String> = preview.labels.iter().map(|(l, n)| format!("{l} · {n}")).collect();
                chips.extend(preview.relationship_types.iter().map(|(t, n)| format!("→{t} · {n}")));
                let samples: Vec<String> = preview
                    .sample_nodes
                    .iter()
                    .map(|n| serde_json::to_string(n).unwrap_or_default())
                    .chain(preview.sample_edges.iter().map(|e| serde_json::to_string(e).unwrap_or_default()))
                    .collect();
                this.child(
                    v_flex()
                        .p_2()
                        .gap_1()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(trf("sources.preview_title", &[&preview.nodes.to_string(), &preview.edges.to_string()])),
                        )
                        .child(h_flex().flex_wrap().gap_1().children(chips.into_iter().map(|c| {
                            div().text_xs().px_1p5().rounded_sm().bg(cx.theme().secondary).child(c)
                        })))
                        .child(div().text_xs().text_color(muted).child(tr("sources.samples")))
                        .children(samples.into_iter().map(|s| {
                            div()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .truncate()
                                .child(s)
                        })),
                )
            })
    }
}

/// A graph an older HawDB wrote cannot be opened: offer to make it again
/// from its data sources (see `khonsu_core::rebuild`), then open it.
pub fn offer_rebuild(state: Entity<AppState>, path: std::path::PathBuf, window: &mut Window, cx: &mut App) {
    let sources = khonsu_core::rebuild::sources_of(&path);
    window.open_dialog(cx, move |dialog, _, cx| {
        let (state, path) = (state.clone(), path.clone());
        let names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
        let body = if names.is_empty() {
            trf("rebuild.no_sources", &[&path.display().to_string()])
        } else {
            trf("rebuild.body", &[&path.display().to_string(), &names.join(", ")])
        };
        let can_rebuild = !names.is_empty();
        dialog
            .title(tr("rebuild.title"))
            .w(crate::ui::scale::design(480.))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).whitespace_normal().child(body))
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(Button::new("cancel").outline().label(tr("common.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .when(can_rebuild, |footer| {
                        footer.child(Button::new("rebuild").primary().label(tr("rebuild.action")).on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            run_rebuild(state.clone(), path.clone(), window, cx);
                        }))
                    }),
            )
    });
}

fn run_rebuild(state: Entity<AppState>, path: std::path::PathBuf, window: &mut Window, cx: &mut App) {
    window.push_notification(trf("rebuild.running", &[&path.display().to_string()]), cx);
    window
        .spawn(cx, async move |cx| {
            let target = path.clone();
            let result = smol::unblock(move || {
                let cancel = AtomicBool::new(false);
                khonsu_core::rebuild::rebuild(&target, crate::db::PROGRAM, &cancel, |_, _| {})
            })
            .await;
            cx.update(move |window, cx| match result {
                Ok(report) => {
                    let nodes: u64 = report.synced.iter().map(|r| r.merge.nodes_written).sum();
                    let edges: u64 = report.synced.iter().map(|r| r.merge.edges_written).sum();
                    let mut text = trf(
                        "rebuild.done",
                        &[&nodes.to_string(), &edges.to_string(), &report.backup.display().to_string()],
                    );
                    if report.failed.is_empty() {
                        window.push_notification(Notification::success(text), cx);
                    } else {
                        for failed in &report.failed {
                            text.push_str(&format!("\n{}: {}", failed.source, failed.error));
                        }
                        window.push_notification(Notification::warning(text), cx);
                    }
                    crate::ui::open_graph_at(state, path, false, false, window, cx);
                }
                Err(e) => window.push_notification(Notification::error(e.to_string()).title(tr("rebuild.failed")), cx),
            })
            .ok();
        })
        .detach();
}

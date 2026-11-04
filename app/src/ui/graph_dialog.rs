//! Choosing a graph: explains that a graph is a folder, checks a choice before
//! trying it, and keeps the dialog open with what went wrong and what to do
//! next. The checks live in [`khonsu_core::location`].

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::location::{self, FolderCheck, NewGraphProblem};
use khonsu_core::OpenError;

use crate::i18n::{tr, trf};
use crate::state::AppState;

const DIALOG_WIDTH: f32 = 560.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Open,
    New,
    /// A new graph, with the CSV form straight after it.
    ImportCsv,
}

pub fn show(state: Entity<AppState>, purpose: Purpose, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| GraphForm::new(state, purpose, window, cx));
    let title = tr(match purpose {
        Purpose::Open => "title_bar.open_graph",
        Purpose::New => "title_bar.new_graph",
        Purpose::ImportCsv => "home.import_csv",
    });
    window.open_dialog(cx, move |dialog, _, _| dialog.title(title).w(crate::ui::scale::design(DIALOG_WIDTH)).child(form.clone()));
}

/// A pasted path often comes with quotes around it.
fn clean_path(text: &str) -> PathBuf {
    PathBuf::from(text.trim().trim_matches('"'))
}

/// What a choice is, for a person. `None` when there is nothing to warn about.
fn check_message(check: &FolderCheck) -> Option<String> {
    match check {
        FolderCheck::Graph => None,
        FolderCheck::Missing => Some(tr("graph_dialog.check.missing").to_string()),
        FolderCheck::File { csv: true } => Some(tr("graph_dialog.check.csv").to_string()),
        FolderCheck::File { csv: false } => Some(tr("graph_dialog.check.file").to_string()),
        FolderCheck::InsideGraph(graph) => Some(trf("graph_dialog.check.inside", &[&graph.display().to_string()])),
        FolderCheck::HoldsGraphs(graphs) => {
            let names: Vec<String> = graphs.iter().map(|graph| display_name(graph)).collect();
            Some(trf("graph_dialog.check.holds", &[&names.join(", ")]))
        }
        FolderCheck::NotAGraph => Some(tr("graph_dialog.check.not_a_graph").to_string()),
    }
}

fn display_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

fn problem_message(problem: &NewGraphProblem) -> String {
    match problem {
        NewGraphProblem::NoName => tr("graph_dialog.new.no_name").to_string(),
        NewGraphProblem::BadName => tr("graph_dialog.new.bad_name").to_string(),
        NewGraphProblem::NoLocation => tr("graph_dialog.new.no_location").to_string(),
        NewGraphProblem::LocationMissing => tr("graph_dialog.new.location_missing").to_string(),
        NewGraphProblem::Taken(path) => trf("graph_dialog.new.taken", &[&path.display().to_string()]),
    }
}

struct GraphForm {
    state: Entity<AppState>,
    purpose: Purpose,
    /// The graph to open, or the folder to put a new one in.
    path: Entity<InputState>,
    name: Entity<InputState>,
    check: Option<FolderCheck>,
    /// Looking for a place a graph can really be made.
    probing: bool,
    /// Nowhere usable was found, so the person has to choose.
    no_suggestion: bool,
    busy: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl GraphForm {
    fn new(state: Entity<AppState>, purpose: Purpose, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr(if purpose == Purpose::Open {
                "graph_dialog.open.placeholder"
            } else {
                "graph_dialog.new.location_placeholder"
            }))
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(tr("graph_dialog.new.name_placeholder")));
        let on_change = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, _: &mut Window, cx: &mut Context<Self>| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                this.recheck(cx);
                cx.notify();
            }
        };
        let subscriptions = vec![cx.subscribe_in(&path, window, on_change), cx.subscribe_in(&name, window, on_change)];
        let mut form = Self {
            state,
            purpose,
            path,
            name,
            check: None,
            probing: false,
            no_suggestion: false,
            busy: false,
            error: None,
            _subscriptions: subscriptions,
        };
        if purpose != Purpose::Open {
            form.suggest_location(window, cx);
        }
        form
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        let text = self.path.read(cx).value().to_string();
        self.check = (self.purpose == Purpose::Open && !text.trim().is_empty()).then(|| location::check_graph_folder(&clean_path(&text)));
    }

    /// Off the UI thread, since it makes and removes a real graph.
    fn suggest_location(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.probing = true;
        let recent = self.state.read(cx).recent.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = smol::unblock(move || {
                let home = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
                location::suggest_location(&recent, home.as_deref(), crate::db::PROGRAM)
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                this.probing = false;
                // Don't overwrite a place chosen while this ran.
                let untouched = this.path.read(cx).value().trim().is_empty();
                match found {
                    Some(place) if untouched => {
                        this.path.update(cx, |input, cx| input.set_value(place.display().to_string(), window, cx));
                    }
                    Some(_) => {}
                    None => this.no_suggestion = untouched,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr(if self.purpose == Purpose::Open { "title_bar.open_graph" } else { "graph_dialog.new.location" }).into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.no_suggestion = false;
                this.path.update(cx, |input, cx| input.set_value(path.display().to_string(), window, cx));
            })
            .ok();
        })
        .detach();
    }

    fn target(&self, cx: &App) -> Result<PathBuf, NewGraphProblem> {
        let place = clean_path(&self.path.read(cx).value());
        location::new_graph_path(&place, &self.name.read(cx).value())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let creating = self.purpose != Purpose::Open;
        let path = if creating {
            match self.target(cx) {
                Ok(path) => path,
                Err(problem) => {
                    self.error = Some(problem_message(&problem));
                    cx.notify();
                    return;
                }
            }
        } else {
            clean_path(&self.path.read(cx).value())
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let import = self.purpose == Purpose::ImportCsv;
        let state = self.state.clone();
        let this = cx.entity().downgrade();
        crate::ui::open_graph_with(
            self.state.clone(),
            path,
            creating,
            false,
            move |result, window, cx| match result {
                Ok(()) => {
                    window.close_dialog(cx);
                    if import {
                        crate::ui::sources::open_csv(state, window, cx);
                    }
                }
                // Written by an older HawDB: there's a dialog for that already.
                Err(OpenError::OldFormat(path)) => {
                    window.close_dialog(cx);
                    crate::ui::sources::offer_rebuild(state, path, window, cx);
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.busy = false;
                        this.error = Some(crate::ui::open_error_message(&error));
                        cx.notify();
                    })
                    .ok();
                }
            },
            window,
            cx,
        );
    }

    /// Back to the same dialog's sibling, for someone who picked data rather
    /// than a graph.
    fn switch_to_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        window.close_dialog(cx);
        show(state, Purpose::ImportCsv, window, cx);
    }

    /// CSV into the graph already open, for someone who doesn't want a new one.
    fn import_into_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        window.close_dialog(cx);
        crate::ui::sources::open_csv(state, window, cx);
    }

    fn render_open(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let warning = cx.theme().warning;
        let success = cx.theme().success;
        let check = self.check.clone();
        let ok = check == Some(FolderCheck::Graph);
        let message = check.as_ref().and_then(check_message);
        // Not after a try that failed: the error below says otherwise.
        let name = (ok && self.error.is_none()).then(|| display_name(&clean_path(&self.path.read(cx).value())));
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).whitespace_normal().child(tr("graph_dialog.open.explain")))
            .child(self.render_path_row(cx))
            .when_some(name, |this, name| {
                this.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_sm()
                        .text_color(success)
                        .child(Icon::new(IconName::CircleCheck).xsmall())
                        .child(trf("graph_dialog.check.graph", &[&name])),
                )
            })
            .when_some(message, |this, message| this.child(div().text_sm().whitespace_normal().text_color(warning).child(message)))
    }

    fn render_new(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let target = self.target(cx);
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).whitespace_normal().child(tr(if self.purpose == Purpose::ImportCsv {
                "graph_dialog.import.explain"
            } else {
                "graph_dialog.new.explain"
            })))
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr("graph_dialog.new.name")))
                    .child(Input::new(&self.name).small()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(tr("graph_dialog.new.location")))
                    .child(self.render_path_row(cx)),
            )
            .when(self.probing, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(muted)
                        .child(Spinner::new().xsmall())
                        .child(tr("graph_dialog.new.probing")),
                )
            })
            .when(self.no_suggestion, |this| {
                this.child(div().text_sm().whitespace_normal().text_color(cx.theme().warning).child(tr("graph_dialog.new.no_suggestion")))
            })
            .map(|this| match &target {
                Ok(path) => this.child(
                    div()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(muted)
                        .child(trf("graph_dialog.new.will_create", &[&path.display().to_string()])),
                ),
                // Empty fields are just not filled in yet.
                Err(NewGraphProblem::NoName | NewGraphProblem::NoLocation) => this,
                Err(problem) => this.child(div().text_sm().whitespace_normal().text_color(cx.theme().warning).child(problem_message(problem))),
            })
    }

    fn render_path_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().gap_2().child(div().flex_1().min_w_0().child(Input::new(&self.path).small())).child(
            Button::new("graph-browse")
                .outline()
                .small()
                .icon(IconName::FolderOpen)
                .label(tr("graph_dialog.browse"))
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
        )
    }
}

impl Render for GraphForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.purpose == Purpose::Open;
        let ready = if open { self.check == Some(FolderCheck::Graph) } else { self.target(cx).is_ok() };
        let suggest_import = open && self.check.as_ref().is_some_and(|check| *check != FolderCheck::Graph);
        let (open_graph, writable) = {
            let state = self.state.read(cx);
            (state.graph.as_ref().map(|graph| graph.name()), !state.read_only())
        };
        let import_into = (self.purpose == Purpose::ImportCsv && writable).then_some(open_graph).flatten();
        v_flex()
            .gap_4()
            .map(|this| if open { this.child(self.render_open(cx).into_any_element()) } else { this.child(self.render_new(cx).into_any_element()) })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().danger)
                        .text_sm()
                        .whitespace_normal()
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(suggest_import, |this| {
                        this.child(
                            Button::new("graph-import-instead")
                                .outline()
                                .label(tr("home.import_csv"))
                                .on_click(cx.listener(|this, _, window, cx| this.switch_to_import(window, cx))),
                        )
                    })
                    .when_some(import_into, |this, name| {
                        this.child(
                            Button::new("graph-import-open")
                                .outline()
                                .label(trf("graph_dialog.import.into_open", &[&name]))
                                .on_click(cx.listener(|this, _, window, cx| this.import_into_open(window, cx))),
                        )
                    })
                    .child(div().flex_1())
                    .child(Button::new("graph-cancel").outline().label(tr("common.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("graph-submit")
                            .primary()
                            .label(tr(match self.purpose {
                                Purpose::Open => "graph_dialog.open.submit",
                                Purpose::New => "graph_dialog.new.submit",
                                Purpose::ImportCsv => "graph_dialog.import.submit",
                            }))
                            .loading(self.busy)
                            .disabled(!ready || self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{check_message, clean_path, FolderCheck, PathBuf};

    #[test]
    fn a_pasted_path_loses_its_quotes_and_spaces() {
        assert_eq!(clean_path("  \"E:\\graphs\\shop\" "), PathBuf::from("E:\\graphs\\shop"));
    }

    #[test]
    fn only_a_graph_goes_without_a_warning() {
        assert!(check_message(&FolderCheck::Graph).is_none());
        for check in [
            FolderCheck::Missing,
            FolderCheck::File { csv: true },
            FolderCheck::File { csv: false },
            FolderCheck::InsideGraph(PathBuf::from("g")),
            FolderCheck::HoldsGraphs(vec![PathBuf::from("g")]),
            FolderCheck::NotAGraph,
        ] {
            assert!(check_message(&check).is_some(), "{check:?}");
        }
    }
}

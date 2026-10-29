//! UI layer: title bar, sidebar, workspace, results, graph canvas, status bar.

pub mod agent_avatar;
pub mod canvas;
pub mod chart;
pub mod geo;
pub mod grid;
pub mod palette;
pub mod results;
pub mod scale;
pub mod setup_dialog;
pub mod sidebar;
pub mod sources;
pub mod status_bar;
pub mod theme;
pub mod tiles;
pub mod title_bar;
pub mod workspace;

use std::path::PathBuf;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt;
use gpui_kit::{Action, App, Entity, KeyBinding, KeyBindingContextPredicate, PathPromptOptions, Window};
use khonsu_core::OpenError;
use serde::Deserialize;

use crate::i18n::{tr, trf};
use crate::state::AppState;

gpui_kit::actions!(
    khonsugraph,
    [
        RunQuery,
        ExplainQuery,
        SaveSpec,
        OpenDashboard,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ToggleSidebar,
        NewQuery,
        NewSqlQuery,
        CloseTab,
        NextTab,
        PreviousTab,
        OpenGraph,
        NewGraph,
        AddSource,
        OpenLogs,
        ToggleBaseMap,
        Quit
    ]
);

/// ⌘1–⌘8 bring that tab to the front, ⌘9 the last one.
#[derive(Clone, Action, PartialEq, Eq, Deserialize)]
#[action(namespace = khonsugraph, no_json)]
pub struct ActivateTab {
    pub index: usize,
}

/// Key context that makes ⌘↵ reachable while the editor is focused.
pub const WORKSPACE_KEY_CONTEXT: &str = "KhonsuGraph";

// `secondary` is ⌘ on macOS and Ctrl on Windows and Linux.
pub const RUN_QUERY_KEYSTROKE: &str = "secondary-enter";

/// A binding that may use `secondary-`, which only the host OS can resolve.
fn bind(keystrokes: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let context = context.map(|context| KeyBindingContextPredicate::parse(context).unwrap().into());
    KeyBinding::load(keystrokes, Box::new(action), context, false, None, cx).unwrap()
}

pub fn init(cx: &mut App) {
    gpui_graph::init(cx);
    let bindings = [
        bind(RUN_QUERY_KEYSTROKE, RunQuery, Some(WORKSPACE_KEY_CONTEXT), cx),
        bind("secondary-e", ExplainQuery, Some(WORKSPACE_KEY_CONTEXT), cx),
        bind("secondary-s", SaveSpec, Some(WORKSPACE_KEY_CONTEXT), cx),
        bind("secondary-=", ZoomIn, None, cx),
        bind("secondary-+", ZoomIn, None, cx),
        bind("secondary--", ZoomOut, None, cx),
        bind("secondary-0", ZoomReset, None, cx),
        bind("secondary-b", ToggleSidebar, None, cx),
        bind("secondary-t", NewQuery, None, cx),
        bind("secondary-shift-t", NewSqlQuery, None, cx),
        bind("secondary-w", CloseTab, None, cx),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        bind("secondary-o", OpenGraph, None, cx),
        bind("secondary-n", NewGraph, None, cx),
        bind("secondary-q", Quit, None, cx),
    ];
    cx.bind_keys(bindings);
    let tabs: Vec<_> = (1..=9)
        .map(|n| {
            let index = if n == 9 { usize::MAX } else { n - 1 };
            bind(&format!("secondary-{n}"), ActivateTab { index }, None, cx)
        })
        .collect();
    cx.bind_keys(tabs);
    cx.on_action(|_: &ZoomIn, cx| scale::set(scale::current().larger(), cx));
    cx.on_action(|_: &ZoomOut, cx| scale::set(scale::current().smaller(), cx));
    cx.on_action(|_: &ZoomReset, cx| scale::set(scale::UiSize::Default, cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ToggleBaseMap, cx| {
        tiles::toggle(cx);
        // The item's check mark is part of the menu, built once.
        set_menus(cx);
    });
    cx.on_action(|_: &OpenLogs, cx| {
        if let Some(dir) = crate::crash::logs_dir() {
            std::fs::create_dir_all(&dir).ok();
            cx.open_with_system(&dir);
        }
    });
    set_menus(cx);
}

/// The menu bar, rebuilt when the language changes.
pub fn set_menus(cx: &mut App) {
    use gpui_kit::component::input;
    use gpui_kit::{Menu, MenuItem};
    let base_map = tiles::enabled(cx);
    let menu = |name: &'static str, items: Vec<MenuItem>| Menu {
        name: tr(name).into(),
        items,
        disabled: false,
    };
    cx.set_menus(vec![
        Menu {
            name: "KhonsuGraph".into(),
            items: vec![
                MenuItem::action(tr("menu.open_logs"), OpenLogs),
                MenuItem::Separator,
                MenuItem::action(tr("menu.quit"), Quit),
            ],
            disabled: false,
        },
        menu(
            "menu.file",
            vec![
                MenuItem::action(tr("title_bar.open_graph"), OpenGraph),
                MenuItem::action(tr("title_bar.new_graph"), NewGraph),
                MenuItem::action(tr("sources.add"), AddSource),
                MenuItem::action(tr("workspace.open_dashboard"), OpenDashboard),
                MenuItem::Separator,
                MenuItem::action(tr("workspace.new_cypher"), NewQuery),
                MenuItem::action(tr("workspace.new_sql"), NewSqlQuery),
                MenuItem::action(tr("menu.close_tab"), CloseTab),
                MenuItem::action(tr("menu.next_tab"), NextTab),
                MenuItem::action(tr("menu.previous_tab"), PreviousTab),
            ],
        ),
        menu(
            "menu.edit",
            vec![
                MenuItem::action(tr("menu.undo"), input::Undo),
                MenuItem::action(tr("menu.redo"), input::Redo),
                MenuItem::Separator,
                MenuItem::action(tr("menu.cut"), input::Cut),
                MenuItem::action(tr("menu.copy"), input::Copy),
                MenuItem::action(tr("menu.paste"), input::Paste),
                MenuItem::action(tr("menu.select_all"), input::SelectAll),
            ],
        ),
        menu(
            "menu.query",
            vec![
                MenuItem::action(tr("workspace.run"), RunQuery),
                MenuItem::action(tr("workspace.explain"), ExplainQuery),
            ],
        ),
        menu(
            "menu.view",
            vec![
                MenuItem::action(tr("menu.toggle_sidebar"), ToggleSidebar),
                MenuItem::Separator,
                MenuItem::action(tr("menu.zoom_in"), ZoomIn),
                MenuItem::action(tr("menu.zoom_out"), ZoomOut),
                MenuItem::action(tr("menu.zoom_reset"), ZoomReset),
                MenuItem::Separator,
                MenuItem::action(tr("menu.base_map"), ToggleBaseMap).checked(base_map),
            ],
        ),
    ]);
}

/// How opening a graph can go wrong, said for a person.
fn open_error_message(err: &OpenError) -> String {
    let mut message = match err {
        OpenError::Locked(_) => trf("notify.open.locked", &[&err.to_string()]),
        OpenError::NotAGraph(path) => trf("notify.open.not_a_graph", &[&path.display().to_string()]),
        OpenError::NotEmpty(path) => trf("notify.open.not_empty", &[&path.display().to_string()]),
        _ => trf("notify.open.failed", &[&err.to_string()]),
    };
    if let OpenError::Engine { hint: Some(hint), .. } = err {
        message.push('\n');
        message.push_str(hint);
    }
    message
}

/// Open (or with `create`, create) the graph at `path` off the UI thread,
/// then load its schema.
pub fn open_graph_at(
    state: Entity<AppState>,
    path: PathBuf,
    create: bool,
    read_only: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if state.read(cx).is_opening() {
        return;
    }
    state.update(cx, |state, cx| state.begin_open(cx));
    window
        .spawn(cx, async move |cx| {
            let result = smol::unblock(move || {
                if create {
                    crate::db::create(&path)
                } else {
                    crate::db::open(&path, read_only)
                }
            })
            .await;
            cx.update(move |window, cx| {
                state.update(cx, |state, cx| state.end_open(cx));
                match result {
                    Ok(info) => {
                        state.update(cx, |state, cx| state.set_graph(Some(info), cx));
                        reload_schema(state, window, cx);
                    }
                    // Written by an older HawDB: offer to make it again
                        // rather than show the engine's integrity error.
                        Err(OpenError::OldFormat(path)) => crate::ui::sources::offer_rebuild(state, path, window, cx),
                    Err(err) => {
                        let title = match err {
                            OpenError::Locked(_) => tr("notify.open.locked_title"),
                            _ => tr("notify.open.failed_title"),
                        };
                        window.push_notification(
                            Notification::error(open_error_message(&err)).title(title),
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
}

/// Ask for a folder and open it as a graph, or create one there.
pub fn pick_graph(state: Entity<AppState>, create: bool, window: &mut Window, cx: &mut App) {
    if state.read(cx).is_opening() {
        return;
    }
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(tr(if create { "title_bar.new_graph" } else { "title_bar.open_graph" }).into()),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(move |window, cx| open_graph_at(state, path, create, false, window, cx))
                .ok();
        })
        .detach();
}

pub fn close_graph(state: Entity<AppState>, cx: &mut App) {
    crate::db::close();
    state.update(cx, |state, cx| state.set_graph(None, cx));
}

/// Reread the schema of the open graph into the state.
pub fn reload_schema(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let schema = smol::unblock(crate::db::schema).await;
            cx.update(move |_, cx| {
                state.update(cx, |state, cx| state.set_schema(schema.map_err(|e| e.message), cx));
            })
            .ok();
        })
        .detach();
}

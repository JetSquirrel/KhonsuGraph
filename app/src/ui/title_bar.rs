//! Title bar, after DuckLocal's: sidebar toggle and the Graph menu on the
//! left, the open graph in the middle, language, size and theme on the right.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::toolbar::{Toolbar, ToolbarGroup};
use gpui_kit::component::{
    h_flex, ActiveTheme, IconName, Sizable, Theme, ThemeMode, TitleBar,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::i18n::{tr, Language};
use crate::state::{AppState, GraphChanged, OpenStateChanged, SidebarToggled};

pub struct TitleBarView {
    state: Entity<AppState>,
    _subscriptions: Vec<Subscription>,
}

impl TitleBarView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |_, _, _: &GraphChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &OpenStateChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &SidebarToggled, cx| cx.notify()),
        ];
        Self {
            state,
            _subscriptions: subscriptions,
        }
    }

    fn toggle_theme(_: &ClickEvent, window: &mut Window, cx: &mut App) {
        let next = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
        crate::ui::scale::apply(cx);
        let _ = crate::prefs::set("theme", if next.is_dark() { "dark" } else { "light" });
    }

    fn toggle_language(_: &ClickEvent, _: &mut Window, cx: &mut App) {
        let next = match crate::i18n::current() {
            Language::Zh => Language::En,
            Language::En => Language::Zh,
        };
        crate::i18n::set_language(next);
        crate::ui::set_menus(cx);
        cx.refresh_windows();
    }

    fn render_graph_menu(&self, opening: bool, has_graph: bool, read_only: bool) -> impl IntoElement {
        let state = self.state.clone();
        Button::new("graph-menu")
            .ghost()
            .compact()
            .xsmall()
            .icon(IconName::FolderOpen)
            .label(tr("title_bar.graph"))
            .loading(opening)
            .dropdown_menu(move |menu, _, _| {
                let open_state = state.clone();
                let new_state = state.clone();
                let ro_state = state.clone();
                let source_state = state.clone();
                let close_state = state.clone();
                menu.item(
                    PopupMenuItem::new(tr("title_bar.open_graph"))
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, window, cx| crate::ui::pick_graph(open_state.clone(), false, window, cx)),
                )
                .item(
                    PopupMenuItem::new(tr("title_bar.new_graph"))
                        .icon(IconName::Plus)
                        .on_click(move |_, window, cx| crate::ui::pick_graph(new_state.clone(), true, window, cx)),
                )
                .when(has_graph, |menu| {
                    menu.separator()
                        .item(
                            PopupMenuItem::new(tr(if read_only {
                                "title_bar.reopen_writable"
                            } else {
                                "title_bar.reopen_read_only"
                            }))
                            .on_click(move |_, window, cx| {
                                let Some(info) = ro_state.read(cx).graph.clone() else {
                                    return;
                                };
                                crate::ui::open_graph_at(ro_state.clone(), info.path, false, !read_only, window, cx);
                            }),
                        )
                        .when(!read_only, |menu| {
                            menu.item(
                                PopupMenuItem::new(tr("sources.add"))
                                    .icon(IconName::Plus)
                                    .on_click(move |_, window, cx| {
                                        crate::ui::sources::open_add(source_state.clone(), window, cx)
                                    }),
                            )
                        })
                        .separator()
                        .item(
                            PopupMenuItem::new(tr("title_bar.close_graph"))
                                .on_click(move |_, _, cx| crate::ui::close_graph(close_state.clone(), cx)),
                        )
                })
            })
    }
}

/// Keeps a dropdown trigger out of the Windows caption hit-test without
/// breaking it (see DuckLocal's title bar).
fn caption_exempt(dropdown: impl IntoElement) -> impl IntoElement {
    div().occlude().child(dropdown)
}

impl Render for TitleBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (graph, opening, sidebar_collapsed) = {
            let state = self.state.read(cx);
            (state.graph.clone(), state.is_opening(), state.is_sidebar_collapsed())
        };
        let dark = cx.theme().mode.is_dark();
        let read_only = graph.as_ref().is_some_and(|g| g.read_only);

        TitleBar::new().when(window.is_fullscreen(), |this| this.pl_0()).child(
            Toolbar::new("title-bar-toolbar")
                .w_full()
                .xsmall()
                .child(
                    ToolbarGroup::new("title-bar-graph")
                        .gap_1()
                        .child(
                            Button::new("toggle-sidebar")
                                .icon(if sidebar_collapsed {
                                    IconName::PanelLeftOpen
                                } else {
                                    IconName::PanelLeftClose
                                })
                                .tooltip_with_action(
                                    tr(if sidebar_collapsed { "sidebar.expand" } else { "sidebar.collapse" }),
                                    &crate::ui::ToggleSidebar,
                                    None,
                                )
                                .occlude()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.update(cx, |state, cx| state.toggle_sidebar(cx));
                                })),
                        )
                        .content(caption_exempt(self.render_graph_menu(opening, graph.is_some(), read_only))),
                )
                .content(div().flex_1())
                .content(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(
                            img("brand/logo.png")
                                .size(crate::ui::scale::design(18.))
                                .rounded_sm(),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_sm().child("KhonsuGraph"))
                        .when_some(graph.as_ref(), |this, info| {
                            this.child(
                                div()
                                    .id("graph-path")
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(format!("— {}", info.name()))
                                    .tooltip({
                                        let path = info.path.display().to_string();
                                        move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                                        }
                                    }),
                            )
                        })
                        .when(read_only, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .px_1p5()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr("title_bar.read_only")),
                            )
                        }),
                )
                .content(div().flex_1())
                .child(
                    ToolbarGroup::new("title-bar-window")
                        .gap_1()
                        .child(
                            Button::new("toggle-language")
                                .label(match crate::i18n::current() {
                                    Language::Zh => "EN",
                                    Language::En => "中",
                                })
                                .w_6()
                                .tooltip(tr("title_bar.toggle_language"))
                                // Consume the press so the Windows caption does
                                // not start a window move (see DuckLocal).
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                    gpui_kit::component::GlobalState::suppress_text_selection(cx);
                                })
                                .on_click(Self::toggle_language),
                        )
                        .content(caption_exempt(
                            Button::new("ui-size")
                                .ghost()
                                .compact()
                                .xsmall()
                                .icon(IconName::ALargeSmall)
                                .tooltip(tr("title_bar.ui_size"))
                                .dropdown_menu(|menu, _, _| {
                                    let current = crate::ui::scale::current();
                                    crate::ui::scale::UiSize::ALL.into_iter().fold(menu, |menu, size| {
                                        menu.item(
                                            PopupMenuItem::new(tr(size.label_key()))
                                                .checked(size == current)
                                                .on_click(move |_, _, cx| crate::ui::scale::set(size, cx)),
                                        )
                                    })
                                }),
                        ))
                        .child(
                            Button::new("toggle-theme")
                                .icon(if dark { IconName::Sun } else { IconName::Moon })
                                .tooltip(tr("title_bar.toggle_theme"))
                                .occlude()
                                .on_click(Self::toggle_theme),
                        ),
                ),
        )
    }
}

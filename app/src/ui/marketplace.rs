//! The marketplace dialog: what the known marketplaces offer, installing a
//! plugin from one, and adding a marketplace by its address.
//!
//! Installing downloads and checks a package (see `khonsu_core::marketplace`)
//! and runs nothing of it. The plugin then goes through the same approval
//! dialog as one installed from a folder, before its form opens.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Disableable, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::marketplace::{self, Listing, Marketplace, Unreachable};
use khonsu_core::plugin;

use crate::i18n::{tr, trf};
use crate::state::AppState;
use crate::ui::sources;

const DIALOG_WIDTH: f32 = 680.;

pub struct MarketplaceView {
    state: Entity<AppState>,
    loading: bool,
    marketplaces: Vec<Marketplace>,
    listings: Vec<Listing>,
    unreachable: Vec<Unreachable>,
    /// The plugin being downloaded, or `+` while a marketplace is being added.
    busy: Option<String>,
    message: Option<(bool, String)>,
    location: Entity<InputState>,
}

/// Opens the marketplace and starts reading what it offers.
pub fn open(state: Entity<AppState>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| MarketplaceView {
        state,
        loading: true,
        marketplaces: Vec::new(),
        listings: Vec::new(),
        unreachable: Vec::new(),
        busy: None,
        message: None,
        location: cx.new(|cx| InputState::new(window, cx).placeholder(tr("marketplace.add_placeholder"))),
    });
    view.update(cx, |view, cx| view.load(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(tr("marketplace.title"))
            .w(crate::ui::scale::design(DIALOG_WIDTH))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .gap_2()
                    .child(
                        Button::new("install-plugin")
                            .outline()
                            .label(tr("plugins.install"))
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                                sources::install_from_folder(window, cx);
                            }),
                    )
                    .child(div().flex_1())
                    .child(Button::new("close").outline().label(tr("common.close")).on_click(|_, window, cx| window.close_dialog(cx))),
            )
    });
}

impl MarketplaceView {
    /// Reads every marketplace's index, off the main thread: it is the network.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let read = smol::unblock(|| (marketplace::list(), marketplace::available(None, None))).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                let (marketplaces, available) = read;
                this.marketplaces = marketplaces;
                match available {
                    Ok((listings, unreachable)) => {
                        this.listings = listings;
                        this.unreachable = unreachable;
                    }
                    Err(e) => this.message = Some((false, e.to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads and installs one listing. A newly installed plugin goes on to
    /// its approval and its form; an updated one is only reported.
    fn install(&mut self, listing: &Listing, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let id = listing.entry.id.clone();
        let spec = format!("{id}@{}", listing.marketplace);
        let update = listing.installed.is_some();
        self.busy = Some(id);
        self.message = None;
        cx.notify();
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || marketplace::install(&spec)).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(installed) if update => {
                        let manifest = &installed.plugin.manifest;
                        this.message = Some((true, trf("marketplace.updated", &[&manifest.name, &manifest.version])));
                        this.load(window, cx);
                    }
                    Ok(installed) => {
                        window.close_dialog(cx);
                        let manifest = &installed.plugin.manifest;
                        window.push_notification(trf("plugins.installed", &[&manifest.name, &manifest.version]), cx);
                        sources::begin_configure(state, installed.plugin, None, window, cx);
                    }
                    Err(e) => this.message = Some((false, e.to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Goes on to a source with a plugin that is already installed.
    fn add_source(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        match plugin::find(&id) {
            Ok(info) => {
                window.close_dialog(cx);
                sources::begin_configure(self.state.clone(), info, None, window, cx);
            }
            Err(e) => {
                self.message = Some((false, e.to_string()));
                cx.notify();
            }
        }
    }

    fn add_marketplace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let location = self.location.read(cx).value().trim().to_string();
        if location.is_empty() || self.busy.is_some() {
            return;
        }
        self.busy = Some("+".into());
        self.message = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || marketplace::add(&location, None)).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok((added, index)) => {
                        this.message = Some((true, trf("marketplace.added", &[&added.name, &index.plugins.len().to_string()])));
                        this.location.update(cx, |input, cx| input.set_value(String::new(), window, cx));
                        this.load(window, cx);
                    }
                    Err(e) => this.message = Some((false, e.to_string())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_marketplace(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        match marketplace::remove(&name) {
            Ok(()) => self.load(window, cx),
            Err(e) => {
                window.push_notification(Notification::error(e.to_string()), cx);
            }
        }
    }
}

impl Render for MarketplaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let busy = self.busy.clone();
        let rows = self.listings.iter().enumerate().map(|(ix, listing)| {
            let entry = &listing.entry;
            let downloading = busy.as_deref() == Some(entry.id.as_str());
            let action: AnyElement = match (&listing.target, &listing.installed) {
                (None, _) => div().text_xs().text_color(cx.theme().warning).child(tr("marketplace.no_package")).into_any_element(),
                (Some(_), Some(_)) if !listing.update => {
                    let id = entry.id.clone();
                    Button::new(("use", ix))
                        .small()
                        .outline()
                        .label(tr("sources.add"))
                        .on_click(cx.listener(move |this, _, window, cx| this.add_source(id.clone(), window, cx)))
                        .into_any_element()
                }
                (Some(_), installed) => {
                    let listing = listing.clone();
                    Button::new(("install", ix))
                        .small()
                        .primary()
                        .loading(downloading)
                        .disabled(busy.is_some())
                        .label(match installed {
                            Some(_) => trf("marketplace.update", &[&entry.version]),
                            None => tr("marketplace.install").to_string(),
                        })
                        .on_click(cx.listener(move |this, _, window, cx| this.install(&listing, window, cx)))
                        .into_any_element()
                }
            };
            let name = if entry.name.is_empty() { entry.id.clone() } else { entry.name.clone() };
            h_flex()
                .p_3()
                .gap_3()
                .items_start()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                                .child(div().text_xs().text_color(muted).child(entry.version.clone()))
                                .child(div().text_xs().px_1().rounded_sm().bg(cx.theme().secondary).child(listing.marketplace.clone()))
                                .when_some(listing.installed.clone(), |this, version| {
                                    this.child(div().text_xs().text_color(cx.theme().success).child(trf("marketplace.installed", &[&version])))
                                }),
                        )
                        .child(div().text_xs().text_color(muted).whitespace_normal().child(entry.description.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .whitespace_normal()
                                .child(format!("{}: {}", tr("permissions.declared"), sources::permission_lines(&entry.permissions).join("; "))),
                        ),
                )
                .child(action)
        });
        let sources_line = self.marketplaces.iter().enumerate().map(|(ix, marketplace)| {
            let name = marketplace.name.clone();
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(muted)
                        .truncate()
                        .child(format!("{} — {}", marketplace.name, marketplace.location)),
                )
                .child(div().flex_1())
                .child(
                    Button::new(("remove-marketplace", ix))
                        .xsmall()
                        .ghost()
                        .label(tr("marketplace.remove"))
                        .on_click(cx.listener(move |this, _, window, cx| this.remove_marketplace(name.clone(), window, cx))),
                )
        });

        v_flex()
            .id("marketplace")
            .gap_2()
            .max_h(crate::ui::scale::design(560.))
            .overflow_y_scroll()
            .child(div().text_sm().text_color(muted).whitespace_normal().child(tr("marketplace.body")))
            .when(self.loading, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(gpui_kit::component::spinner::Spinner::new().small())
                        .child(div().text_sm().child(tr("marketplace.loading"))),
                )
            })
            .when(!self.loading && self.listings.is_empty() && self.unreachable.is_empty(), |this| {
                this.child(div().text_sm().text_color(muted).child(tr("marketplace.empty")))
            })
            .children(rows)
            .children(self.unreachable.iter().map(|unreachable| {
                div()
                    .p_2()
                    .rounded_md()
                    .text_xs()
                    .whitespace_normal()
                    .bg(cx.theme().warning.opacity(0.1))
                    .text_color(cx.theme().warning)
                    .child(trf("marketplace.unreachable", &[&unreachable.marketplace, &unreachable.message]))
            }))
            .when_some(self.message.clone(), |this, (ok, message)| {
                this.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .text_sm()
                        .whitespace_normal()
                        .bg(if ok { cx.theme().success.opacity(0.1) } else { cx.theme().danger.opacity(0.1) })
                        .text_color(if ok { cx.theme().success } else { cx.theme().danger })
                        .child(message),
                )
            })
            .child(div().pt_2().text_xs().text_color(muted).child(tr("marketplace.sources")))
            .children(sources_line)
            .child(
                h_flex().gap_1().child(div().flex_1().child(Input::new(&self.location).small())).child(
                    Button::new("add-marketplace")
                        .small()
                        .outline()
                        .loading(busy.as_deref() == Some("+"))
                        .disabled(busy.is_some())
                        .label(tr("marketplace.add"))
                        .on_click(cx.listener(|this, _, window, cx| this.add_marketplace(window, cx))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings in gpui's `test`, which the attribute below
    // would then expand to again.
    use super::{marketplace, AppState, InputState, Listing, MarketplaceView, Unreachable};
    use gpui_kit::AppContext as _;
    use khonsu_core::marketplace::{Entry, Package};
    use khonsu_core::protocol::Permissions;

    fn listing(id: &str, target: Option<&str>, installed: Option<&str>, update: bool) -> Listing {
        Listing {
            marketplace: "official".into(),
            entry: Entry {
                id: id.into(),
                name: id.to_uppercase(),
                version: "0.3.0".into(),
                description: "Tables as nodes and relationships".into(),
                permissions: Permissions {
                    network: vec!["The database server you configure".into()],
                    read_files: true,
                    ..Permissions::default()
                },
                packages: vec![Package {
                    target: "macos-aarch64".into(),
                    url: format!("{id}.zip"),
                    sha256: "0".repeat(64),
                    size: 1,
                }],
            },
            target: target.map(str::to_string),
            installed: installed.map(str::to_string),
            update,
        }
    }

    /// Every state a row can be in draws: to install, installed, out of date,
    /// and with no package for this computer, beside a marketplace that could
    /// not be read.
    #[gpui_kit::test]
    fn every_state_of_the_dialog_draws(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let state = cx.new(AppState::new);
            MarketplaceView {
                state,
                loading: false,
                marketplaces: marketplace::list(),
                listings: vec![
                    listing("sql", Some("macos-aarch64"), None, false),
                    listing("ducklocal", Some("macos-aarch64"), Some("0.3.0"), false),
                    listing("old", Some("any"), Some("0.1.0"), true),
                    listing("elsewhere", None, None, false),
                ],
                unreachable: vec![Unreachable {
                    marketplace: "team".into(),
                    location: "https://example.com/marketplace.json".into(),
                    message: "http status: 404".into(),
                }],
                busy: Some("sql".into()),
                message: Some((false, "not the package listed".into())),
                location: cx.new(|cx| InputState::new(window, cx)),
            }
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.busy = None;
            view.loading = true;
            view.listings.clear();
            cx.notify();
        });
        cx.run_until_parked();
    }
}

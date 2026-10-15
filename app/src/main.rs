// A GUI executable must not allocate a console when opened from Explorer.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod assets;
mod crash;
mod db;
mod i18n;
mod prefs;
mod state;
mod ui;

use gpui_kit::component::{ThemeMode, TitleBar};
use gpui_kit::*;

use crate::app::KhonsuApp;

fn main() {
    crash::init();
    i18n::set_current(i18n::initial_language());
    ui::scale::load();

    let application = gpui_kit::application().with_assets(assets::AppAssets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        ui::init(cx);
        let mode = match prefs::get("theme").ok().flatten().as_deref() {
            Some("dark") => ThemeMode::Dark,
            _ => ThemeMode::Light,
        };
        ui::theme::install(mode, cx);
        ui::scale::apply(cx);

        let window_bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(window_bounds)),
            window_min_size: Some(size(px(960.), px(600.))),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, move |window, cx| cx.new(|cx| KhonsuApp::new(window, cx)))
            .expect("Failed to open window");
    });
}

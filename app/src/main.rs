// A GUI executable must not allocate a console when opened from Explorer.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod assets;
mod crash;
mod db;
mod history;
mod i18n;
mod prefs;
mod query;
mod remote;
mod setup;
mod spec;
mod state;
mod ui;
#[cfg(windows)]
mod windows;

use std::ffi::OsString;
use std::path::Path;

use gpui_kit::component::{ThemeMode, TitleBar};
use gpui_kit::*;

use crate::app::KhonsuApp;

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();
    if let Some(code) = cli_call(&args) {
        std::process::exit(code);
    }

    crash::init();
    i18n::set_current(i18n::initial_language());
    ui::scale::load();

    let application = gpui_kit::application().with_assets(assets::AppAssets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        ui::init(cx);
        let mode = match prefs::get("theme").ok().flatten().as_deref() {
            Some("light") => ThemeMode::Light,
            _ => ThemeMode::Dark,
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

/// Run a command line without a window, as DuckLocal does: this executable
/// is the `khonsu` command too. It is one when called by that name (the link
/// the Command line & AI dialog makes, or `khonsu.exe` on Windows), or when
/// its first argument is a subcommand or a flag. macOS adds `-psn_…` to a
/// launch from Finder, which opens the window.
fn cli_call(args: &[OsString]) -> Option<i32> {
    if !is_cli_call(args) {
        return None;
    }
    #[cfg(windows)]
    windows::attach_cli_console();
    Some(khonsu_cli::run(args.iter().skip(1).cloned()))
}

/// Whether `args`, the program name first, are a command line (see `cli_call`).
fn is_cli_call(args: &[OsString]) -> bool {
    let called_khonsu = args
        .first()
        .and_then(|program| Path::new(program).file_stem())
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("khonsu"));
    let command = args
        .get(1)
        .and_then(|first| first.to_str())
        .is_some_and(|first| khonsu_cli::is_command(first) || (first.starts_with('-') && !first.starts_with("-psn_")));
    called_khonsu || command
}

#[cfg(test)]
mod tests {
    use super::is_cli_call;
    use std::ffi::OsString;

    fn call(args: &[&str]) -> bool {
        is_cli_call(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn a_subcommand_or_a_flag_runs_the_cli_and_nothing_opens_the_window() {
        assert!(!call(&["khonsugraph"]));
        assert!(!call(&["/Applications/KhonsuGraph.app/Contents/MacOS/khonsugraph", "-psn_0_12345"]));
        assert!(call(&["khonsugraph", "query", "--cypher", "MATCH (n) RETURN n"]));
        assert!(call(&["khonsugraph", "--help"]));
        assert!(call(&["khonsugraph", "help"]));
        assert!(!call(&["khonsugraph", "notes.txt"]));
    }

    #[test]
    fn called_as_khonsu_it_is_always_the_cli() {
        assert!(call(&["/usr/local/bin/khonsu"]));
        assert!(call(&["khonsu", "open", "--state"]));
        assert!(call(&[r"C:\KhonsuGraph\khonsu.exe"]) || cfg!(not(windows)));
        assert!(call(&["KHONSU.EXE"]));
    }
}

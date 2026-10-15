//! Root view.

use gpui_kit::component::{v_flex, ActiveTheme};
use gpui_kit::*;

use crate::i18n::tr;

pub struct KhonsuApp;

impl KhonsuApp {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for KhonsuApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .items_center()
            .justify_center()
            .child("KhonsuGraph")
            .child(tr("app.tagline"))
    }
}

//! Bottom status bar, after DuckLocal's: the open graph on the left, the last
//! query and the versions on the right.

use gpui_kit::component::separator::Separator;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{h_flex, ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::state::{AppState, GraphChanged, OpenStateChanged, QueryStatsChanged, SchemaChanged, SourcesChanged};

pub struct StatusBarView {
    state: Entity<AppState>,
    _subscriptions: Vec<Subscription>,
}

impl StatusBarView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |_, _, _: &GraphChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &QueryStatsChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &OpenStateChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &SchemaChanged, cx| cx.notify()),
            cx.subscribe(&state, |_, _, _: &SourcesChanged, cx| cx.notify()),
        ];
        Self {
            state,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for StatusBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let mono = cx.theme().mono_font_family.clone();

        let left = if state.is_opening() {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Spinner::new().xsmall())
                .child(tr("status_bar.opening"))
                .into_any_element()
        } else if let Some(syncing) = &state.syncing {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Spinner::new().xsmall())
                .child(trf("status_bar.syncing", &[syncing]))
                .into_any_element()
        } else if let Some(graph) = &state.graph {
            let counts = state
                .schema
                .as_ref()
                .map(|s| trf("status_bar.counts", &[&s.node_count.to_string(), &s.relationship_count.to_string()]));
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Icon::new(IconName::CircleCheck).xsmall().text_color(cx.theme().success))
                .child(div().font_family(mono.clone()).child(graph.path.display().to_string()))
                .children(counts.map(|c| div().text_color(cx.theme().muted_foreground).child(c)))
                .into_any_element()
        } else {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(Icon::new(IconName::CircleX).xsmall().text_color(cx.theme().muted_foreground))
                .child(tr("status_bar.no_graph"))
                .into_any_element()
        };

        let mut bar = StatusBar::new().h(crate::ui::scale::design(32.)).left(left);
        if let Some(stats) = &state.last_query {
            let mut text = trf(
                "status_bar.last_query",
                &[&crate::state::format_duration(stats.elapsed_ms), &stats.rows.to_string()],
            );
            if stats.nodes + stats.edges > 0 {
                text.push_str(&trf("status_bar.last_query_graph", &[&stats.nodes.to_string(), &stats.edges.to_string()]));
            }
            bar = bar.right(Separator::vertical()).right(div().font_family(mono.clone()).child(text));
        }
        bar.right(Separator::vertical()).right(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(format!("KhonsuGraph {} · HawDB {}", env!("CARGO_PKG_VERSION"), khonsu_core::HAWDB_VERSION)),
        )
    }
}

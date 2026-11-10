//! The graph canvas: nodes and relationships of a result, drawn by
//! [`gpui_graph::NetworkGraph`]. This view owns the HawDB side — converting
//! results, fetching missing endpoints and neighbors, the inspector and the
//! legend — and leaves layout, painting and interaction to the component.
//!
//! Drag the background to pan, scroll to zoom, drag a node to move it. Click
//! a node or relationship to inspect it; double-click a node to add its
//! neighbors around it. Expanding never moves nodes already on the canvas.
//! Grouping by a node property outlines each value's nodes; double-click a
//! collapsed group to open it again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gpui_graph::{ColorBy, Edge, GraphData as Graph, LabelMode, NetworkGraph, NetworkGraphEvent, Node, Selection};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt, WindowExt, TITLE_BAR_HEIGHT};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use khonsu_core::evidence::{self, Evidence};
use khonsu_core::explore::{Direction, Exploration};
use khonsu_core::investigation::{Capture, Investigation, Reopened, SavedQuery, EXTENSION};
use khonsu_core::plugin_analysis::Offered;
use khonsu_core::protocol::{ConditionStatus, VerdictTone};
use khonsu_core::settings::{self, Source};
use khonsu_core::{GraphData, GraphEdge, GraphNode};
use serde_json::Value as Json;

use crate::i18n::{tr, trf};
use crate::ui::explorer::{self, Explorer, View};
use crate::ui::palette;

mod display;
mod inspector;
mod investigate;
mod load;
mod nodes;
mod render;

use nodes::*;

/// Above this many nodes the canvas shows the first ones and says so.
pub const NODE_LIMIT: usize = 2000;

/// A dashboard `graph`'s data-driven look: the node property that colors the
/// nodes, fixed colors for some of its values, and the numeric property that
/// sizes them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeStyle {
    pub color: Option<String>,
    /// Value and color: a name from `khonsu_dash::model::GRAPH_COLORS` or
    /// `#rrggbb`.
    pub colors: Vec<(String, String)>,
    pub size: Option<String>,
}

/// How nodes are drawn: what [`to_node`] needs beside the node.
struct Look<'a> {
    caption: Option<&'a str>,
    group: Option<&'a str>,
    style: &'a NodeStyle,
    /// Values of the color property in the order they were first seen; a
    /// value's palette slot is its place here.
    color_slots: &'a [String],
    /// The smallest and largest value of the size property on the canvas.
    size_range: Option<(f64, f64)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    Node(i64),
    Edge(i64),
}

pub struct GraphCanvas {
    graph: Entity<NetworkGraph>,
    /// Everything on the canvas, by HawDB id, for the inspector.
    nodes: HashMap<i64, GraphNode>,
    edges: HashMap<i64, GraphEdge>,
    selected: Option<Selected>,
    /// Nodes the result held beyond [`NODE_LIMIT`].
    hidden: usize,
    expanding: bool,
    explorer: Explorer,
    /// Configured sources, read when a selection changes, for the inspector's
    /// evidence.
    sources: Vec<Source>,
    /// The canvas shows only one path of the last walk.
    path_view: bool,
    /// The query behind this result, to keep with a saved investigation.
    query: Option<SavedQuery>,
    /// The node property shown as each node's caption, before the usual
    /// name-like ones: a dashboard `graph`'s `caption`.
    caption_key: Option<String>,
    /// The node property nodes are grouped by, if any.
    group_key: Option<String>,
    /// A dashboard `graph`'s `color`, `colors` and `size`.
    style: NodeStyle,
    /// See [`Look::color_slots`]; kept while the canvas shows one result.
    color_slots: Vec<String>,
    size_range: Option<(f64, f64)>,
    /// Which captions are drawn, and whether a hovered or picked node fades
    /// the rest: a dashboard `graph`'s `labels` and `highlight`.
    label_mode: LabelMode,
    focus_dimming: bool,
    /// A dashboard panel shows no inspector: its width is the grid's.
    compact: bool,
    /// Drawn over the whole window below the title bar, instead of in place.
    maximized: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    /// Offers the button that fills the window; a dashboard panel has its
    /// own, which enlarges the panel within the dashboard.
    can_fill_window: bool,
    _subscriptions: Vec<Subscription>,
}

/// What a canvas tells its host: a node was selected, for a dashboard filter
/// to pick its property.
pub enum CanvasEvent {
    NodeSelected(GraphNode),
}

impl EventEmitter<CanvasEvent> for GraphCanvas {}

impl GraphCanvas {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let graph = cx.new(|cx| NetworkGraph::new(cx).with_label_mode(LabelMode::Auto));
        let subscription = cx.subscribe_in(&graph, window, Self::on_graph_event);
        Self {
            graph,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            selected: None,
            hidden: 0,
            expanding: false,
            explorer: Explorer::default(),
            sources: Vec::new(),
            path_view: false,
            query: None,
            caption_key: None,
            group_key: None,
            style: NodeStyle::default(),
            color_slots: Vec::new(),
            size_range: None,
            label_mode: LabelMode::Auto,
            focus_dimming: true,
            compact: false,
            maximized: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            can_fill_window: true,
            _subscriptions: vec![subscription],
        }
    }

}

#[cfg(test)]
mod tests {
    use super::{edge_id, group_keys, to_edge};
    use khonsu_core::{GraphEdge, GraphNode};
    use serde_json::json;

    fn person(id: i64, name: &str, community: i64, city: &str) -> GraphNode {
        GraphNode {
            id,
            labels: vec!["Person".into()],
            properties: [
                ("name".to_string(), json!(name)),
                ("community".to_string(), json!(community)),
                ("city".to_string(), json!(city)),
                ("khonsu_source".to_string(), json!("people")),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn shared_plain_values_are_offered_for_grouping_and_names_are_not() {
        let nodes = [
            person(1, "Ada", 1, "London"),
            person(2, "Bob", 1, "London"),
            person(3, "Cy", 2, "Paris"),
            person(4, "Di", 2, "Paris"),
        ];
        assert_eq!(group_keys(nodes.iter()), vec!["city".to_string(), "community".to_string()]);
    }

    #[test]
    fn edge_round_trips_its_id() {
        let edge = GraphEdge {
            id: 42,
            source: 1,
            target: 2,
            rel_type: "KNOWS".into(),
            properties: Default::default(),
        };
        let converted = to_edge(&edge);
        assert_eq!(converted.source.as_ref(), "1");
        assert_eq!(converted.target.as_ref(), "2");
        assert_eq!(edge_id(&converted), Some(42));
    }
}

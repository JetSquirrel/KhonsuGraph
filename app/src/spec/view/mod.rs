//! The view half of a `.dash` spec: the file as a workspace tab.
//!
//! `mod.rs` checks a spec without a window; this module draws one. A
//! `Dashboard` parses and validates its file, runs every query on the window's
//! connection, and lays the plots out on a twelve-column grid: each plot
//! spans its `width`, plots fill a row left to right and wrap, and each row is
//! one panel of a vertical resizable stack — the row heights are the user's
//! to drag, and a stack taller than the tab scrolls. The data a plot draws is
//! `prepare`'s business; the view only renders what it prepares. A plot whose
//! query failed, or whose columns do not resolve, shows the reason in its own
//! panel: one bad plot never takes the dashboard down. And a reload follows
//! the app rule: a spec that no longer validates never replaces a working
//! dashboard — the previous one stays up and the reason appears above it.
//!
//! One chart note: the catalog's `BarChart` and `LineChart` draw a single
//! series and its one multi-series chart fills areas, so every `bar`, a
//! pivoted `line` and every `scatter` are drawn by the plots in `plot.rs` —
//! grouped bars, bare lines, unconnected dots — on the same primitives the
//! catalog charts compose.
//!
//! A `filter` block makes a plot pickable: a click on a bar, or on a table's
//! row, picks a value, and every query that reads `$name` re-runs with it —
//! only those, and only when the SQL they would run has changed; the picking
//! plot's own queries never narrow (see `filter.rs`). The picks show as chips
//! above the plots, each clearing its own; a click on the picked bar again
//! clears it too.
//!
//! The toolbar's source toggle swaps the whole body for the spec's text in an
//! editor — not a read-only one: the source is where a dashboard is fixed.
//! Edits are written back with the save button or ⌘S, and a save reloads; a
//! save whose spec no longer validates keeps the last working dashboard up
//! and says why above it. While the buffer holds unsaved edits the tab shows
//! a dirty dot, and an external change to the file is a conflict banner
//! rather than a silent overwrite; with a clean buffer it just reloads.
//!
//! The `Dashboard` lives here with its rendering entry point; its methods are
//! split by concern: `run` (reading and running the spec), `layout` (the grid
//! and the maximized plot), `panel` (one plot's face), `table`, `picks`
//! (filters), `source` (the editor and file watching), `review` (comments)
//! and `activity` (agent presence).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::spec::presence::{self, Presence};
use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::base::{ResizeHandleContext, ResizeHandleState};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::input::{CompletionProvider, Editor, EditorState, Rope, RopeExt, TabSize};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{resizable_panel, resize_handle_appearance, v_resizable};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt, TITLE_BAR_HEIGHT};
use gpui_kit::prelude::FluentBuilder;

use crate::spec::wheel::WheelLatch;
use gpui_kit::*;

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryOutcome, QueryResult};
use crate::spec::comment_card::CommentCard;
use crate::spec::comments::{self, Author, Comments};
use crate::spec::complete::{self, CompletionKind};
use crate::spec::filter::Pick;
use crate::spec::model::{self, Spec};
use crate::spec::plot::{x_label_count, GroupedBars, SeriesPlot};
use crate::spec::prepare::{keep_series_order, prepare, PlotPoint, PreparedPlot};
use crate::spec::watch::{Debounce, FileStamp, POLL_INTERVAL};
use crate::ui::chart::{format_value, legend_row, map_notes, pie_parts};
use crate::ui::grid::starts_with_ignore_case;
use crate::ui::canvas::{CanvasEvent, GraphCanvas};
use crate::ui::geo::GeoPlot;
use crate::ui::grid::{fit_column_width, sorted_rows};

mod activity;
mod layout;
mod panel;
mod picks;
mod review;
mod run;
mod source;
mod table;

use layout::*;
use picks::*;
use table::*;

/// The plot panels' default and drag bounds, in the spirit of the workspace's
/// own results split.
const PLOT_DEFAULT: f32 = 300.;
const PLOT_MIN: f32 = 160.;
const PLOT_MAX: f32 = 800.;
/// A row with a graph starts taller: a force layout spreads about as far
/// down as across, so a chart's short, wide row drew it small, with the
/// sides of the panel empty.
const GRAPH_DEFAULT: f32 = 440.;
/// A row of nothing but cards holds one number each: it starts short.
const CARD_DEFAULT: f32 = 104.;
/// A card with a note under its value needs a line more.
const CARD_NOTED: f32 = 124.;
const CARD_MIN: f32 = 80.;
const CARD_MAX: f32 = 400.;
/// A row of nothing but text panels heads what follows: a title and a line.
const TEXT_DEFAULT: f32 = 76.;
/// A text panel's measures at the default interface size, for sizing its row
/// to its words: padding and gaps, the title's line, one line of words, and
/// one character's average width at the words' size.
const TEXT_PADDING: f32 = 30.;
const TEXT_TITLE: f32 = 34.;
const TEXT_LINE: f32 = 24.;
const TEXT_CHAR: f32 = 8.5;
/// The width a text panel is taken to have before the first layout.
const TEXT_ROOM_GUESS: f32 = 900.;
const TEXT_MIN: f32 = 40.;
/// The space between two plots' cards, and between the cards and the tab's
/// edges.
const PAGE_GAP: f32 = 8.;
/// A plot card's corner radius.
const PANEL_RADIUS: f32 = 8.;

/// What a sortable header's arrow takes beside the column name.
const SORT_ICON_WIDTH: f32 = 20.;

/// A table plot's chrome around its rows — title, header row, padding — and
/// one row's height, for sizing a row of tables to its content.
const TABLE_CHROME: f32 = 84.;
const TABLE_ROW: f32 = 30.;
const TABLE_MIN: f32 = 120.;

/// A `.dash` file open as a tab: its plots, and the run that produced them.
pub struct Dashboard {
    /// The tab's id, baked into the resizable group's element id so two
    /// dashboards never share panel sizes.
    id: u64,
    path: PathBuf,
    /// Why nothing can be drawn: the spec itself did not read, parse or
    /// validate, and no earlier spec is still drawable. Per-plot failures live
    /// on the plot instead.
    spec_error: Option<String>,
    /// Why what is shown is out of date, while an earlier run still draws.
    stale_reason: Option<String>,
    plots: Vec<PreparedPlot>,
    /// The successful query results by query name, for the `table` plots that
    /// render the grid directly.
    results: HashMap<String, Arc<QueryResult>>,
    /// Table entities, one per table plot. They need a window to create, so
    /// they are built lazily at render and dropped on every landed run.
    tables: Vec<Option<Entity<TableState<SpecTableDelegate>>>>,
    running: bool,
    /// Whether the body shows the spec's source instead of the plots.
    showing_source: bool,
    /// The spec text as last read from (or written to) disk, or why it could
    /// not be read: the baseline the editor's dirty state is measured against.
    source: Option<Result<String, String>>,
    /// The editor the source view draws in, built lazily like the tables;
    /// `source_version`/`editor_version` pair so a re-read pushes to the
    /// editor once, and no render resets its scroll or cursor.
    source_editor: Option<Entity<EditorState>>,
    source_version: u64,
    editor_version: u64,
    /// The file's stamp as last written by us or reloaded by the watcher, so
    /// our own save does not come back as an "external" change.
    known_stamp: FileStamp,
    /// The file changed on disk while the buffer held unsaved edits. Shown as
    /// a banner; the buffer is never overwritten to resolve it — saving is
    /// the resolution.
    conflict: bool,
    /// Polls the spec file for external changes. Dropping it ends the
    /// watcher, which is how closing the tab stops watching.
    watcher: Option<Task<()>>,
    /// Which scroller the wheel gesture in progress moves: a table plot, or
    /// the stack it sits in (see `wheel`).
    wheel: WheelLatch,
    /// The spec the plots on screen came from: which filters exist and which
    /// plot each picks on.
    spec: Option<Arc<Spec>>,
    /// What each filter has picked, by filter name. Survives reloads for as
    /// long as the filter does.
    picks: HashMap<String, Pick>,
    /// Each query's last outcome and the SQL that produced it: a re-run after
    /// a pick reuses every query whose SQL the pick did not change.
    outcomes: Outcomes,
    /// A pick changed while a run was in flight: run again when it lands.
    refilter_pending: bool,
    /// The graph changed while a run was in flight: reload when it lands.
    reload_pending: bool,
    /// Where each pickable plot was last laid out, by plot name, for a click
    /// to find its bar.
    plot_bounds: HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>,
    /// One canvas per `graph` panel, by plot name, with the data it shows so
    /// a re-run that returned the same graph keeps the layout.
    graph_canvases: HashMap<String, GraphPanel>,
    /// Row-selection subscriptions of the pickable tables, by table entity;
    /// dropped with them.
    table_subscriptions: Vec<(EntityId, Subscription)>,
    /// The plots turned over to show their SQL, by plot name.
    sql_shown: HashSet<String>,
    /// The plot blown up to fill the dashboard, by plot name; Esc or its
    /// button puts it back on the grid.
    maximized: Option<String>,
    /// Takes Esc while a plot is blown up.
    focus: FocusHandle,
    /// Each card's value size for this frame, by plot index: the smallest
    /// any card in its grid row needs, so a row of numbers reads as one.
    card_sizes: HashMap<usize, f32>,
    /// The review threads on the plots, as last read from the comments file
    /// beside the spec (see `comments`), and why it could not be read.
    comments: Comments,
    comments_error: Option<String>,
    /// The plot whose comment card is open, by plot name.
    comment_open: Option<String>,
    /// Each plot's comment card, by plot name; built lazily at render, since
    /// its text box needs a window, and kept so a half-written comment
    /// survives the card closing.
    comment_cards: HashMap<String, Entity<CommentCard>>,
    /// Agents at work on this dashboard and what their edits changed (see
    /// `presence`).
    presence: Presence,
    /// Whether the timer that ends quiet sessions and counts how long the
    /// changed plots have been in view is running.
    presence_ticking: bool,
    _presence_tick: Option<Task<()>>,
    /// Where each plot with a change mark was last drawn, to tell whether
    /// the person can see it.
    card_bounds: HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>,
    /// When the plots were last painted, and whether the window had focus
    /// then: a hidden tab or an unfocused window shows nobody anything.
    painted: Rc<Cell<Option<Instant>>>,
    window_active: Rc<Cell<bool>>,
    /// The run in flight was started by an edit on disk, whose changed plots
    /// are marked when it lands.
    external_reload: bool,
    /// Each plot's definition as last drawn — its block and its query's SQL —
    /// to tell which plots an edit on disk changed.
    plot_prints: HashMap<String, String>,
}

/// What a dashboard asks of the workspace around it.
pub enum DashboardEvent {
    /// Open this SQL in a new query tab, under this title.
    OpenSql { sql: String },
}

impl EventEmitter<DashboardEvent> for Dashboard {}

/// Each query's outcome by name, with the SQL that ran.
type Outcomes = HashMap<String, (String, Result<Arc<QueryResult>, String>)>;

/// A `graph` panel's canvas, the data it shows, and its subscription.
type GraphPanel = (Entity<GraphCanvas>, Option<Arc<khonsu_core::GraphData>>, Subscription);

impl Dashboard {
    pub fn new(id: u64, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let known_stamp = FileStamp::capture(&path);
        let mut this = Self {
            id,
            path,
            spec_error: None,
            stale_reason: None,
            plots: Vec::new(),
            results: HashMap::new(),
            tables: Vec::new(),
            running: false,
            showing_source: false,
            source: None,
            source_editor: None,
            source_version: 0,
            editor_version: 0,
            known_stamp,
            conflict: false,
            watcher: None,
            wheel: WheelLatch::default(),
            spec: None,
            picks: HashMap::new(),
            outcomes: HashMap::new(),
            refilter_pending: false,
            reload_pending: false,
            plot_bounds: HashMap::new(),
            graph_canvases: HashMap::new(),
            table_subscriptions: Vec::new(),
            sql_shown: HashSet::new(),
            maximized: None,
            focus: cx.focus_handle(),
            card_sizes: HashMap::new(),
            comments: Comments::default(),
            comments_error: None,
            comment_open: None,
            comment_cards: HashMap::new(),
            presence: Presence::default(),
            presence_ticking: false,
            _presence_tick: None,
            card_bounds: HashMap::new(),
            painted: Rc::new(Cell::new(None)),
            window_active: Rc::new(Cell::new(false)),
            external_reload: false,
            plot_prints: HashMap::new(),
        };
        this.load_comments();
        this.watch(cx);
        this.reload(cx);
        this
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// What the tab shows, for `ducklocal open --state`: each plot and whether it
    /// drew, and every filter with the value it has picked, if any.
    pub fn summary(&self) -> serde_json::Value {
        use serde_json::json;
        let filters = self.spec.as_ref().map_or_else(Vec::new, |spec| {
            spec.filters
                .iter()
                .map(|filter| {
                    let pick = self.picks.get(&filter.name);
                    json!({
                        "name": filter.name,
                        "plot": filter.plot,
                        "column": filter.column,
                        "picked": pick.is_some(),
                        "multiple": filter.multiple,
                        "value": pick.and_then(|pick| (pick.values.len() == 1).then(|| pick.values[0].clone())),
                        "values": pick.map(|pick| pick.values.clone()),
                    })
                })
                .collect()
        });
        json!({
            "running": self.running,
            "error": self.spec_error,
            "stale": self.stale_reason,
            "plots": self.plots.iter().map(|plot| json!({
                "name": plot.name,
                "type": plot.kind,
                "title": plot.title,
                "query": plot.query,
                "failure": plot.failure,
                "rows": self.results.get(&plot.query).map(|r| r.rows.len()),
                "open_comments": self.comments.open_on(&plot.name),
            })).collect::<Vec<_>>(),
            "filters": filters,
            "comments_file": comments::path_for(&self.path),
        })
    }

    fn render_spec_failure(&self, error: String, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(IconName::CircleX)
                    .large()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr("dashboard.spec_failed")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .max_w_96()
                    .text_center()
                    .child(error),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                Icon::new(AssetIcon::ChartNoAxesColumn)
                    .large()
                    // Decoration, not data: deliberately faded.
                    .text_color(cx.theme().muted_foreground.alpha(0.5)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("dashboard.empty")),
            )
            .into_any_element()
    }

    fn render_loading(&self, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().large().color(cx.theme().muted_foreground))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("results.running")),
            )
            .into_any_element()
    }

    /// The zone the dashboard's times are shown in, when its spec sets one.
    fn render_zone(&self, cx: &App) -> Option<impl IntoElement> {
        let zone = self.spec.as_ref()?.timezone.as_deref().and_then(khonsu_dash::zone::Zone::parse)?;
        Some(
            div()
                .flex_none()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(trf("dashboard.timezone", &[&zone.label()])),
        )
    }

    fn render_stale_warning(&self, cx: &App) -> Option<impl IntoElement> {
        let reason = self.stale_reason.clone()?;
        Some(
            v_flex()
                .flex_none()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().danger.alpha(0.12))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr("dashboard.not_updated")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                ),
        )
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_active.set(window.is_window_active());
        let body = if self.showing_source {
            // The source view stands on its own, spec error or not: the text
            // of a broken spec is exactly what one opens the source to fix.
            self.render_source(window, cx)
        } else if let Some(error) = self.spec_error.clone() {
            self.render_spec_failure(error, cx)
        } else if let Some(ix) = self.maximized_ix() {
            self.render_maximized(ix, window, cx)
        } else if self.plots.is_empty() {
            // The first run has not landed yet: loading, not empty. A spec
            // that ran and has no plots is the empty state.
            if self.running {
                self.render_loading(cx)
            } else {
                self.render_empty(cx)
            }
        } else {
            let rows = grid_rows(self.plots.iter().map(|plot| plot.width));
            // Cards side by side share one value size: one shrunk alone left
            // its number smaller and its label lower than its neighbours'.
            self.card_sizes.clear();
            for row in &rows {
                let cards: Vec<usize> = row
                    .iter()
                    .copied()
                    .filter(|&ix| self.plots[ix].kind == "card")
                    .collect();
                let size = cards
                    .iter()
                    .map(|&ix| self.card_value_size(ix))
                    .fold(CARD_VALUE_MAX, f32::min);
                self.card_sizes
                    .extend(cards.into_iter().map(|ix| (ix, size)));
            }
            let mut stack = 0.;
            let panels = rows
                .iter()
                .map(|row| {
                    let all = |kind: &str| row.iter().all(|&ix| self.plots[ix].kind == kind);
                    // A heading row with a card beside it is a row of cards.
                    let cards = row
                        .iter()
                        .all(|&ix| matches!(self.plots[ix].kind.as_str(), "card" | "text"));
                    let (default, min, max) = if all("text") {
                        (self.text_row_height(row), TEXT_MIN, CARD_MAX)
                    } else if cards {
                        let noted = row.iter().any(|&ix| self.plots[ix].note.is_some());
                        let default = if noted { CARD_NOTED } else { CARD_DEFAULT };
                        (default, CARD_MIN, CARD_MAX)
                    } else if row.iter().any(|&ix| self.plots[ix].kind == "graph") {
                        (GRAPH_DEFAULT, PLOT_MIN, PLOT_MAX)
                    } else if let Some(height) = self.table_row_height(row) {
                        // A row of short tables is as tall as its rows, not
                        // a chart's height of empty space under them.
                        (height, TABLE_MIN.min(height), PLOT_MAX)
                    } else {
                        (PLOT_DEFAULT, PLOT_MIN, PLOT_MAX)
                    };
                    stack += default;
                    // A row short of twelve columns leaves the rest empty,
                    // so a plot's width means the same on every row. The
                    // gutter is padding inside each share, not a gap
                    // between them, so the shares still sum to the row.
                    let text_row = all("text");
                    let cells = row.iter().map(|&ix| {
                        let share = self.plots[ix].width as f32 / model::GRID_COLUMNS as f32;
                        // A text panel heads the page, on the page itself;
                        // every other plot is a card of its own.
                        let boxed = self.plots[ix].kind != "text";
                        div()
                            .h_full()
                            .w(relative(share))
                            .min_w_0()
                            .relative()
                            .px(px(PAGE_GAP / 2.))
                            .child(
                                div()
                                    .size_full()
                                    .relative()
                                    .overflow_hidden()
                                    .when(boxed, |this| {
                                        this.bg(crate::ui::palette::panel(cx))
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded(px(PANEL_RADIUS))
                                    })
                                    .child(self.render_plot(ix, window, cx))
                                    .children(self.render_plot_presence(ix, cx))
                                    .children(self.record_card_bounds(ix)),
                            )
                            .children(self.render_presence_tag(ix, cx))
                    });
                    let cells = cells.collect::<Vec<_>>();
                    let default = default + PAGE_GAP;
                    stack += PAGE_GAP;
                    resizable_panel()
                        .size(px(default))
                        .size_range(px(min + PAGE_GAP)..px(max + PAGE_GAP))
                        .child(
                            h_flex()
                                .size_full()
                                .px(px(PAGE_GAP / 2.))
                                .py(px(PAGE_GAP / 2.))
                                .when(text_row, |this| this.pb_0())
                                .children(cells),
                        )
                })
                .collect::<Vec<_>>();
            // Every row keeps its default height and the tab scrolls, rather
            // than a dozen plots squeezed into one screen and the rest clipped
            // out of reach. Dragging a divider still trades height between
            // neighbours. A tab taller than the stack is not stretched to fill:
            // the group scales every row by the same factor, so a row of cards
            // would grow as much as a chart, all of it empty space. The rest
            // of the tab stays below the last row, as on a page.
            let stack = px(stack);
            let wheel = self.wheel.clone();
            let viewport = self.wheel.viewport();
            let painted = self.painted.clone();
            // The canvas sits outside the scrolling div, so its bounds are
            // the viewport's, not the scrolled content's.
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id(format!("dashboard-scroll-{}", self.id))
                        .size_full()
                        .on_scroll(move |event, window, _| {
                            wheel.stack_scrolled(event, window)
                        })
                        .overflow_y_scrollbar()
                        .child(
                            v_flex()
                                .w_full()
                                .child(
                                    div().w_full().h(stack).child(
                                        v_resizable(format!("dashboard-{}", self.id))
                                            // The page gap already parts the rows: a
                                            // line across it at rest would cut the page
                                            // in strips. Under the pointer the handle
                                            // shows itself as everywhere else.
                                            .with_handle_appearance({
                                                let shown = resize_handle_appearance();
                                                Rc::new(
                                            move |handle: &ResizeHandleContext,
                                                  window: &mut Window,
                                                  cx: &mut App| {
                                                if handle.state() == ResizeHandleState::Idle {
                                                    Some(div().into_any_element())
                                                } else {
                                                    shown(handle, window, cx)
                                                }
                                            },
                                        )
                                            })
                                            .children(panels),
                                    ),
                                )
                                .children(self.render_adding(cx)),
                        ),
                )
                .child(
                    canvas(
                        move |b, _, _| viewport.set(Some(b)),
                        move |_, _, _, _| painted.set(Some(Instant::now())),
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !this.showing_source && this.maximized_ix().is_some() {
                    cx.stop_propagation();
                    this.set_maximized(None, window, cx);
                }
            }))
            .bg(crate::ui::palette::page(cx))
            .text_color(cx.theme().foreground)
            .children(self.render_conflict_warning(cx))
            .children(self.render_stale_warning(cx))
            .when(!self.showing_source, |this| {
                this.children(self.render_removed(cx))
            })
            .when(!self.showing_source, |this| {
                this.children(self.render_zone(cx)).children(self.render_picks(cx))
            })
            .child(body)
            .children(self.render_presence_names(cx))
            .into_any_element()
    }
}

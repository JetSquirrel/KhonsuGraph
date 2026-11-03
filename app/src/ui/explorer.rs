//! Path Explorer's state, apart from the canvas that draws it: what to walk,
//! what the last walk found, and the views to go back to.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use khonsu_core::evidence::{Assertion, Evidence, Field, SourceCoverage};
use khonsu_core::settings::RunStatus;
use khonsu_core::explore::{Direction, Exploration, ExploreRequest, Hop, Limit, DEFAULT_MAX_HOPS, MAX_HOPS};
use khonsu_core::investigation::{GraphState, Investigation, RecordRef, Reopened};
use khonsu_core::plugin_analysis::{Finding, Offered, Report};
use khonsu_core::protocol::{AnalysisInfo, ConditionStatus, HopRef};
use khonsu_core::util::format_unix_time;
use khonsu_core::{GraphData, GraphEdge, GraphNode};

use crate::i18n::{tr, trf};

/// A canvas to return to. Positions are not kept: the layout starts over.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub walk: Option<Exploration>,
    pub selected: Option<i64>,
}

/// What a reopened investigation said, kept beside the walk it gave until the
/// canvas gets another result.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub name: String,
    pub saved_at: u64,
    pub graph_state: GraphState,
    pub differences: Vec<String>,
    pub missing_seeds: Vec<RecordRef>,
    pub target_missing: bool,
    pub unsure: Vec<RecordRef>,
    pub path_same: Option<bool>,
    pub incomplete_sources: Vec<String>,
}

impl Restored {
    pub fn new(saved: &Investigation, reopened: &Reopened) -> Self {
        Self {
            name: saved.name.clone(),
            saved_at: saved.saved_at,
            graph_state: reopened.graph_state,
            differences: reopened.differences.clone(),
            missing_seeds: reopened.missing_seeds.clone(),
            target_missing: reopened.target_missing,
            unsure: reopened.unsure.clone(),
            path_same: reopened.path_same,
            incomplete_sources: reopened.incomplete_sources.clone(),
        }
    }

    /// Everything that differs from when it was saved, as warnings, so that a
    /// new answer does not pass for the saved one.
    pub fn status_lines(&self) -> Vec<(String, bool)> {
        let name = |record: &RecordRef| record.stable_id.clone().unwrap_or_else(|| format!("#{}", record.internal_id));
        let names = |records: &[RecordRef]| records.iter().map(name).collect::<Vec<_>>().join(", ");
        let mut lines = Vec::new();
        match self.graph_state {
            GraphState::Unchanged => {}
            GraphState::Changed => lines.push((tr("investigation.changed").to_string(), true)),
            GraphState::NotCompared => lines.push((tr("investigation.not_compared").to_string(), true)),
        }
        lines.extend(self.differences.iter().map(|text| (text.clone(), true)));
        lines.extend(self.incomplete_sources.iter().map(|text| (text.clone(), true)));
        if !self.missing_seeds.is_empty() {
            lines.push((trf("investigation.missing_seeds", &[&names(&self.missing_seeds)]), true));
        }
        if self.target_missing {
            lines.push((tr("investigation.target_missing").to_string(), true));
        } else if self.path_same == Some(false) {
            lines.push((tr("investigation.path_differs").to_string(), true));
        }
        if !self.unsure.is_empty() {
            lines.push((trf("investigation.unsure", &[&names(&self.unsure)]), true));
        }
        lines.push((trf("investigation.reopened", &[&self.name, &format_unix_time(self.saved_at)]), false));
        lines
    }
}

/// At most this many views are kept; the oldest goes first.
const HISTORY: usize = 20;

pub struct Explorer {
    pub direction: Direction,
    /// Empty follows every type.
    pub types: BTreeSet<String>,
    pub max_hops: u32,
    /// Added starts; with none, a walk starts from the selected node.
    pub starts: BTreeSet<i64>,
    /// Relationship types in the graph, to offer as filters.
    pub available: Vec<String>,
    /// Set while a walk runs; setting the flag stops it at its next query.
    pub running: Option<Arc<AtomicBool>>,
    pub walk: Option<Exploration>,
    /// A walk that failed; the canvas keeps what it had.
    pub error: Option<String>,
    /// Analyses that plugins offer, and which apply is up to their labels.
    pub analyses: Vec<Offered>,
    /// The last run of each analysis by [`analysis_key`], kept until the canvas
    /// gets a new result.
    pub reports: BTreeMap<String, Report>,
    /// Those running; setting the flag ends the plugin.
    pub checking: BTreeMap<String, Arc<AtomicBool>>,
    pub analysis_errors: BTreeMap<String, String>,
    /// The reopened investigation this walk came from.
    pub restored: Option<Restored>,
    /// The finding path on the canvas, as hops and as what to save for it.
    pub finding_path: Option<(Vec<Hop>, Vec<HopRef>)>,
    history: Vec<View>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            direction: Direction::Out,
            types: BTreeSet::new(),
            max_hops: DEFAULT_MAX_HOPS,
            starts: BTreeSet::new(),
            available: Vec::new(),
            running: None,
            walk: None,
            error: None,
            analyses: Vec::new(),
            reports: BTreeMap::new(),
            checking: BTreeMap::new(),
            analysis_errors: BTreeMap::new(),
            restored: None,
            finding_path: None,
            history: Vec::new(),
        }
    }
}

impl Explorer {
    /// What to walk from `selected`, unless no node was chosen.
    pub fn request(&self, selected: Option<i64>) -> Option<ExploreRequest> {
        let starts: Vec<i64> = if self.starts.is_empty() {
            vec![selected?]
        } else {
            self.starts.iter().copied().collect()
        };
        Some(ExploreRequest {
            direction: self.direction,
            rel_types: self.types.iter().cloned().collect(),
            max_hops: self.max_hops,
            ..ExploreRequest::new(starts)
        })
    }

    pub fn change_hops(&mut self, by: i32) {
        self.max_hops = self.max_hops.saturating_add_signed(by).clamp(1, MAX_HOPS);
    }

    pub fn toggle_type(&mut self, name: &str) {
        if !self.types.remove(name) {
            self.types.insert(name.to_string());
        }
    }

    pub fn toggle_start(&mut self, id: i64) {
        if !self.starts.remove(&id) {
            self.starts.insert(id);
        }
    }

    pub fn push_view(&mut self, view: View) {
        if self.history.len() == HISTORY {
            self.history.remove(0);
        }
        self.history.push(view);
    }

    pub fn pop_view(&mut self) -> Option<View> {
        self.history.pop()
    }

    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    /// A new result on the canvas: nothing here describes it any more.
    /// Filters stay, since they are what the person is looking for.
    pub fn forget_walks(&mut self) {
        self.walk = None;
        self.error = None;
        self.reports.clear();
        self.analysis_errors.clear();
        // What they would report no longer describes the canvas.
        for (_, cancel) in std::mem::take(&mut self.checking) {
            cancel.store(true, Ordering::Relaxed);
        }
        self.restored = None;
        self.finding_path = None;
        self.starts.clear();
        self.history.clear();
    }

    /// The warnings about the last walk, for the canvas to show when no node is
    /// selected and the panel is out of sight.
    pub fn warning(&self) -> Option<String> {
        let warnings: Vec<String> = self.status_lines().into_iter().filter(|(_, warn)| *warn).map(|(line, _)| line).collect();
        (!warnings.is_empty()).then(|| warnings.join(" · "))
    }

    /// Says what the last walk found and everything that limited it. These
    /// sentences are what stops an empty answer reading as "nothing is there".
    pub fn status_lines(&self) -> Vec<(String, bool)> {
        let mut lines = Vec::new();
        if let Some(restored) = &self.restored {
            lines.extend(restored.status_lines());
        }
        if let Some(error) = &self.error {
            lines.push((trf("explore.failed", &[error]), true));
        }
        let Some(walk) = &self.walk else {
            return lines;
        };
        if walk.hops.is_empty() && walk.is_complete() {
            lines.push((tr("explore.none").to_string(), false));
        } else {
            lines.push((
                trf(
                    "explore.found",
                    &[&walk.graph.nodes.len().to_string(), &walk.graph.edges.len().to_string(), &walk.depth_reached.to_string()],
                ),
                false,
            ));
        }
        for limit in &walk.limits {
            let key = match limit {
                Limit::Hops => "explore.limit.hops",
                Limit::Nodes => "explore.limit.nodes",
                Limit::Edges => "explore.limit.edges",
            };
            lines.push((tr(key).to_string(), true));
        }
        if walk.cancelled {
            lines.push((tr("explore.cancelled").to_string(), true));
        }
        if !walk.missing_starts.is_empty() {
            lines.push((tr("explore.missing_start").to_string(), true));
        }
        lines
    }
}

/// The hops to `target` in words, when the walk reached it from a start.
pub fn explained_path(walk: &Exploration, target: i64) -> Vec<String> {
    walk.path_to(target).unwrap_or_default().into_iter().map(|hop| walk.explain(hop)).collect()
}

/// A source's last run in words, for the inspector.
pub fn coverage_text(coverage: &SourceCoverage) -> String {
    let status = match coverage.status {
        RunStatus::Complete => tr("evidence.coverage.complete"),
        RunStatus::Partial => tr("evidence.coverage.partial"),
        RunStatus::Failed => tr("evidence.coverage.failed"),
        RunStatus::Cancelled => tr("evidence.coverage.cancelled"),
    };
    format!("{status} ({})", coverage.run_id)
}

/// The evidence for a record, as the rows the inspector shows.
pub fn evidence_rows(record: &Evidence) -> Vec<(&'static str, String)> {
    let unknown = || tr("evidence.unknown").to_string();
    let text = |field: &Field<String>| field.known().cloned().unwrap_or_else(unknown);
    let time = |field: &Field<u64>| field.known().map(|at| format_unix_time(*at)).unwrap_or_else(unknown);
    let assertion = match record.assertion {
        Assertion::Explicit => tr("evidence.explicit"),
        Assertion::Derived => tr("evidence.derived"),
        Assertion::Unknown => tr("evidence.unknown"),
    };
    vec![
        ("evidence.id", record.evidence_id.clone()),
        ("evidence.source", text(&record.source)),
        ("evidence.plugin", text(&record.plugin)),
        ("evidence.written", time(&record.record_written_at)),
        ("evidence.last_sync", time(&record.source_last_successful_sync)),
        ("evidence.coverage", record.source_coverage.known().map_or_else(unknown, |coverage| coverage_text(coverage))),
        ("evidence.snapshot", text(&record.source_snapshot)),
        ("evidence.assertion", assertion.to_string()),
    ]
}

/// The findings about `node`, in the report's order.
pub fn findings_for<'a>(report: &'a Report, node: &GraphNode) -> Vec<&'a Finding> {
    let resource = khonsu_core::evidence::for_node(node, &[]).evidence_id;
    report.findings.iter().filter(|finding| finding.resource == resource).collect()
}

impl Explorer {
    /// What to save as the exact path when saving at `node`: the finding path, if
    /// the walk is still that path and `node` is where it ends.
    pub fn exact_path_to(&self, node: i64) -> Vec<HopRef> {
        let current = self.walk.as_ref().map(|walk| walk.hops.as_slice());
        match &self.finding_path {
            Some((hops, refs)) if current == Some(hops.as_slice()) && hops.last().is_some_and(|hop| hop.to == node) => refs.clone(),
            _ => Vec::new(),
        }
    }
}

/// Which run of an analysis this is about: plugins each name their own.
pub fn analysis_key(offered: &Offered) -> String {
    format!("{}/{}", offered.plugin.id(), offered.info.id)
}

/// A plugin's text in the window's language, falling back on what it wrote.
pub fn analysis_text(info: &AnalysisInfo, key: &str, fallback: &str) -> String {
    info.translated(crate::i18n::current().code(), key).unwrap_or(fallback).to_string()
}

/// A verdict's words, from the analysis that gave it.
pub fn verdict_text(info: &AnalysisInfo, id: &str) -> String {
    let title = info.verdicts.iter().find(|verdict| verdict.id == id).map_or(id, |verdict| verdict.title.as_str());
    analysis_text(info, &format!("verdict.{id}"), title)
}

/// A rule's title, from the analysis that has it.
pub fn rule_text(info: &AnalysisInfo, id: &str) -> String {
    let title = info.rules.iter().find(|rule| rule.rule_id == id).map_or(id, |rule| rule.title.as_str());
    analysis_text(info, &format!("rule.{id}"), title)
}

pub fn status_key(status: ConditionStatus) -> &'static str {
    match status {
        ConditionStatus::Met => "analysis.status.met",
        ConditionStatus::NotMet => "analysis.status.not_met",
        ConditionStatus::Unknown => "analysis.status.unknown",
    }
}

/// Only the nodes and relationships on the path to `target`.
pub fn path_graph(walk: &Exploration, target: i64) -> Option<GraphData> {
    let path = walk.path_to(target).filter(|path| !path.is_empty())?;
    let mut ids: BTreeSet<i64> = path.iter().flat_map(|hop| [hop.from, hop.to]).collect();
    ids.insert(target);
    let edges: BTreeSet<i64> = path.iter().map(|hop| hop.edge).collect();
    Some(GraphData {
        nodes: walk.graph.nodes.iter().filter(|node| ids.contains(&node.id)).cloned().collect(),
        edges: walk.graph.edges.iter().filter(|edge| edges.contains(&edge.id)).cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use khonsu_core::explore::Hop;
    use serde_json::{json, Map};

    fn node(id: i64, name: &str) -> GraphNode {
        GraphNode {
            id,
            labels: vec!["N".into()],
            properties: Map::from_iter([("name".to_string(), json!(name))]),
        }
    }

    fn edge(id: i64, source: i64, target: i64) -> GraphEdge {
        GraphEdge {
            id,
            source,
            target,
            rel_type: "NEXT".into(),
            properties: Map::new(),
        }
    }

    fn hop(depth: u32, from: i64, to: i64, edge: i64) -> Hop {
        Hop {
            depth,
            from,
            to,
            edge,
            rel_type: "NEXT".into(),
            along: true,
        }
    }

    /// `a → b → c`, and `a → d` off to the side.
    fn walk() -> Exploration {
        Exploration {
            starts: vec![1],
            graph: GraphData {
                nodes: vec![node(1, "a"), node(2, "b"), node(3, "c"), node(4, "d")],
                edges: vec![edge(10, 1, 2), edge(11, 2, 3), edge(12, 1, 4)],
            },
            hops: vec![hop(1, 1, 2, 10), hop(1, 1, 4, 12), hop(2, 2, 3, 11)],
            depth_reached: 2,
            ..Exploration::default()
        }
    }

    fn offered() -> Offered {
        Offered {
            plugin: khonsu_core::plugin::PluginInfo {
                manifest: serde_json::from_value(json!({"id": "demo", "name": "Demo", "version": "1", "protocol": 1, "entry": ["demo"]})).unwrap(),
                dir: Default::default(),
                builtin: true,
            },
            info: serde_json::from_value(json!({
                "id": "links",
                "title": "Links",
                "subject_labels": ["Thing"],
                "verdicts": [{"id": "flagged", "title": "Flagged", "tone": "attention"}],
                "rules": [{"rule_id": "demo.rule", "title": "A rule", "statement": "It holds."}],
                "translations": {"zh": {"verdict.flagged": "\u{5df2}\u{6807}\u{8bb0}"}},
            }))
            .unwrap(),
        }
    }

    fn report() -> Report {
        let finding: Finding = serde_json::from_value(json!({
            "finding_id": "f", "rule_id": "demo.rule", "verdict": "flagged",
            "resource": "node:db", "resource_name": "db",
            "conditions": [], "unknowns": [], "assumptions": [],
            "path": null, "walk": null, "contributing": [], "evidence": [],
        }))
        .unwrap();
        Report {
            plugin: "demo".into(),
            analysis: "links".into(),
            title: "Links".into(),
            claim: String::new(),
            order: String::new(),
            verdicts: vec![],
            rules: vec![],
            subjects: 1,
            summary: BTreeMap::new(),
            complete: true,
            sources: BTreeMap::new(),
            findings: vec![finding],
        }
    }

    #[test]
    fn a_plugins_words_are_used_with_its_own_as_the_fallback() {
        let info = offered().info;
        assert_eq!(analysis_text(&info, "no.such.key", "fallback"), "fallback");
        assert_eq!(rule_text(&info, "demo.rule"), "A rule");
        assert_eq!(rule_text(&info, "unheard.of"), "unheard.of", "an ID is better than nothing");
        assert_eq!(verdict_text(&info, "unheard_of"), "unheard_of");
        assert_eq!(analysis_key(&offered()), "demo/links");
        for status in [ConditionStatus::Met, ConditionStatus::NotMet, ConditionStatus::Unknown] {
            assert!(!tr(status_key(status)).is_empty());
        }
    }

    /// The window goes by what a plugin declares, so a label it never heard of
    /// is offered the same way as any other.
    #[test]
    fn an_analysis_is_offered_on_the_labels_it_declares_and_nothing_is_hard_coded() {
        let on = offered();
        assert!(on.applies_to(&["Thing".to_string()]));
        assert!(!on.applies_to(&["Instance".to_string()]));
        for (name, source) in [("explorer", include_str!("explorer.rs")), ("canvas", include_str!("canvas.rs"))] {
            let code = source.split("#[cfg(test)]").next().unwrap();
            for provider in ["aws.exposure", "\"Instance\"", "SecurityGroup", "potential_exposure"] {
                assert!(!code.contains(provider), "{name} names {provider}");
            }
        }
    }

    #[test]
    fn findings_are_matched_to_the_selected_node_and_a_new_result_drops_them() {
        let report = report();
        let db = GraphNode {
            id: 8,
            labels: vec!["Thing".into()],
            properties: Map::from_iter([("khonsu_id".to_string(), json!("db"))]),
        };
        let found = findings_for(&report, &db);
        assert_eq!(found.len(), 1);
        assert!(findings_for(&report, &node(99, "elsewhere")).is_empty());

        let running = Arc::new(AtomicBool::new(false));
        let mut explorer = Explorer {
            reports: BTreeMap::from([("demo/links".to_string(), report)]),
            analysis_errors: BTreeMap::from([("demo/links".to_string(), "x".to_string())]),
            checking: BTreeMap::from([("demo/links".to_string(), running.clone())]),
            analyses: vec![offered()],
            ..Explorer::default()
        };
        explorer.forget_walks();
        assert!(explorer.reports.is_empty() && explorer.analysis_errors.is_empty(), "the graph may have changed");
        assert!(explorer.checking.is_empty() && running.load(Ordering::Relaxed), "and what is running is stopped");
        assert_eq!(explorer.analyses.len(), 1, "what plugins offer has not changed");
    }

    fn restored() -> Restored {
        Restored {
            name: "web tier".into(),
            saved_at: 1000,
            graph_state: GraphState::Unchanged,
            differences: vec![],
            missing_seeds: vec![],
            target_missing: false,
            unsure: vec![],
            path_same: Some(true),
            incomplete_sources: vec![],
        }
    }

    #[test]
    fn a_reopened_investigation_says_nothing_alarming_when_nothing_changed() {
        let lines = restored().status_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(!lines[0].1 && lines[0].0.contains("web tier"), "{lines:?}");
    }

    #[test]
    fn every_difference_from_when_it_was_saved_is_a_warning() {
        let record = |id: &str| RecordRef { stable_id: Some(id.into()), internal_id: 1 };
        let changed = Restored {
            graph_state: GraphState::Changed,
            differences: vec!["Nodes: 1 when saved, 2 now.".into()],
            missing_seeds: vec![record("shop/gone")],
            unsure: vec![RecordRef { stable_id: None, internal_id: 7 }],
            path_same: Some(false),
            ..restored()
        };
        let lines = changed.status_lines();
        let warned = |text: &str| lines.iter().any(|(line, warn)| *warn && line.contains(text));
        assert!(warned(tr("investigation.changed")), "{lines:?}");
        assert!(warned("Nodes: 1 when saved"), "{lines:?}");
        assert!(warned("shop/gone"), "{lines:?}");
        assert!(warned("#7"), "{lines:?}");
        assert!(warned(tr("investigation.path_differs")), "{lines:?}");

        // The target having gone says more than the path differing.
        let gone = Restored { target_missing: true, path_same: Some(false), ..restored() }.status_lines();
        assert!(gone.iter().any(|(line, warn)| *warn && line == tr("investigation.target_missing")), "{gone:?}");
        assert!(!gone.iter().any(|(line, _)| line == tr("investigation.path_differs")), "{gone:?}");

        // Too large to compare is not the same as unchanged.
        let unsure = Restored { graph_state: GraphState::NotCompared, ..restored() }.status_lines();
        assert_eq!(unsure[0], (tr("investigation.not_compared").to_string(), true));
    }

    #[test]
    fn the_warnings_of_a_reopened_investigation_reach_the_canvas_and_a_new_result_clears_them() {
        let mut explorer = Explorer {
            restored: Some(Restored { graph_state: GraphState::Changed, ..restored() }),
            ..Explorer::default()
        };
        assert_eq!(explorer.warning().as_deref(), Some(tr("investigation.changed")));

        // With no node selected this is all that shows, so a lost path must be in it.
        explorer.restored = Some(Restored { graph_state: GraphState::Changed, path_same: Some(false), ..restored() });
        assert!(explorer.warning().unwrap().contains(tr("investigation.path_differs")));

        explorer.forget_walks();
        assert!(explorer.restored.is_none() && explorer.warning().is_none());
    }

    /// Saving a finding's path keeps that path, not the fewest hops a walk
    /// would find. Once the walk or the selection moves on it is a normal save.
    #[test]
    fn only_the_finding_path_still_on_the_canvas_is_saved_exactly() {
        let hops = vec![hop(1, 1, 2, 10), hop(2, 2, 3, 11)];
        let refs = vec![HopRef { edge: "rel:ab".into(), along: true }, HopRef { edge: "rel:bc".into(), along: true }];
        let mut explorer = Explorer {
            walk: Some(Exploration { hops: hops.clone(), ..Exploration::default() }),
            finding_path: Some((hops, refs.clone())),
            ..Explorer::default()
        };
        assert_eq!(explorer.exact_path_to(3), refs);
        assert!(explorer.exact_path_to(2).is_empty(), "another node on it means the path to that node");
        explorer.walk = Some(walk());
        assert!(explorer.exact_path_to(3).is_empty(), "another walk replaced it");
        explorer.forget_walks();
        assert!(explorer.finding_path.is_none());
    }

    #[test]
    fn evidence_rows_say_not_supplied_for_what_is_missing() {
        let record = khonsu_core::evidence::for_node(&node(1, "by hand"), &[]);
        let rows = evidence_rows(&record);
        let value = |key: &str| rows.iter().find(|(row, _)| *row == key).unwrap().1.clone();
        assert_eq!(value("evidence.id"), "node-internal:1");
        for key in ["evidence.source", "evidence.plugin", "evidence.written", "evidence.last_sync", "evidence.snapshot", "evidence.assertion"] {
            assert_eq!(value(key), tr("evidence.unknown"), "{key}");
        }
    }

    #[test]
    fn a_walk_starts_from_the_selection_unless_starts_were_added() {
        let mut explorer = Explorer::default();
        assert!(explorer.request(None).is_none());
        assert_eq!(explorer.request(Some(7)).unwrap().starts, [7]);

        explorer.toggle_start(2);
        explorer.toggle_start(1);
        assert_eq!(explorer.request(Some(7)).unwrap().starts, [1, 2]);
        assert_eq!(explorer.request(None).unwrap().starts, [1, 2]);

        explorer.toggle_start(1);
        assert_eq!(explorer.request(None).unwrap().starts, [2]);
    }

    #[test]
    fn the_request_carries_the_filters_and_hops_stay_in_range() {
        let mut explorer = Explorer {
            direction: Direction::In,
            ..Explorer::default()
        };
        explorer.toggle_type("CALLS");
        explorer.toggle_type("RUNS");
        explorer.toggle_type("RUNS");
        for _ in 0..40 {
            explorer.change_hops(1);
        }
        assert_eq!(explorer.max_hops, MAX_HOPS);
        for _ in 0..40 {
            explorer.change_hops(-1);
        }
        assert_eq!(explorer.max_hops, 1);

        let request = explorer.request(Some(1)).unwrap();
        assert_eq!(request.direction, Direction::In);
        assert_eq!(request.rel_types, ["CALLS"]);
        assert_eq!(request.max_hops, 1);
    }

    #[test]
    fn going_back_returns_views_newest_first_and_keeps_a_bounded_number() {
        let mut explorer = Explorer::default();
        assert!(!explorer.can_go_back());
        let view = |id: i64| View {
            nodes: vec![node(id, "x")],
            edges: vec![],
            walk: None,
            selected: None,
        };
        for id in 0..(HISTORY as i64 + 5) {
            explorer.push_view(view(id));
        }
        assert_eq!(explorer.pop_view().unwrap().nodes[0].id, HISTORY as i64 + 4);
        let mut remaining = 1;
        let mut oldest = None;
        while let Some(view) = explorer.pop_view() {
            remaining += 1;
            oldest = Some(view.nodes[0].id);
        }
        assert_eq!(remaining, HISTORY);
        assert_eq!(oldest, Some(5));
    }

    #[test]
    fn a_new_result_forgets_walks_but_keeps_the_filters() {
        let mut explorer = Explorer {
            walk: Some(walk()),
            error: Some("x".into()),
            max_hops: 5,
            ..Explorer::default()
        };
        explorer.toggle_start(1);
        explorer.toggle_type("CALLS");
        explorer.push_view(View { nodes: vec![], edges: vec![], walk: None, selected: None });
        explorer.forget_walks();
        assert!(explorer.walk.is_none() && explorer.error.is_none());
        assert!(explorer.starts.is_empty() && !explorer.can_go_back());
        assert_eq!(explorer.max_hops, 5);
        assert!(explorer.types.contains("CALLS"));
    }

    #[test]
    fn a_path_keeps_only_its_own_nodes_and_relationships() {
        let path = path_graph(&walk(), 3).unwrap();
        let ids: Vec<i64> = path.nodes.iter().map(|node| node.id).collect();
        let edges: Vec<i64> = path.edges.iter().map(|edge| edge.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        assert_eq!(edges, [10, 11]);
        assert_eq!(explained_path(&walk(), 3), ["a -[NEXT]-> b", "b -[NEXT]-> c"]);

        assert!(path_graph(&walk(), 1).is_none(), "a start has no path to show");
        assert!(path_graph(&walk(), 99).is_none(), "not reached");
        assert!(explained_path(&walk(), 99).is_empty());
    }

    #[test]
    fn an_empty_or_cut_answer_says_which_it_is() {
        let lines = |walk: Exploration, error: Option<&str>| {
            let explorer = Explorer {
                walk: Some(walk),
                error: error.map(String::from),
                ..Explorer::default()
            };
            explorer.status_lines()
        };
        let none = lines(Exploration::default(), None);
        assert_eq!(none, [(tr("explore.none").to_string(), false)]);

        // Empty because it was stopped is a warning, not "no relationships".
        let cancelled = lines(Exploration { cancelled: true, ..Exploration::default() }, None);
        assert!(cancelled.iter().all(|(line, _)| line != tr("explore.none")));
        assert!(cancelled.contains(&(tr("explore.cancelled").to_string(), true)));

        let capped = lines(
            Exploration {
                limits: vec![Limit::Hops, Limit::Nodes, Limit::Edges],
                ..walk()
            },
            None,
        );
        for key in ["explore.limit.hops", "explore.limit.nodes", "explore.limit.edges"] {
            assert!(capped.contains(&(tr(key).to_string(), true)), "{key}");
        }

        let missing = lines(Exploration { missing_starts: vec![9], ..Exploration::default() }, None);
        assert!(missing.contains(&(tr("explore.missing_start").to_string(), true)));

        let failed = lines(Exploration::default(), Some("boom"));
        assert!(failed[0].1 && failed[0].0.contains("boom"));
    }

    #[test]
    fn only_an_incomplete_walk_warns_on_the_canvas() {
        let with = |walk: Exploration| Explorer { walk: Some(walk), ..Explorer::default() };
        assert!(Explorer::default().warning().is_none());
        assert!(with(walk()).warning().is_none());
        let cut = with(Exploration { limits: vec![Limit::Hops], ..walk() });
        assert_eq!(cut.warning().as_deref(), Some(tr("explore.limit.hops")));
    }
}

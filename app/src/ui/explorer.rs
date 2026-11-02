//! Path Explorer's state, apart from the canvas that draws it: what to walk,
//! what the last walk found, and the views to go back to.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use khonsu_core::evidence::{Assertion, Evidence, Field};
use khonsu_core::explore::{Direction, Exploration, ExploreRequest, Limit, DEFAULT_MAX_HOPS, MAX_HOPS};
use khonsu_core::exposure::{Finding, Report, Status, Verdict, RULE_BALANCER, RULE_DIRECT, RULE_LATERAL};
use khonsu_core::investigation::{GraphState, Investigation, RecordRef, Reopened};
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
    /// The last exposure check, kept until the canvas gets a new result.
    pub exposure: Option<Report>,
    pub checking_exposure: bool,
    pub exposure_error: Option<String>,
    /// The reopened investigation this walk came from.
    pub restored: Option<Restored>,
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
            exposure: None,
            checking_exposure: false,
            exposure_error: None,
            restored: None,
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
        self.exposure = None;
        self.exposure_error = None;
        self.restored = None;
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
        ("evidence.snapshot", text(&record.source_snapshot)),
        ("evidence.assertion", assertion.to_string()),
    ]
}

/// The findings about `node`, in the report's order.
pub fn findings_for<'a>(report: &'a Report, node: &GraphNode) -> Vec<&'a Finding> {
    let resource = khonsu_core::evidence::for_node(node, &[]).evidence_id;
    report.findings.iter().filter(|finding| finding.resource == resource).collect()
}

pub fn verdict_key(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::PotentialExposure => "exposure.verdict.potential",
        Verdict::InsufficientData => "exposure.verdict.insufficient",
        Verdict::NotIndicated => "exposure.verdict.not_indicated",
    }
}

pub fn status_key(status: Status) -> &'static str {
    match status {
        Status::Met => "exposure.status.met",
        Status::NotMet => "exposure.status.not_met",
        Status::Unknown => "exposure.status.unknown",
    }
}

/// A rule's title in the window's language.
pub fn rule_key(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        RULE_DIRECT => Some("exposure.rule.direct"),
        RULE_BALANCER => Some("exposure.rule.balancer"),
        RULE_LATERAL => Some("exposure.rule.lateral"),
        _ => None,
    }
}

/// A condition's statement in the window's language.
pub fn condition_key(id: &str) -> Option<&'static str> {
    Some(match id {
        "instance-running" => "exposure.cond.running",
        "public-address" => "exposure.cond.address",
        "open-inbound-rule" => "exposure.cond.open_rule",
        "internet-route" => "exposure.cond.route",
        "internet-facing" => "exposure.cond.facing",
        "balancer-open-rule" => "exposure.cond.balancer_open",
        "target-healthy" => "exposure.cond.healthy",
        "target-allows-balancer" => "exposure.cond.target_allows",
        "source-potentially-exposed" => "exposure.cond.source",
        "group-allows-source" => "exposure.cond.group_allows",
        _ => return None,
    })
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

    /// A public balancer in front of `app`, and `db` allowing `app`'s group, so
    /// that every rule produces a finding.
    fn exposure_report() -> Report {
        let at = |id: i64, label: &str, extra: serde_json::Value| GraphNode {
            id,
            labels: vec![label.into()],
            properties: extra.as_object().unwrap().clone(),
        };
        let link = |id: i64, source: i64, rel: &str, target: i64, extra: serde_json::Value| GraphEdge {
            id,
            source,
            target,
            rel_type: rel.into(),
            properties: extra.as_object().unwrap().clone(),
        };
        let data = GraphData {
            nodes: vec![
                at(1, "Cidr", json!({"khonsu_id": "net", "cidr": "0.0.0.0/0"})),
                at(2, "SecurityGroup", json!({"khonsu_id": "sg-lb"})),
                at(3, "SecurityGroup", json!({"khonsu_id": "sg-app"})),
                at(4, "SecurityGroup", json!({"khonsu_id": "sg-db"})),
                at(5, "LoadBalancer", json!({"khonsu_id": "lb", "scheme": "internet-facing"})),
                at(6, "TargetGroup", json!({"khonsu_id": "tg", "port": 80})),
                at(7, "Instance", json!({"khonsu_id": "app", "name": "app", "state": "running"})),
                at(8, "Instance", json!({"khonsu_id": "db", "name": "db", "state": "running"})),
            ],
            edges: vec![
                link(1, 1, "ROUTES_TO", 5, json!({"khonsu_id": "a"})),
                link(2, 1, "ALLOWS", 2, json!({"khonsu_id": "b", "protocol": "tcp", "ports": "80"})),
                link(3, 5, "PROTECTED_BY", 2, json!({"khonsu_id": "c"})),
                link(4, 5, "FORWARDS_TO", 6, json!({"khonsu_id": "d"})),
                link(5, 6, "ROUTES_TO", 7, json!({"khonsu_id": "e", "health": "healthy"})),
                link(6, 7, "PROTECTED_BY", 3, json!({"khonsu_id": "f"})),
                link(7, 2, "ALLOWS", 3, json!({"khonsu_id": "g", "protocol": "tcp", "ports": "80"})),
                link(8, 8, "PROTECTED_BY", 4, json!({"khonsu_id": "h"})),
                link(9, 3, "ALLOWS", 4, json!({"khonsu_id": "i", "protocol": "tcp", "ports": "5432"})),
            ],
        };
        khonsu_core::exposure::evaluate(&data, &[], true)
    }

    #[test]
    fn every_rule_and_condition_has_words_in_the_window() {
        let report = exposure_report();
        assert_eq!(report.summary.potential_exposure, 2, "{report:#?}");
        let rules: BTreeSet<&str> = report.findings.iter().map(|finding| finding.rule_id.as_str()).collect();
        assert_eq!(rules.len(), 3, "the test should reach every rule: {rules:?}");
        for finding in &report.findings {
            let title = rule_key(&finding.rule_id).unwrap_or_else(|| panic!("no title for {}", finding.rule_id));
            assert_ne!(tr(title), title, "{title} is not in the table");
            for condition in &finding.conditions {
                let key = condition_key(&condition.id).unwrap_or_else(|| panic!("no words for {}", condition.id));
                assert_ne!(tr(key), key, "{key} is not in the table");
            }
        }
        for key in [Verdict::PotentialExposure, Verdict::InsufficientData, Verdict::NotIndicated].map(verdict_key) {
            assert_ne!(tr(key), key);
        }
        for key in [Status::Met, Status::NotMet, Status::Unknown].map(status_key) {
            assert_ne!(tr(key), key);
        }
    }

    #[test]
    fn findings_are_matched_to_the_selected_node_and_a_new_result_drops_them() {
        let report = exposure_report();
        let db = GraphNode {
            id: 8,
            labels: vec!["Instance".into()],
            properties: Map::from_iter([("khonsu_id".to_string(), json!("db"))]),
        };
        let found = findings_for(&report, &db);
        assert!(!found.is_empty());
        assert!(found.iter().all(|finding| finding.resource == "node:db"));
        assert!(findings_for(&report, &node(99, "elsewhere")).is_empty());

        let mut explorer = Explorer {
            exposure: Some(report),
            exposure_error: Some("x".into()),
            ..Explorer::default()
        };
        explorer.forget_walks();
        assert!(explorer.exposure.is_none() && explorer.exposure_error.is_none(), "the graph may have changed");
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

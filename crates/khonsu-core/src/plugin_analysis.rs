//! Analyses that plugins offer over a graph.
//!
//! The boundary: a plugin owns what its findings mean, and the host owns the
//! graph. The host reads a bounded part of the graph, sends it with stable
//! references to each record, and checks what comes back. A field's value, a
//! path's records and a finding's evidence are all rebuilt here from the graph,
//! so a plugin can only point at records, not describe them. Nothing here knows
//! any provider's labels, rules or verdicts. See `docs/plugins.md`.

use crate::engine::{Graph, QueryError};
use crate::evidence::{self, Evidence, Field, PathEvidence, SourceCoverage};
use crate::merge::SOURCE_PROPERTY;
use crate::explore::{is_identifier, node_name, Direction, Exploration, Hop};
use crate::plugin::{self, PluginError, PluginInfo, SyncEvent};
use crate::protocol::{
    AnalysisEdge, AnalysisInfo, AnalysisNode, AnalyzeRequest, ConditionStatus, FindingRecord, HopRef, RuleInfo, VerdictInfo,
    PROTOCOL_VERSION,
};
use crate::result::{value_to_json, GraphCollector, GraphData, GraphEdge, GraphNode};
use crate::settings::{RunStatus, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;

/// Findings kept in a report unless asked otherwise; the rest are counted.
pub const DEFAULT_MAX_FINDINGS: usize = 200;
/// Records read of each label or type. A larger graph gives an incomplete report.
const MAX_RECORDS: usize = 50_000;

/// An exact field a condition read, with its value as the graph holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceField {
    /// The evidence ID of the record that holds it.
    pub record: String,
    pub field: String,
    /// What the source calls it.
    pub source_field: String,
    pub value: Field<Json>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub id: String,
    pub statement: String,
    pub status: ConditionStatus,
    /// What was found, in words.
    pub detail: String,
    pub fields: Vec<SourceField>,
}

/// The `khonsu explore` walk that shows a finding's records in Path Explorer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkHint {
    /// A `khonsu_id`, or an internal ID when the record has none.
    pub from: String,
    pub to: String,
    pub direction: Direction,
    pub types: Vec<String>,
    pub max_hops: u32,
}

/// A finding checked against the graph, with its records looked up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub finding_id: String,
    pub rule_id: String,
    pub verdict: String,
    /// The evidence ID of the resource it is about, and what people call it.
    pub resource: String,
    pub resource_name: String,
    pub conditions: Vec<Condition>,
    pub unknowns: Vec<String>,
    pub assumptions: Vec<String>,
    /// The candidate path, when the records for one exist. Its `rule_id` is known.
    pub path: Option<PathEvidence>,
    /// The same path as stable references, which is how it crosses between
    /// processes and into a saved file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_refs: Option<Vec<HopRef>>,
    /// That path as hops over this graph's internal IDs.
    #[serde(skip)]
    pub hops: Vec<Hop>,
    /// The records those hops cross, for drawing the path.
    #[serde(skip)]
    pub path_graph: GraphData,
    pub walk: Option<WalkHint>,
    /// The finding this one rests on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// Sources behind its records whose last sync was not complete. Worked out
    /// here, so it shows whatever the plugin made of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete_sources: Vec<String>,
    /// The evidence IDs of every record the conditions read or the path crosses.
    pub contributing: Vec<String>,
    pub evidence: Vec<Evidence>,
}

impl Finding {
    /// The candidate path as a walk, for Path Explorer to show.
    pub fn exploration(&self) -> Option<Exploration> {
        let start = self.hops.first()?.from;
        Some(Exploration {
            starts: vec![start],
            graph: self.path_graph.clone(),
            hops: self.hops.clone(),
            depth_reached: self.hops.len() as u32,
            ..Exploration::default()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// The plugin and analysis that produced this.
    pub plugin: String,
    pub analysis: String,
    pub title: String,
    pub claim: String,
    pub order: String,
    pub verdicts: Vec<VerdictInfo>,
    pub rules: Vec<RuleInfo>,
    /// Resources looked at.
    pub subjects: u64,
    /// Findings by verdict, counting any cut by a cap.
    pub summary: BTreeMap<String, usize>,
    /// False when the graph was too large to read whole, or a source's last
    /// sync was not complete: a missing finding then means nothing.
    pub complete: bool,
    /// How much of each source its last sync covered.
    pub sources: BTreeMap<String, Field<SourceCoverage>>,
    pub findings: Vec<Finding>,
}

impl Report {
    /// Keeps the findings `keep` accepts, up to `max`, and counts what it cut.
    pub fn narrowed(mut self, keep: impl Fn(&Finding) -> bool, max: usize) -> (Self, usize) {
        self.findings.retain(|finding| keep(finding));
        let cut = self.findings.len().saturating_sub(max);
        self.findings.truncate(max);
        (self, cut)
    }
}

/// An analysis a plugin offers, with the plugin that runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offered {
    pub plugin: PluginInfo,
    pub info: AnalysisInfo,
}

impl Offered {
    /// Whether this analysis can say anything about a node with these labels.
    pub fn applies_to(&self, labels: &[String]) -> bool {
        labels.iter().any(|label| self.info.subject_labels.contains(label))
    }
}

/// Every analysis on offer from plugins that may run. A plugin not approved as
/// it is now is not started just to ask, and one that fails to describe itself
/// offers nothing.
pub fn offered() -> Vec<Offered> {
    plugin::list()
        .into_iter()
        .filter(|plugin| plugin::require_approved(plugin).is_ok())
        .flat_map(|plugin| {
            let analyses = plugin::describe(&plugin).map(|describe| describe.analyses).unwrap_or_default();
            analyses.into_iter().filter(|info| check_info(info).is_ok()).map(move |info| Offered { plugin: plugin.clone(), info })
        })
        .collect()
}

/// Finds one analysis of a plugin, checking the plugin may run first.
pub fn find(plugin_id: &str, analysis: &str) -> Result<Offered, PluginError> {
    let plugin = plugin::find(plugin_id)?;
    plugin::require_approved(&plugin)?;
    let analyses = plugin::describe(&plugin)?.analyses;
    let names: Vec<&str> = analyses.iter().map(|info| info.id.as_str()).collect();
    let Some(info) = analyses.iter().find(|info| info.id == analysis) else {
        let offers = if names.is_empty() { "offers no analyses".to_string() } else { format!("offers {}", names.join(", ")) };
        return Err(PluginError::Other(format!("plugin {plugin_id} has no analysis {analysis:?}: it {offers}")));
    };
    check_info(info).map_err(|message| PluginError::Failed { plugin: plugin_id.to_string(), message })?;
    Ok(Offered { info: info.clone(), plugin })
}

/// A label or type goes into query text, so it must be a plain name.
fn check_info(info: &AnalysisInfo) -> Result<(), String> {
    let names = info.node_labels.iter().chain(&info.relationship_types);
    if let Some(bad) = names.into_iter().find(|name| !is_identifier(name)) {
        return Err(format!("analysis {} asks to read {bad:?}, which is not a label or type name", info.id));
    }
    if info.verdicts.is_empty() {
        return Err(format!("analysis {} declares no verdicts", info.id));
    }
    Ok(())
}

/// What an analysis was given to read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalysisInput {
    pub data: GraphData,
    /// False when more was there than was read.
    pub complete: bool,
    /// The graph it came from.
    pub graph: std::path::PathBuf,
}

impl Graph {
    /// Reads what `info` asks for, up to the cap. This holds the graph, and
    /// running the plugin afterwards does not.
    pub fn read_analysis_input(&mut self, info: &AnalysisInfo) -> Result<AnalysisInput, QueryError> {
        let params = BTreeMap::new();
        let mut collector = GraphCollector::default();
        let mut complete = true;
        let mut read = |collector: &mut GraphCollector, text: String| -> Result<bool, QueryError> {
            let rows = self.raw_cypher(&text, &params)?;
            let whole = rows.len() <= MAX_RECORDS;
            for row in rows.iter().take(MAX_RECORDS) {
                row.iter().for_each(|value| collector.visit(&value_to_json(value)));
            }
            Ok(whole)
        };
        let limit = MAX_RECORDS + 1;
        for label in &info.node_labels {
            complete &= read(&mut collector, format!("MATCH (n:{label}) RETURN n ORDER BY id(n) LIMIT {limit}"))?;
        }
        for name in &info.relationship_types {
            complete &= read(&mut collector, format!("MATCH (a)-[r:{name}]->(b) RETURN a, r, b ORDER BY id(r) LIMIT {limit}"))?;
        }
        Ok(AnalysisInput { data: collector.finish().unwrap_or_default(), complete, graph: self.path().to_path_buf() })
    }
}

/// A last sync known to have missed something. Unknown is not counted: a source
/// synced before runs were kept says nothing either way.
fn is_incomplete(coverage: &Field<SourceCoverage>) -> bool {
    coverage.known().is_some_and(|coverage| coverage.status != RunStatus::Complete)
}

/// How much of each source behind `input` its last sync covered. That includes a
/// source of this graph with nothing in it: its sync can have failed before
/// writing anything.
fn coverage_of_sources(input: &AnalysisInput, sources: &[Source]) -> BTreeMap<String, Field<SourceCoverage>> {
    let properties = input.data.nodes.iter().map(|node| &node.properties).chain(input.data.edges.iter().map(|edge| &edge.properties));
    let named: BTreeSet<&str> = properties.filter_map(|properties| properties.get(SOURCE_PROPERTY).and_then(Json::as_str)).collect();
    let here = crate::engine::normalize_path(&input.graph);
    let of_this_graph = sources.iter().filter(|source| crate::engine::normalize_path(&source.graph) == here).map(|source| source.id.as_str());
    named.into_iter().chain(of_this_graph).map(|source| (source.to_string(), evidence::coverage_of(source, sources))).collect()
}

fn source_of(properties: &serde_json::Map<String, Json>) -> Option<String> {
    properties.get(SOURCE_PROPERTY).and_then(Json::as_str).map(String::from)
}

/// The request for `input`. Record properties have bookkeeping left out and
/// secrets masked, as they are in evidence.
fn request_for(info: &AnalysisInfo, input: &AnalysisInput, sources: &[Source]) -> AnalyzeRequest {
    let refs: BTreeMap<i64, String> = input.data.nodes.iter().map(|node| (node.id, evidence::node_evidence_id(node))).collect();
    let nodes = input.data.nodes.iter().map(|node| AnalysisNode {
        reference: evidence::node_evidence_id(node),
        labels: node.labels.clone(),
        name: node_name(node),
        source: source_of(&node.properties),
        properties: evidence::own_properties(&node.properties).0,
    });
    let edges = input.data.edges.iter().filter_map(|edge| {
        Some(AnalysisEdge {
            reference: evidence::edge_evidence_id(edge),
            from: refs.get(&edge.source)?.clone(),
            to: refs.get(&edge.target)?.clone(),
            rel_type: edge.rel_type.clone(),
            source: source_of(&edge.properties),
            properties: evidence::own_properties(&edge.properties).0,
        })
    });
    AnalyzeRequest {
        protocol: PROTOCOL_VERSION,
        analysis: info.id.clone(),
        nodes: nodes.collect(),
        edges: edges.collect(),
        complete: input.complete,
        coverage: coverage_of_sources(input, sources)
            .into_iter()
            .filter_map(|(source, coverage)| {
                let coverage = coverage.known()?;
                Some((source, crate::protocol::SourceCoverage { run_id: coverage.run_id.clone(), status: coverage.status.word().to_string(), gaps: coverage.gaps.clone() }))
            })
            .collect(),
    }
}

/// Runs an analysis over what was read and checks the result. It holds no graph,
/// so a long analysis does not keep the window from using it.
pub fn run(offered: &Offered, input: &AnalysisInput, sources: &[Source], cancel: &AtomicBool, on_event: impl FnMut(SyncEvent)) -> Result<Report, PluginError> {
    let request = request_for(&offered.info, input, sources);
    let end = plugin::run_analyze(&offered.plugin, &request, cancel, on_event)?;
    report_from(offered, input, sources, end.findings, end.subjects).map_err(|message| PluginError::Failed {
        plugin: offered.plugin.id().to_string(),
        message: format!("its analysis was refused: {message}"),
    })
}

/// Reads the graph and runs an analysis, for a caller that may hold it throughout.
pub fn analyze(graph: &mut Graph, offered: &Offered, sources: &[Source], cancel: &AtomicBool) -> Result<Report, PluginError> {
    let input = graph.read_analysis_input(&offered.info)?;
    run(offered, &input, sources, cancel, |_| {})
}

/// Stable lookups over what the plugin was sent.
struct Records<'a> {
    data: &'a GraphData,
    nodes: BTreeMap<String, &'a GraphNode>,
    edges: BTreeMap<String, &'a GraphEdge>,
    by_id: BTreeMap<i64, &'a GraphNode>,
}

impl<'a> Records<'a> {
    fn new(data: &'a GraphData) -> Self {
        Self {
            data,
            nodes: data.nodes.iter().map(|node| (evidence::node_evidence_id(node), node)).collect(),
            edges: data.edges.iter().map(|edge| (evidence::edge_evidence_id(edge), edge)).collect(),
            by_id: data.nodes.iter().map(|node| (node.id, node)).collect(),
        }
    }

    fn has(&self, reference: &str) -> bool {
        self.nodes.contains_key(reference) || self.edges.contains_key(reference)
    }

    /// A field as the graph holds it, whatever the plugin thought it was.
    fn value(&self, reference: &str, field: &str) -> Field<Json> {
        if let Some(edge) = self.edges.get(reference) {
            let end = match field {
                "@from" => Some(edge.source),
                "@to" => Some(edge.target),
                _ => None,
            };
            if let Some(end) = end {
                return self.by_id.get(&end).map(|node| json!(evidence::node_evidence_id(node))).into();
            }
            return evidence::own_properties(&edge.properties).0.get(field).cloned().into();
        }
        self.nodes.get(reference).and_then(|node| evidence::own_properties(&node.properties).0.get(field).cloned()).into()
    }

    fn evidence(&self, reference: &str, sources: &[Source]) -> Option<Evidence> {
        if let Some(node) = self.nodes.get(reference) {
            return Some(evidence::for_node(node, sources));
        }
        let edge = self.edges.get(reference)?;
        Some(evidence::for_edge(edge, &self.data.nodes, sources))
    }

    /// References as hops over internal IDs. A hop must start where the last one
    /// ended, so a plugin cannot claim a connection the graph does not have.
    fn hops(&self, refs: &[HopRef]) -> Result<Vec<Hop>, String> {
        let mut hops: Vec<Hop> = Vec::with_capacity(refs.len());
        for (at, hop) in refs.iter().enumerate() {
            let edge = self.edges.get(&hop.edge).ok_or_else(|| format!("hop {} crosses {}, which it was not given", at + 1, hop.edge))?;
            let (from, to) = if hop.along { (edge.source, edge.target) } else { (edge.target, edge.source) };
            if hops.last().is_some_and(|last| last.to != from) {
                return Err(format!("hop {} crosses {}, which does not start where the hop before it ended", at + 1, hop.edge));
            }
            hops.push(Hop { depth: at as u32 + 1, from, to, edge: edge.id, rel_type: edge.rel_type.clone(), along: hop.along });
        }
        Ok(hops)
    }

    /// The records `hops` cross, for drawing them and nothing else.
    fn path_graph(&self, hops: &[Hop]) -> GraphData {
        let nodes: BTreeSet<i64> = hops.iter().flat_map(|hop| [hop.from, hop.to]).collect();
        let edges: BTreeSet<i64> = hops.iter().map(|hop| hop.edge).collect();
        GraphData {
            nodes: nodes.into_iter().filter_map(|id| self.by_id.get(&id).map(|node| (*node).clone())).collect(),
            edges: self.data.edges.iter().filter(|edge| edges.contains(&edge.id)).cloned().collect(),
        }
    }

    /// How to show `hops` in Path Explorer.
    fn walk_hint(&self, hops: &[Hop]) -> Option<WalkHint> {
        let name = |id: i64| {
            let node = self.by_id.get(&id)?;
            let reference = evidence::node_evidence_id(node);
            Some(reference.strip_prefix("node:").map_or_else(|| node.id.to_string(), str::to_string))
        };
        let types: BTreeSet<String> = hops.iter().map(|hop| hop.rel_type.clone()).collect();
        Some(WalkHint {
            from: name(hops.first()?.from)?,
            to: name(hops.last()?.to)?,
            direction: if hops.iter().all(|hop| hop.along) { Direction::Out } else { Direction::Both },
            types: types.into_iter().collect(),
            max_hops: hops.len() as u32,
        })
    }
}

/// Checks a plugin's findings against the records it was sent and looks up
/// everything they point at. One bad finding refuses the whole result: dropping
/// it would read as that resource having no finding.
fn report_from(offered: &Offered, input: &AnalysisInput, sources: &[Source], records: Vec<FindingRecord>, subjects: u64) -> Result<Report, String> {
    let info = &offered.info;
    let lookup = Records::new(&input.data);
    let rank: BTreeMap<&str, usize> = info.verdicts.iter().enumerate().map(|(at, verdict)| (verdict.id.as_str(), at)).collect();
    let ids: BTreeSet<&str> = records.iter().map(|record| record.finding_id.as_str()).collect();
    if ids.len() != records.len() {
        return Err("two findings have the same ID".into());
    }
    let mut findings = Vec::with_capacity(records.len());
    for record in &records {
        findings.push(check_finding(info, &lookup, sources, record, &rank, &ids).map_err(|message| format!("finding {}: {message}", record.finding_id))?);
    }
    findings.sort_by(|a, b| {
        let key = |finding: &'_ Finding| rank.get(finding.verdict.as_str()).copied();
        (key(a), &a.rule_id, &a.resource, &a.finding_id).cmp(&(key(b), &b.rule_id, &b.resource, &b.finding_id))
    });
    let summary = info
        .verdicts
        .iter()
        .map(|verdict| (verdict.id.clone(), findings.iter().filter(|finding| finding.verdict == verdict.id).count()))
        .collect();
    let names: Vec<&str> = info.verdicts.iter().map(|verdict| verdict.id.as_str()).collect();
    let coverage = coverage_of_sources(input, sources);
    let complete = input.complete && coverage.values().all(|coverage| !is_incomplete(coverage));
    Ok(Report {
        plugin: offered.plugin.id().to_string(),
        analysis: info.id.clone(),
        title: info.title.clone(),
        claim: info.claim.clone(),
        order: format!("By verdict ({}), then rule ID, then resource ID.", names.join(", ")),
        verdicts: info.verdicts.clone(),
        rules: info.rules.clone(),
        subjects,
        summary,
        complete,
        sources: coverage,
        findings,
    })
}

fn check_finding(info: &AnalysisInfo, lookup: &Records, sources: &[Source], record: &FindingRecord, rank: &BTreeMap<&str, usize>, ids: &BTreeSet<&str>) -> Result<Finding, String> {
    if !info.rules.iter().any(|rule| rule.rule_id == record.rule_id) {
        return Err(format!("rule {:?} is not one the analysis declares", record.rule_id));
    }
    if !rank.contains_key(record.verdict.as_str()) {
        return Err(format!("verdict {:?} is not one the analysis declares", record.verdict));
    }
    let Some(resource) = lookup.nodes.get(&record.resource) else {
        return Err(format!("resource {} is not a node it was given", record.resource));
    };
    if let Some(parent) = &record.depends_on {
        if !ids.contains(parent.as_str()) {
            return Err(format!("it depends on {parent}, which is not in the result"));
        }
    }

    let mut cited: BTreeSet<String> = record.records.iter().cloned().collect();
    let mut conditions = Vec::with_capacity(record.conditions.len());
    for condition in &record.conditions {
        let mut fields = Vec::with_capacity(condition.fields.len());
        for field in &condition.fields {
            cited.insert(field.record.clone());
            fields.push(SourceField {
                record: field.record.clone(),
                field: field.field.clone(),
                source_field: field.source_field.clone(),
                value: lookup.value(&field.record, &field.field),
            });
        }
        conditions.push(Condition {
            id: condition.id.clone(),
            statement: condition.statement.clone(),
            status: condition.status,
            detail: condition.detail.clone(),
            fields,
        });
    }
    cited.insert(record.resource.clone());
    if let Some(missing) = cited.iter().find(|reference| !lookup.has(reference)) {
        return Err(format!("it cites {missing}, which it was not given"));
    }

    let refs = record.path.as_ref().map(|path| path.hops.as_slice()).unwrap_or_default();
    let hops = lookup.hops(refs)?;
    if let (Some(first), Some(last)) = (hops.first(), hops.last()) {
        if first.from != resource.id && last.to != resource.id {
            return Err(format!("its path neither starts nor ends at {}", record.resource));
        }
    }
    let path: Option<PathEvidence> = record
        .path
        .as_ref()
        .filter(|_| !hops.is_empty())
        .map(|path| evidence::for_hops(&hops, lookup.data, sources, &path.derivation, Field::Known(record.rule_id.clone())));

    // In the path's order, then the rest by ID.
    let mut contributing: Vec<String> = path.as_ref().map(|path| path.supporting.clone()).unwrap_or_default();
    let rest: Vec<String> = cited.into_iter().filter(|id| !contributing.contains(id)).collect();
    contributing.extend(rest);
    let records: Vec<Evidence> = contributing.iter().filter_map(|id| lookup.evidence(id, sources)).collect();
    let behind: BTreeSet<&str> = records.iter().filter_map(|record| record.source.known().map(String::as_str)).collect();
    let incomplete_sources = behind.into_iter().filter(|source| is_incomplete(&evidence::coverage_of(source, sources))).map(String::from).collect();

    Ok(Finding {
        finding_id: record.finding_id.clone(),
        rule_id: record.rule_id.clone(),
        verdict: record.verdict.clone(),
        resource: record.resource.clone(),
        resource_name: node_name(resource),
        conditions,
        unknowns: record.unknowns.clone(),
        assumptions: record.assumptions.clone(),
        path,
        path_refs: (!refs.is_empty()).then(|| refs.to_vec()),
        walk: lookup.walk_hint(&hops),
        path_graph: lookup.path_graph(&hops),
        hops,
        depends_on: record.depends_on.clone(),
        incomplete_sources,
        contributing,
        evidence: records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ConditionRecord, FieldRef, Manifest, PathRef, VerdictTone};
    use serde_json::{Map, Value};
    use std::path::PathBuf;

    fn props(id: &str, extra: Value) -> Map<String, Value> {
        let mut map = Map::from_iter([("khonsu_id".to_string(), json!(id))]);
        map.extend(extra.as_object().unwrap().clone());
        map
    }

    /// `a -[e1]-> b -[e2]-> c`, in terms no provider uses.
    fn input() -> AnalysisInput {
        let node = |id: i64, name: &str| GraphNode { id, labels: vec!["Thing".into()], properties: props(name, json!({"name": name, "color": "red", "api_token": "abc123"})) };
        let edge = |id: i64, source: i64, target: i64, name: &str| GraphEdge { id, source, target, rel_type: "LINKS".into(), properties: props(name, json!({"weight": 3})) };
        AnalysisInput {
            data: GraphData { nodes: vec![node(1, "a"), node(2, "b"), node(3, "c")], edges: vec![edge(10, 1, 2, "e1"), edge(11, 2, 3, "e2")] },
            complete: true,
            graph: PathBuf::from("g"),
        }
    }

    fn offered() -> Offered {
        Offered {
            plugin: PluginInfo {
                manifest: Manifest { id: "demo".into(), name: "Demo".into(), version: "1".into(), protocol: 1, entry: vec!["demo".into()], description: String::new(), permissions: Default::default() },
                dir: PathBuf::new(),
                builtin: true,
            },
            info: AnalysisInfo {
                id: "links".into(),
                title: "Links".into(),
                claim: "Only what the records say.".into(),
                subject_labels: vec!["Thing".into()],
                verdicts: vec![
                    VerdictInfo { id: "flagged".into(), title: "Flagged".into(), tone: VerdictTone::Attention },
                    VerdictInfo { id: "clear".into(), title: "Clear".into(), tone: VerdictTone::Quiet },
                ],
                rules: vec![RuleInfo { rule_id: "demo.rule".into(), title: "Rule".into(), statement: "It holds.".into(), assumptions: vec![] }],
                ..AnalysisInfo::default()
            },
        }
    }

    fn finding(id: &str, verdict: &str, resource: &str) -> FindingRecord {
        FindingRecord {
            finding_id: id.into(),
            rule_id: "demo.rule".into(),
            verdict: verdict.into(),
            resource: resource.into(),
            conditions: vec![],
            unknowns: vec![],
            assumptions: vec![],
            path: None,
            depends_on: None,
            records: vec![],
        }
    }

    fn along(edges: &[&str]) -> PathRef {
        PathRef { derivation: "by rule".into(), hops: edges.iter().map(|edge| HopRef { edge: format!("rel:{edge}"), along: true }).collect() }
    }

    fn checked(records: Vec<FindingRecord>) -> Result<Report, String> {
        report_from(&offered(), &input(), &[], records, 3)
    }

    #[test]
    fn a_path_survives_the_wire_and_is_rebuilt_from_the_graph() {
        let record = FindingRecord { path: Some(along(&["e1", "e2"])), ..finding("f", "flagged", "node:c") };
        // As it crosses the process boundary.
        let line = serde_json::to_string(&record).unwrap();
        let report = checked(vec![serde_json::from_str(&line).unwrap()]).unwrap();
        let found = &report.findings[0];
        let walk = found.exploration().unwrap();
        assert_eq!(walk.starts, [1]);
        assert_eq!(walk.hops.iter().map(|hop| (hop.depth, hop.from, hop.to, hop.edge)).collect::<Vec<_>>(), [(1, 1, 2, 10), (2, 2, 3, 11)]);
        assert_eq!(walk.graph.nodes.len(), 3);
        assert_eq!(walk.graph.edges.len(), 2);
        assert!(walk.is_complete());
        let path = found.path.as_ref().unwrap();
        assert_eq!(path.hops, ["a -[LINKS]-> b", "b -[LINKS]-> c"]);
        assert_eq!(path.rule_id, Field::Known("demo.rule".to_string()));
        assert_eq!(found.contributing, ["node:a", "rel:e1", "node:b", "rel:e2", "node:c"]);
        let hint = found.walk.as_ref().unwrap();
        assert_eq!((hint.from.as_str(), hint.to.as_str(), hint.direction, hint.max_hops), ("a", "c", Direction::Out, 2));

        // And its references survive the report's own JSON.
        let text = serde_json::to_value(&report).unwrap();
        assert_eq!(text["findings"][0]["path_refs"][1]["edge"], "rel:e2");
    }

    #[test]
    fn a_hop_against_the_arrow_reads_from_its_far_end() {
        let path = PathRef { derivation: "by rule".into(), hops: vec![HopRef { edge: "rel:e1".into(), along: false }] };
        let report = checked(vec![FindingRecord { path: Some(path), ..finding("f", "flagged", "node:a") }]).unwrap();
        let hop = &report.findings[0].hops[0];
        assert_eq!((hop.from, hop.to, hop.along), (2, 1, false));
        assert_eq!(report.findings[0].walk.as_ref().unwrap().direction, Direction::Both);
    }

    #[test]
    fn a_value_is_read_from_the_graph_and_not_from_the_plugin() {
        let field = |record: &str, field: &str| FieldRef { record: record.into(), field: field.into(), source_field: "Source.Field".into() };
        let condition = ConditionRecord {
            id: "c".into(),
            statement: "It holds.".into(),
            status: ConditionStatus::Met,
            detail: String::new(),
            fields: vec![field("node:a", "color"), field("node:a", "absent"), field("node:a", "api_token"), field("rel:e1", "@to")],
        };
        let report = checked(vec![FindingRecord { conditions: vec![condition], ..finding("f", "flagged", "node:a") }]).unwrap();
        let fields = &report.findings[0].conditions[0].fields;
        assert_eq!(fields[0].value, Field::Known(json!("red")));
        assert_eq!(fields[0].source_field, "Source.Field");
        assert_eq!(fields[1].value, Field::Unknown, "a field that is not there is unknown");
        assert_eq!(fields[3].value, Field::Known(json!("node:b")));
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("abc123"), "a secret stays masked: {text}");
        assert_eq!(report.findings[0].contributing, ["node:a", "rel:e1"], "every record a condition read is cited");
    }

    #[test]
    fn a_result_that_does_not_match_the_graph_is_refused_whole() {
        let refused = |record: FindingRecord, expected: &str| {
            let good = finding("good", "clear", "node:a");
            let message = checked(vec![good, record]).unwrap_err();
            assert!(message.contains(expected), "{message}");
        };
        refused(finding("f", "flagged", "node:forged"), "not a node it was given");
        refused(finding("f", "flagged", "rel:e1"), "not a node it was given");
        refused(finding("f", "reachable", "node:a"), "not one the analysis declares");
        refused(FindingRecord { rule_id: "other".into(), ..finding("f", "flagged", "node:a") }, "not one the analysis declares");
        refused(FindingRecord { records: vec!["node:forged".into()], ..finding("f", "flagged", "node:a") }, "cites node:forged");
        refused(FindingRecord { path: Some(along(&["forged"])), ..finding("f", "flagged", "node:a") }, "not given");
        refused(FindingRecord { path: Some(along(&["e2", "e1"])), ..finding("f", "flagged", "node:c") }, "does not start where");
        refused(FindingRecord { path: Some(along(&["e1"])), ..finding("f", "flagged", "node:c") }, "neither starts nor ends");
        refused(FindingRecord { depends_on: Some("nothing".into()), ..finding("f", "flagged", "node:a") }, "depends on");
        refused(finding("good", "flagged", "node:b"), "same ID");
    }

    #[test]
    fn findings_come_in_the_declared_order_whatever_the_plugin_sent() {
        let report = checked(vec![finding("3", "clear", "node:a"), finding("2", "flagged", "node:c"), finding("1", "flagged", "node:b")]).unwrap();
        let order: Vec<&str> = report.findings.iter().map(|finding| finding.finding_id.as_str()).collect();
        assert_eq!(order, ["1", "2", "3"]);
        assert_eq!(report.summary, BTreeMap::from([("flagged".to_string(), 2), ("clear".to_string(), 1)]));
        assert_eq!(report.subjects, 3);
        assert!(report.order.contains("flagged, clear"), "{}", report.order);

        let (narrow, cut) = report.narrowed(|_| true, 1);
        assert_eq!((narrow.findings.len(), cut), (1, 2));
        assert_eq!(narrow.summary["flagged"], 2, "the summary still counts everything");
    }

    #[test]
    fn a_graph_too_large_to_read_says_so() {
        let mut large = input();
        large.complete = false;
        let report = report_from(&offered(), &large, &[], vec![], 0).unwrap();
        assert!(!report.complete);
        assert!(!request_for(&offered().info, &large, &[]).complete);
    }

    #[test]
    fn what_a_plugin_is_sent_has_no_bookkeeping_or_secrets() {
        let mut given = input();
        given.data.nodes[0].properties.insert("khonsu_hash".into(), json!("private"));
        let request = request_for(&offered().info, &given, &[]);
        assert_eq!(request.nodes[0].reference, "node:a");
        assert_eq!(request.nodes[0].name, "a");
        assert_eq!(request.edges[0].from, "node:a");
        assert_eq!(request.edges[0].to, "node:b");
        let text = serde_json::to_string(&request).unwrap();
        assert!(!text.contains("abc123"), "{text}");
        assert!(!text.contains("private"), "{text}");
    }

    fn source_with(status: RunStatus) -> Source {
        serde_json::from_value(json!({
            "id": "shop", "name": "shop", "plugin": "demo", "graph": "g",
            "runs": [{
                "run_id": "shop#1", "number": 1, "mode": "full", "started_at": 1, "ended_at": 2,
                "status": status, "plugin": "demo@0", "received_nodes": 0, "received_edges": 0,
                "removals_applied": false, "cursor_advanced": false, "gaps": ["page 3"],
            }],
        }))
        .unwrap()
    }

    fn tagged() -> AnalysisInput {
        let mut given = input();
        for node in &mut given.data.nodes {
            node.properties.insert(SOURCE_PROPERTY.into(), json!("shop"));
        }
        for edge in &mut given.data.edges {
            edge.properties.insert(SOURCE_PROPERTY.into(), json!("shop"));
        }
        given
    }

    /// A plugin that ignores coverage altogether: the result still shows it.
    #[test]
    fn an_incomplete_source_stays_visible_whatever_the_plugin_says() {
        for status in [RunStatus::Partial, RunStatus::Failed, RunStatus::Cancelled] {
            let sources = [source_with(status)];
            let report = report_from(&offered(), &tagged(), &sources, vec![finding("f", "clear", "node:a")], 1).unwrap();
            assert!(!report.complete, "{status:?}");
            assert_eq!(report.findings[0].incomplete_sources, ["shop"], "{status:?}");
            assert_eq!(report.findings[0].verdict, "clear", "the verdict is the plugin's to set");
            let sent = request_for(&offered().info, &tagged(), &sources);
            assert_eq!(sent.coverage["shop"].status, status.word());
            assert_eq!(sent.coverage["shop"].gaps, ["page 3"]);
            assert_eq!(sent.nodes[0].source.as_deref(), Some("shop"));
            assert!(!sent.coverage["shop"].is_complete());
        }
        let whole = [source_with(RunStatus::Complete)];
        let report = report_from(&offered(), &tagged(), &whole, vec![finding("f", "clear", "node:a")], 1).unwrap();
        assert!(report.complete);
        assert!(report.findings[0].incomplete_sources.is_empty());
    }

    #[test]
    fn a_source_with_no_record_of_its_runs_is_not_called_incomplete() {
        let report = report_from(&offered(), &tagged(), &[], vec![], 0).unwrap();
        assert!(report.complete);
        assert_eq!(report.sources["shop"], Field::Unknown);
        assert!(request_for(&offered().info, &tagged(), &[]).coverage.is_empty());
    }

    /// Its sync can have failed before writing anything, so it has no record to
    /// be found by, and the absence of its records must not read as clear.
    #[test]
    fn a_source_of_this_graph_with_nothing_in_it_still_counts() {
        let report = report_from(&offered(), &input(), &[source_with(RunStatus::Failed)], vec![], 0).unwrap();
        assert!(!report.complete);
        assert!(report.sources.contains_key("shop"));
    }

    #[test]
    fn a_plugin_cannot_put_query_text_in_what_it_reads() {
        let mut info = offered().info;
        info.relationship_types = vec!["LINKS]->() DETACH DELETE a //".into()];
        assert!(check_info(&info).unwrap_err().contains("not a label or type name"));
        info.relationship_types = vec![];
        info.node_labels = vec!["Thing) DETACH DELETE n //".into()];
        assert!(check_info(&info).is_err());
    }

    #[test]
    fn an_analysis_is_offered_by_label_alone() {
        let on = offered();
        assert!(on.applies_to(&["Thing".to_string()]));
        assert!(!on.applies_to(&["Other".to_string()]));
    }
}

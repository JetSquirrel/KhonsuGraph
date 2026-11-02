//! Explainable AWS exposure rules: which instances the configuration in a
//! graph leaves potentially open to the internet, and exactly why.
//!
//! A rule is a list of conditions, each met, not met or unknown, and each
//! naming the fields it read. The verdict follows from those alone: anything
//! not met gives [`Verdict::NotIndicated`], otherwise anything unknown gives
//! [`Verdict::InsufficientData`], otherwise [`Verdict::PotentialExposure`].
//! There is no score, and no verdict says "reachable": the graph holds
//! configuration, and nothing here sends traffic. See `docs/exposure-rules.md`.
//!
//! A record missing from the graph is not read as a fact about the account: a
//! saved response that was absent reads as empty (#38), so an instance with no
//! security groups or a subnet with no route table gives "unknown".

use crate::engine::{Graph, QueryError};
use crate::evidence::{self, Evidence, Field, PathEvidence};
use crate::explore::{node_name, Direction, Hop};
use crate::result::{value_to_json, GraphCollector, GraphData, GraphEdge, GraphNode};
use crate::settings::Source;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};

pub const RULE_DIRECT: &str = "aws.exposure.direct-instance";
pub const RULE_BALANCER: &str = "aws.exposure.public-load-balancer";
pub const RULE_LATERAL: &str = "aws.exposure.security-group-lateral";

/// Findings kept in a report unless asked otherwise; the rest are counted.
pub const DEFAULT_MAX_FINDINGS: usize = 200;
/// Relationships read of each type. A larger graph gives an incomplete report.
const MAX_RELATIONSHIPS: usize = 50_000;
const RELATIONSHIP_TYPES: &[&str] = &["ALLOWS", "PROTECTED_BY", "ROUTES_TO", "FORWARDS_TO", "IN_SUBNET", "IN_VPC", "USES_ROUTE_TABLE", "ROUTES_VIA"];

pub const CLAIM: &str = "Potential exposure worked out from configuration. No traffic was sent, so this does not show that anything is reachable, and a resource with no finding is not shown to be safe.";
pub const ORDER: &str = "By verdict (potential_exposure, insufficient_data, not_indicated), then rule ID, then resource ID. There is no risk score.";

const NOT_READ_NETWORK: &str = "Network ACLs, host firewalls and anything outside this AWS account are not read.";
const FAMILY: &str = "0.0.0.0/0 and ::/0 are one Internet record, so a rule's address family is not compared with the instance's address.";

/// A rule in words, for a person reading a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub rule_id: String,
    pub title: String,
    pub statement: String,
    pub assumptions: Vec<String>,
}

pub fn rules() -> Vec<Rule> {
    let rule = |id: &str, title: &str, statement: &str, assumptions: &[&str]| Rule {
        rule_id: id.into(),
        title: title.into(),
        statement: statement.into(),
        assumptions: assumptions.iter().map(|text| (*text).into()).collect(),
    };
    vec![
        rule(
            RULE_DIRECT,
            "Instance open to the internet directly",
            "A running instance with a public address, a security group rule open to the internet, and a route to an internet gateway from its subnet.",
            &[NOT_READ_NETWORK, FAMILY, "Whether anything listens on the allowed ports is not read."],
        ),
        rule(
            RULE_BALANCER,
            "Instance behind an internet-facing load balancer",
            "A running, healthy target of an internet-facing load balancer whose security group is open to the internet, where the target's security group allows the target port from the balancer.",
            &[
                NOT_READ_NETWORK,
                "Listeners and their rules are not read, so an open security group is not shown to lead to this target group.",
                "The target group's port is used; a target registered on another port is not seen.",
                "A CIDR rule is not compared with the balancer's addresses, which are not read.",
            ],
        ),
        rule(
            RULE_LATERAL,
            "Instance one security group rule behind a potentially exposed one",
            "A running instance whose security group allows traffic from the security group of an instance with a potential exposure finding.",
            &[
                NOT_READ_NETWORK,
                "Only one step is followed; use khonsu explore for longer chains.",
                "Whether the source instance could be made to send this traffic is not assessed.",
            ],
        ),
    ]
}

/// There is deliberately no "reachable" here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every condition is met by configuration.
    PotentialExposure,
    /// None ruled it out, and at least one could not be evaluated.
    InsufficientData,
    /// A condition is not met, so this rule does not point at it.
    NotIndicated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Met,
    NotMet,
    Unknown,
}

/// An exact field a condition read, and what AWS called it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceField {
    /// The evidence ID of the record that holds it.
    pub record: String,
    pub field: String,
    pub aws_field: String,
    pub value: Field<Json>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub id: String,
    pub statement: String,
    pub status: Status,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The same rule and records give the same ID.
    pub finding_id: String,
    pub rule_id: String,
    pub verdict: Verdict,
    /// The evidence ID of the resource it is about, and what people call it.
    pub resource: String,
    pub resource_name: String,
    pub conditions: Vec<Condition>,
    /// The statements of conditions that could not be evaluated.
    pub unknowns: Vec<String>,
    pub assumptions: Vec<String>,
    /// The candidate path, when the records for one exist. Its `rule_id` is known.
    pub path: Option<PathEvidence>,
    /// The same path as hops, for a later rule to continue from.
    #[serde(skip)]
    pub hops: Vec<Hop>,
    pub walk: Option<WalkHint>,
    /// The finding this one rests on, for a lateral step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// The evidence IDs of every record the conditions read or the path crosses.
    pub contributing: Vec<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub potential_exposure: usize,
    pub insufficient_data: usize,
    pub not_indicated: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub claim: String,
    pub order: String,
    pub rules: Vec<Rule>,
    /// Instances looked at.
    pub instances: usize,
    /// Counts every finding, including any cut by a cap.
    pub summary: Summary,
    /// False when the graph was too large to read whole: a missing finding
    /// then means nothing.
    pub complete: bool,
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

impl Graph {
    /// Reads what the rules need and evaluates them.
    pub fn exposure(&mut self, sources: &[Source]) -> Result<Report, QueryError> {
        let (data, complete) = self.read_exposure_records()?;
        Ok(evaluate(&data, sources, complete))
    }

    fn read_exposure_records(&mut self) -> Result<(GraphData, bool), QueryError> {
        let params = BTreeMap::new();
        let mut collector = GraphCollector::default();
        let mut complete = true;
        let mut read = |collector: &mut GraphCollector, text: String, cap: usize| -> Result<bool, QueryError> {
            let rows = self.raw_cypher(&text, &params)?;
            let whole = rows.len() <= cap;
            for row in rows.iter().take(cap) {
                row.iter().for_each(|value| collector.visit(&value_to_json(value)));
            }
            Ok(whole)
        };
        // An instance with no relationships still counts as looked at.
        complete &= read(&mut collector, format!("MATCH (n:Instance) RETURN n ORDER BY id(n) LIMIT {}", MAX_RELATIONSHIPS + 1), MAX_RELATIONSHIPS)?;
        for name in RELATIONSHIP_TYPES {
            let text = format!("MATCH (a)-[r:{name}]->(b) RETURN a, r, b ORDER BY id(r) LIMIT {}", MAX_RELATIONSHIPS + 1);
            complete &= read(&mut collector, text, MAX_RELATIONSHIPS)?;
        }
        Ok((collector.finish().unwrap_or_default(), complete))
    }
}

/// Evaluates every rule over `data`. The result depends on nothing else.
pub fn evaluate(data: &GraphData, sources: &[Source], complete: bool) -> Report {
    let index = Index::new(data);
    let mut findings = Vec::new();
    for instance in index.with_label("Instance") {
        findings.push(direct(&index, instance, sources));
        for (balancer, group, to_target, forwards) in index.balancers_of(instance) {
            findings.push(behind_balancer(&index, instance, balancer, group, to_target, forwards, sources));
        }
    }
    let lateral = lateral_findings(&index, &findings, sources);
    findings.extend(lateral);
    findings.sort_by(|a, b| (a.verdict, &a.rule_id, &a.resource, &a.finding_id).cmp(&(b.verdict, &b.rule_id, &b.resource, &b.finding_id)));

    let count = |verdict: Verdict| findings.iter().filter(|finding| finding.verdict == verdict).count();
    Report {
        claim: CLAIM.into(),
        order: ORDER.into(),
        rules: rules(),
        instances: index.with_label("Instance").len(),
        summary: Summary {
            potential_exposure: count(Verdict::PotentialExposure),
            insufficient_data: count(Verdict::InsufficientData),
            not_indicated: count(Verdict::NotIndicated),
        },
        complete,
        findings,
    }
}

/// Stable lookups over the records, sorted so that every choice is repeatable.
struct Index<'a> {
    nodes: BTreeMap<i64, &'a GraphNode>,
    data: &'a GraphData,
    node_ids: BTreeMap<String, &'a GraphNode>,
    edge_ids: BTreeMap<String, &'a GraphEdge>,
    /// Relationships by (node, type), in the order of the far end's stable ID.
    out: BTreeMap<(i64, &'a str), Vec<&'a GraphEdge>>,
    into: BTreeMap<(i64, &'a str), Vec<&'a GraphEdge>>,
}

impl<'a> Index<'a> {
    fn new(data: &'a GraphData) -> Self {
        let nodes: BTreeMap<i64, &GraphNode> = data.nodes.iter().map(|node| (node.id, node)).collect();
        let mut out: BTreeMap<(i64, &str), Vec<&GraphEdge>> = BTreeMap::new();
        let mut into: BTreeMap<(i64, &str), Vec<&GraphEdge>> = BTreeMap::new();
        for edge in &data.edges {
            out.entry((edge.source, &edge.rel_type)).or_default().push(edge);
            into.entry((edge.target, &edge.rel_type)).or_default().push(edge);
        }
        out.values_mut().chain(into.values_mut()).for_each(|edges| edges.sort_by_cached_key(|edge| edge_id(edge)));
        Self {
            nodes,
            data,
            node_ids: data.nodes.iter().map(|node| (record_id(node), node)).collect(),
            edge_ids: data.edges.iter().map(|edge| (edge_id(edge), edge)).collect(),
            out,
            into,
        }
    }

    fn with_label(&self, label: &str) -> Vec<&'a GraphNode> {
        let mut found: Vec<&GraphNode> = self.nodes.values().copied().filter(|node| node.labels.iter().any(|name| name == label)).collect();
        found.sort_by_cached_key(|node| record_id(node));
        found
    }

    fn out_of(&self, node: i64, rel_type: &'static str) -> &[&'a GraphEdge] {
        self.out.get(&(node, rel_type)).map_or(&[], Vec::as_slice)
    }

    fn arriving_at(&self, node: i64, rel_type: &'static str) -> &[&'a GraphEdge] {
        self.into.get(&(node, rel_type)).map_or(&[], Vec::as_slice)
    }

    fn node(&self, id: i64) -> Option<&'a GraphNode> {
        self.nodes.get(&id).copied()
    }

    /// `ALLOWS` rules into `groups` from the internet.
    fn internet_rules(&self, groups: &[&GraphEdge]) -> Vec<&'a GraphEdge> {
        groups
            .iter()
            .flat_map(|protected| self.arriving_at(protected.target, "ALLOWS"))
            .filter(|rule| self.node(rule.source).is_some_and(is_internet))
            .copied()
            .collect()
    }

    /// Every (balancer, target group, group → instance, balancer → group) that
    /// sends traffic to `instance`.
    fn balancers_of(&self, instance: &GraphNode) -> Vec<(&'a GraphNode, &'a GraphNode, &'a GraphEdge, &'a GraphEdge)> {
        let mut found = Vec::new();
        for to_target in self.arriving_at(instance.id, "ROUTES_TO") {
            let Some(group) = self.node(to_target.source).filter(|node| has_label(node, "TargetGroup")) else { continue };
            for forwards in self.arriving_at(group.id, "FORWARDS_TO") {
                if let Some(balancer) = self.node(forwards.source) {
                    found.push((balancer, group, *to_target, *forwards));
                }
            }
        }
        found
    }

    /// The table a subnet uses: its own association, else its VPC's main one.
    fn route_table_of(&self, subnet: &GraphNode) -> Option<&'a GraphNode> {
        if let Some(explicit) = self.out_of(subnet.id, "USES_ROUTE_TABLE").first() {
            return self.node(explicit.target);
        }
        let vpc = self.out_of(subnet.id, "IN_VPC").first()?.target;
        self.arriving_at(vpc, "IN_VPC")
            .iter()
            .filter_map(|edge| self.node(edge.source))
            .find(|node| has_label(node, "RouteTable") && node.properties.get("is_main") == Some(&json!(true)))
    }
}

fn has_label(node: &GraphNode, label: &str) -> bool {
    node.labels.iter().any(|name| name == label)
}

fn is_internet(node: &GraphNode) -> bool {
    has_label(node, "Cidr") && matches!(node.properties.get("cidr").and_then(Json::as_str), Some("0.0.0.0/0" | "::/0"))
}

fn record_id(node: &GraphNode) -> String {
    evidence::node_evidence_id(node)
}

fn edge_id(edge: &GraphEdge) -> String {
    evidence::edge_evidence_id(edge)
}

fn node_field(node: &GraphNode, field: &str, aws_field: &str) -> SourceField {
    SourceField {
        record: record_id(node),
        field: field.into(),
        aws_field: aws_field.into(),
        value: node.properties.get(field).cloned().into(),
    }
}

fn edge_field(edge: &GraphEdge, field: &str, aws_field: &str) -> SourceField {
    SourceField {
        record: edge_id(edge),
        field: field.into(),
        aws_field: aws_field.into(),
        value: edge.properties.get(field).cloned().into(),
    }
}

/// A rule's ports in words, as the plugin wrote them.
fn ports_of(rule: &GraphEdge) -> String {
    let text = |key: &str| rule.properties.get(key).and_then(Json::as_str).unwrap_or("unknown").to_string();
    format!("{} {}", text("protocol"), text("ports"))
}

/// Whether a security group rule covers `port` on `protocol`. The plugin writes
/// ports as `all`, `443` or `8000-8100`.
fn rule_covers(rule: &GraphEdge, protocol: &str, port: i64) -> bool {
    let text = |key: &str| rule.properties.get(key).and_then(Json::as_str).unwrap_or_default();
    if text("protocol") != "all" && text("protocol") != protocol {
        return false;
    }
    let ports = text("ports");
    if ports == "all" {
        return true;
    }
    match ports.split_once('-') {
        Some((low, high)) => matches!((low.parse::<i64>(), high.parse::<i64>()), (Ok(low), Ok(high)) if (low..=high).contains(&port)),
        None => ports.parse() == Ok(port),
    }
}

fn condition(id: &str, statement: &str, status: Status, detail: impl Into<String>, fields: Vec<SourceField>) -> Condition {
    Condition {
        id: id.into(),
        statement: statement.into(),
        status,
        detail: detail.into(),
        fields,
    }
}

fn running(instance: &GraphNode) -> Condition {
    let (status, detail) = match instance.properties.get("state").and_then(Json::as_str) {
        Some("running") => (Status::Met, "The instance is running.".to_string()),
        Some(state) => (Status::NotMet, format!("The instance is {state}.")),
        None => (Status::Unknown, "The instance's state was not supplied.".to_string()),
    };
    condition("instance-running", "The instance is running.", status, detail, vec![node_field(instance, "state", "ec2 describe-instances: State.Name")])
}

/// Whether the groups in `protected` have an open rule, with no groups at all
/// being unknown: every instance has one, so none means they were not read.
fn open_rule(index: &Index, id: &str, whose: &str, protected: &[&GraphEdge], aws_groups: &str) -> Condition {
    let statement = format!("{whose} has a security group rule open to the internet.");
    let rules = index.internet_rules(protected);
    let group_fields: Vec<SourceField> = protected
        .iter()
        .map(|edge| SourceField {
            record: edge_id(edge),
            field: "PROTECTED_BY".into(),
            aws_field: aws_groups.into(),
            value: Field::Known(json!(index.node(edge.target).map(record_id))),
        })
        .collect();
    if protected.is_empty() {
        let detail = "No security groups are recorded for it, so its rules could not be read.";
        return condition(id, &statement, Status::Unknown, detail, vec![]);
    }
    let mut fields = group_fields;
    for rule in &rules {
        fields.push(edge_field(rule, "protocol", "ec2 describe-security-groups: IpPermissions[].IpProtocol"));
        fields.push(edge_field(rule, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
    }
    if rules.is_empty() {
        return condition(id, &statement, Status::NotMet, "None of its security groups has a rule from 0.0.0.0/0 or ::/0.", fields);
    }
    let ports: BTreeSet<String> = rules.iter().map(|rule| ports_of(rule)).collect();
    let detail = format!("Open to the internet on {}.", ports.into_iter().collect::<Vec<_>>().join(", "));
    condition(id, &statement, Status::Met, detail, fields)
}

fn verdict_of(conditions: &[Condition]) -> Verdict {
    if conditions.iter().any(|condition| condition.status == Status::NotMet) {
        Verdict::NotIndicated
    } else if conditions.iter().any(|condition| condition.status == Status::Unknown) {
        Verdict::InsufficientData
    } else {
        Verdict::PotentialExposure
    }
}

fn public_address(instance: &GraphNode) -> Condition {
    let fields = vec![
        node_field(instance, "public_ip", "ec2 describe-instances: PublicIpAddress"),
        node_field(instance, "ipv6_address", "ec2 describe-instances: Ipv6Address"),
    ];
    let addresses: Vec<&str> = ["public_ip", "ipv6_address"].iter().filter_map(|key| instance.properties.get(*key).and_then(Json::as_str)).collect();
    let (status, detail) = if addresses.is_empty() {
        (Status::NotMet, "AWS reported no public IPv4 or IPv6 address.".to_string())
    } else {
        (Status::Met, format!("Public address {}.", addresses.join(", ")))
    };
    condition("public-address", "The instance has a public address.", status, detail, fields)
}

fn internet_route(index: &Index, instance: &GraphNode) -> (Condition, Vec<String>) {
    let id = "internet-route";
    let statement = "The instance's subnet has an active default route to an internet gateway.";
    let Some(subnet) = index.out_of(instance.id, "IN_SUBNET").first().and_then(|edge| index.node(edge.target)) else {
        return (condition(id, statement, Status::Unknown, "No subnet is recorded for the instance.", vec![]), vec![]);
    };
    let subnet_id = record_id(subnet);
    let Some(table) = index.route_table_of(subnet) else {
        let detail = "No route table is recorded for the subnet or as its VPC's main table, so its routes could not be read.";
        return (condition(id, statement, Status::Unknown, detail, vec![]), vec![subnet_id]);
    };
    let wanted: Vec<&str> = match (instance.properties.contains_key("public_ip"), instance.properties.contains_key("ipv6_address")) {
        (true, false) => vec!["0.0.0.0/0"],
        (false, true) => vec!["::/0"],
        _ => vec!["0.0.0.0/0", "::/0"],
    };
    let routes = index.out_of(table.id, "ROUTES_VIA");
    let text = |edge: &GraphEdge, key: &str| edge.properties.get(key).and_then(Json::as_str).unwrap_or_default().to_string();
    let is_v6 = |destination: &str| destination.contains(':');
    // Only routes of the address's own family can carry its traffic.
    let active: Vec<&&GraphEdge> = routes
        .iter()
        .filter(|route| text(route, "state") == "active" && wanted.iter().any(|family| is_v6(family) == is_v6(&text(route, "destination"))))
        .collect();
    let mut fields = vec![
        node_field(table, "ipv4_default_target", "ec2 describe-route-tables: Routes[DestinationCidrBlock=0.0.0.0/0]"),
        node_field(table, "ipv6_default_target", "ec2 describe-route-tables: Routes[DestinationIpv6CidrBlock=::/0]"),
    ];
    for route in routes {
        fields.push(edge_field(route, "destination", "ec2 describe-route-tables: Routes[].DestinationCidrBlock"));
        fields.push(edge_field(route, "state", "ec2 describe-route-tables: Routes[].State"));
    }
    let name = node_name(table);
    let (status, detail) = if let Some(route) = active.iter().find(|route| wanted.contains(&text(route, "destination").as_str())) {
        (Status::Met, format!("Route table {name} sends {} to an internet gateway.", text(route, "destination")))
    } else if let Some(route) = active.first() {
        (Status::Unknown, format!("Route table {name} sends only {} to an internet gateway, so whether it covers a given client is not known.", text(route, "destination")))
    } else {
        (Status::NotMet, format!("Route table {name} has no active default route to an internet gateway."))
    };
    let mut records = vec![subnet_id, record_id(table)];
    records.extend(routes.iter().map(|route| edge_id(route)));
    (condition(id, statement, status, detail, fields), records)
}

fn direct(index: &Index, instance: &GraphNode, sources: &[Source]) -> Finding {
    let protected = index.out_of(instance.id, "PROTECTED_BY");
    let open = open_rule(index, "open-inbound-rule", "The instance", protected, "ec2 describe-instances: SecurityGroups[].GroupId");
    let (route, route_records) = internet_route(index, instance);
    let conditions = vec![running(instance), public_address(instance), open, route];

    // The first open rule, with the group it opens, is the candidate path.
    let hops = protected.iter().find_map(|edge| {
        let rule = index.arriving_at(edge.target, "ALLOWS").iter().find(|rule| index.node(rule.source).is_some_and(is_internet))?;
        Some(vec![along(rule, 1), against(edge, 2)])
    });
    let mut records = route_records;
    records.push(record_id(instance));
    records.extend(protected.iter().map(|edge| edge_id(edge)));
    for rule in index.internet_rules(protected) {
        records.push(edge_id(rule));
    }
    build(index, instance, RULE_DIRECT, conditions, hops.unwrap_or_default(), records, None, sources)
}

fn behind_balancer(index: &Index, instance: &GraphNode, balancer: &GraphNode, group: &GraphNode, to_target: &GraphEdge, forwards: &GraphEdge, sources: &[Source]) -> Finding {
    let balancer_groups = index.out_of(balancer.id, "PROTECTED_BY");
    let instance_groups = index.out_of(instance.id, "PROTECTED_BY");

    let (status, detail) = match balancer.properties.get("scheme").and_then(Json::as_str) {
        Some("internet-facing") => (Status::Met, "The load balancer is internet-facing.".to_string()),
        Some(scheme) => (Status::NotMet, format!("The load balancer's scheme is {scheme}.")),
        None => (Status::Unknown, "The load balancer's scheme was not supplied.".to_string()),
    };
    let facing = condition("internet-facing", "The load balancer is internet-facing.", status, detail, vec![node_field(balancer, "scheme", "elbv2 describe-load-balancers: Scheme")]);
    let open = open_rule(index, "balancer-open-rule", "The load balancer", balancer_groups, "elbv2 describe-load-balancers: SecurityGroups[]");
    let health = {
        let field = edge_field(to_target, "health", "elbv2 describe-target-health: TargetHealth.State");
        let (status, detail) = match to_target.properties.get("health").and_then(Json::as_str) {
            Some("healthy") => (Status::Met, "The target was healthy.".to_string()),
            Some(state) => (Status::Unknown, format!("The target's health was {state}, which can change without the configuration changing.")),
            None => (Status::Unknown, "The target's health was not supplied.".to_string()),
        };
        condition("target-healthy", "The target is healthy.", status, detail, vec![field])
    };
    let (allows, allow_records) = target_allows_balancer(index, group, balancer_groups, instance_groups);
    let conditions = vec![running(instance), facing, open, health, allows];

    let opening = index.arriving_at(balancer.id, "ROUTES_TO").iter().find(|edge| index.node(edge.source).is_some_and(is_internet));
    let hops = opening.map_or_else(Vec::new, |edge| vec![along(edge, 1), along(forwards, 2), along(to_target, 3)]);
    let mut records = allow_records;
    records.extend([record_id(instance), record_id(balancer), record_id(group), edge_id(to_target), edge_id(forwards)]);
    records.extend(balancer_groups.iter().map(|edge| edge_id(edge)));
    for rule in index.internet_rules(balancer_groups) {
        records.push(edge_id(rule));
    }
    build(index, instance, RULE_BALANCER, conditions, hops, records, Some(format!("{}\n{}", record_id(balancer), record_id(group))), sources)
}

/// Whether the target's groups let the balancer in on the target group's port.
fn target_allows_balancer(index: &Index, group: &GraphNode, balancer_groups: &[&GraphEdge], instance_groups: &[&GraphEdge]) -> (Condition, Vec<String>) {
    let id = "target-allows-balancer";
    let statement = "The target's security group allows the target group's port from the load balancer.";
    let port_field = node_field(group, "port", "elbv2 describe-target-groups: Port");
    let Some(port) = group.properties.get("port").and_then(Json::as_i64) else {
        return (condition(id, statement, Status::Unknown, "The target group's port was not supplied.", vec![port_field]), vec![]);
    };
    if instance_groups.is_empty() {
        let detail = "No security groups are recorded for the target, so its rules could not be read.";
        return (condition(id, statement, Status::Unknown, detail, vec![port_field]), vec![]);
    }
    let protocol = match group.properties.get("protocol").and_then(Json::as_str) {
        Some("UDP") => "udp",
        _ => "tcp",
    };
    let from_balancer: BTreeSet<i64> = balancer_groups.iter().map(|edge| edge.target).collect();
    let rules: Vec<&GraphEdge> = instance_groups
        .iter()
        .flat_map(|edge| index.arriving_at(edge.target, "ALLOWS"))
        .filter(|rule| rule_covers(rule, protocol, port))
        .copied()
        .collect();
    let matching = rules.iter().find(|rule| from_balancer.contains(&rule.source) || index.node(rule.source).is_some_and(is_internet));
    let other_cidr = rules.iter().find(|rule| index.node(rule.source).is_some_and(|node| has_label(node, "Cidr")));
    let chosen = matching.or(other_cidr);
    let mut fields = vec![port_field];
    let mut records = Vec::new();
    if let Some(rule) = chosen {
        fields.push(edge_field(rule, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
        records.push(edge_id(rule));
    }
    let (status, detail) = if matching.is_some() {
        (Status::Met, format!("A rule allows {protocol} {port} from the load balancer's group or the internet."))
    } else if other_cidr.is_some() {
        (Status::Unknown, format!("A CIDR rule allows {protocol} {port}, and the load balancer's addresses are not read to compare with it."))
    } else {
        (Status::NotMet, format!("No rule on the target's groups allows {protocol} {port} from the load balancer."))
    };
    (condition(id, statement, status, detail, fields), records)
}

/// One finding for each instance one group rule behind an instance that a rule
/// left potentially exposed or undecided. Findings about instances the rules
/// do not point at would only be noise.
fn lateral_findings(index: &Index, findings: &[Finding], sources: &[Source]) -> Vec<Finding> {
    // The strongest finding on each instance, first by verdict then by rule.
    let mut best: BTreeMap<&str, &Finding> = BTreeMap::new();
    for finding in findings.iter().filter(|finding| finding.verdict != Verdict::NotIndicated) {
        best.entry(finding.resource.as_str())
            .and_modify(|held| {
                if (finding.verdict, &finding.rule_id) < (held.verdict, &held.rule_id) {
                    *held = finding;
                }
            })
            .or_insert(finding);
    }
    let mut out = Vec::new();
    for source in index.with_label("Instance") {
        let Some(found) = best.get(record_id(source).as_str()) else { continue };
        let mut pairs: BTreeMap<i64, (&GraphEdge, &GraphEdge, Vec<&GraphEdge>)> = BTreeMap::new();
        for &own in index.out_of(source.id, "PROTECTED_BY") {
            for &rule in index.out_of(own.target, "ALLOWS") {
                for &protected in index.arriving_at(rule.target, "PROTECTED_BY") {
                    if protected.source != source.id && index.node(protected.source).is_some_and(|node| has_label(node, "Instance")) {
                        pairs.entry(protected.source).or_insert((own, protected, Vec::new())).2.push(rule);
                    }
                }
            }
        }
        for (target, (own, protected, rules)) in pairs {
            let Some(instance) = index.node(target) else { continue };
            out.push(lateral(index, instance, source, found, own, protected, &rules, sources));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn lateral(index: &Index, instance: &GraphNode, source: &GraphNode, found: &Finding, own: &GraphEdge, protected: &GraphEdge, rules: &[&GraphEdge], sources: &[Source]) -> Finding {
    let name = node_name(source);
    let (status, detail) = match found.verdict {
        Verdict::PotentialExposure => (Status::Met, format!("{name} has a potential exposure finding from {}.", found.rule_id)),
        _ => (Status::Unknown, format!("{name} has insufficient data under {}.", found.rule_id)),
    };
    let exposed = condition("source-potentially-exposed", "The source instance has a potential exposure finding.", status, detail, vec![]);
    let ports: BTreeSet<String> = rules.iter().map(|rule| ports_of(rule)).collect();
    let mut rule_fields = Vec::new();
    for rule in rules {
        rule_fields.push(edge_field(rule, "protocol", "ec2 describe-security-groups: IpPermissions[].IpProtocol"));
        rule_fields.push(edge_field(rule, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
    }
    let allows = condition(
        "group-allows-source",
        "A security group rule on this instance allows the source instance's group.",
        Status::Met,
        format!("Allowed on {}.", ports.into_iter().collect::<Vec<_>>().join(", ")),
        rule_fields,
    );
    let conditions = vec![running(instance), exposed, allows];

    // The source's own path, then across the rule to this instance.
    let rule = rules[0];
    let mut hops = found.hops.clone();
    if !hops.is_empty() {
        let at = hops.len() as u32;
        hops.extend([along(own, at + 1), along(rule, at + 2), against(protected, at + 3)]);
    }
    let mut records = vec![record_id(instance), record_id(source), edge_id(own), edge_id(protected)];
    records.extend(rules.iter().map(|rule| edge_id(rule)));
    let mut finding = build(index, instance, RULE_LATERAL, conditions, hops, records, Some(record_id(source)), sources);
    finding.depends_on = Some(found.finding_id.clone());
    finding
}

fn along(edge: &GraphEdge, depth: u32) -> Hop {
    Hop {
        depth,
        from: edge.source,
        to: edge.target,
        edge: edge.id,
        rel_type: edge.rel_type.clone(),
        along: true,
    }
}

fn against(edge: &GraphEdge, depth: u32) -> Hop {
    Hop {
        depth,
        from: edge.target,
        to: edge.source,
        edge: edge.id,
        rel_type: edge.rel_type.clone(),
        along: false,
    }
}

/// How to show `hops` in Path Explorer.
fn walk_hint(index: &Index, hops: &[Hop], instance: &GraphNode) -> Option<WalkHint> {
    let start = index.node(hops.first()?.from)?;
    let name = |node: &GraphNode| {
        let id = record_id(node);
        id.strip_prefix("node:").map_or_else(|| node.id.to_string(), str::to_string)
    };
    let types: BTreeSet<String> = hops.iter().map(|hop| hop.rel_type.clone()).collect();
    Some(WalkHint {
        from: name(start),
        to: name(instance),
        direction: if hops.iter().all(|hop| hop.along) { Direction::Out } else { Direction::Both },
        types: types.into_iter().collect(),
        max_hops: hops.len() as u32,
    })
}

#[allow(clippy::too_many_arguments)]
fn build(index: &Index, instance: &GraphNode, rule_id: &str, conditions: Vec<Condition>, hops: Vec<Hop>, records: Vec<String>, subject: Option<String>, sources: &[Source]) -> Finding {
    let resource = record_id(instance);
    let verdict = verdict_of(&conditions);
    let unknowns = conditions.iter().filter(|condition| condition.status == Status::Unknown).map(|condition| condition.statement.clone()).collect();
    let path = (!hops.is_empty()).then(|| {
        evidence::for_hops(&hops, index.data, sources, "the rule's candidate path", Field::Known(rule_id.to_string()))
    });
    let walk = walk_hint(index, &hops, instance);

    // In the path's order, then the rest by ID.
    let mut contributing: Vec<String> = path.as_ref().map(|path| path.supporting.clone()).unwrap_or_default();
    let rest: BTreeSet<String> = records.into_iter().filter(|id| !contributing.contains(id)).collect();
    contributing.extend(rest);
    let evidence = contributing.iter().filter_map(|id| record_for(index, id, sources)).collect();

    let id_text = [Some(rule_id.to_string()), Some(resource.clone()), subject].into_iter().flatten().collect::<Vec<_>>().join("\n");
    let assumptions = rules().into_iter().find(|rule| rule.rule_id == rule_id).map(|rule| rule.assumptions).unwrap_or_default();
    Finding {
        finding_id: format!("finding:{:016x}", evidence::fnv1a(&id_text)),
        rule_id: rule_id.into(),
        verdict,
        resource,
        resource_name: node_name(instance),
        conditions,
        unknowns,
        assumptions,
        path,
        hops,
        walk,
        depends_on: None,
        contributing,
        evidence,
    }
}

fn record_for(index: &Index, id: &str, sources: &[Source]) -> Option<Evidence> {
    if let Some(node) = index.node_ids.get(id) {
        return Some(evidence::for_node(node, sources));
    }
    let edge = index.edge_ids.get(id)?;
    Some(evidence::for_edge(edge, &index.data.nodes, sources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value};

    fn props(id: &str, extra: Value) -> Map<String, Value> {
        let mut map = Map::from_iter([("khonsu_id".to_string(), json!(id))]);
        map.extend(extra.as_object().unwrap().clone());
        map
    }

    /// Builds a graph by stable name, with IDs in order of first mention.
    #[derive(Default)]
    struct Builder {
        data: GraphData,
    }

    impl Builder {
        fn node(mut self, id: &str, label: &str, extra: Value) -> Self {
            let next = self.data.nodes.len() as i64 + 1;
            self.data.nodes.push(GraphNode {
                id: next,
                labels: vec![label.into()],
                properties: props(id, extra),
            });
            self
        }

        fn edge(mut self, source: &str, rel_type: &str, target: &str, extra: Value) -> Self {
            let find = |name: &str| self.data.nodes.iter().find(|node| node.properties["khonsu_id"] == json!(name)).unwrap_or_else(|| panic!("no node {name}")).id;
            let (source_id, target_id) = (find(source), find(target));
            let next = self.data.edges.len() as i64 + 100;
            let id = format!("{source}|{rel_type}|{target}|{extra}");
            self.data.edges.push(GraphEdge {
                id: next,
                source: source_id,
                target: target_id,
                rel_type: rel_type.into(),
                properties: props(&id, extra),
            });
            self
        }

        fn report(&self) -> Report {
            evaluate(&self.data, &[], true)
        }
    }

    /// An internet-open group on a running public instance with a route: every
    /// direct condition is met. Tests take this apart one fact at a time.
    fn exposed() -> Builder {
        Builder::default()
            .node("internet", "Cidr", json!({"name": "Internet", "cidr": "0.0.0.0/0"}))
            .node("sg", "SecurityGroup", json!({"name": "sg"}))
            .node("vpc", "Vpc", json!({"name": "vpc"}))
            .node("subnet", "Subnet", json!({"name": "subnet"}))
            .node("rtb", "RouteTable", json!({"name": "rtb", "is_main": false, "ipv4_default_target": "igw"}))
            .node("igw", "InternetGateway", json!({"name": "igw"}))
            .node("web", "Instance", json!({"name": "web", "state": "running", "public_ip": "198.51.100.1"}))
            .edge("internet", "ALLOWS", "sg", json!({"protocol": "tcp", "ports": "443", "rule": "a"}))
            .edge("web", "PROTECTED_BY", "sg", json!({}))
            .edge("web", "IN_SUBNET", "subnet", json!({}))
            .edge("subnet", "IN_VPC", "vpc", json!({}))
            .edge("subnet", "USES_ROUTE_TABLE", "rtb", json!({}))
            .edge("rtb", "ROUTES_VIA", "igw", json!({"destination": "0.0.0.0/0", "state": "active", "rule": "r"}))
    }

    fn without_edge(mut builder: Builder, rel_type: &str) -> Builder {
        builder.data.edges.retain(|edge| edge.rel_type != rel_type);
        builder
    }

    fn set(mut builder: Builder, id: &str, key: &str, value: Option<Value>) -> Builder {
        let node = builder.data.nodes.iter_mut().find(|node| node.properties["khonsu_id"] == json!(id)).unwrap();
        match value {
            Some(value) => node.properties.insert(key.into(), value),
            None => node.properties.remove(key),
        };
        builder
    }

    fn only<'a>(report: &'a Report, rule: &str, resource: &str) -> &'a Finding {
        let mut found = report.findings.iter().filter(|finding| finding.rule_id == rule && finding.resource == format!("node:{resource}"));
        let first = found.next().unwrap_or_else(|| panic!("no {rule} finding on {resource}: {report:#?}"));
        assert!(found.next().is_none());
        first
    }

    fn status(finding: &Finding, id: &str) -> Status {
        finding.conditions.iter().find(|condition| condition.id == id).unwrap_or_else(|| panic!("no condition {id}: {finding:#?}")).status
    }

    #[test]
    fn every_direct_condition_met_is_a_potential_exposure_with_its_path_and_rule() {
        let report = exposed().report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(finding.verdict, Verdict::PotentialExposure);
        assert!(finding.unknowns.is_empty());
        let path = finding.path.as_ref().unwrap();
        assert_eq!(path.rule_id, Field::Known(RULE_DIRECT.to_string()));
        assert_eq!(path.hops, ["Internet -[ALLOWS]-> sg", "sg <-[PROTECTED_BY]- web"]);
        assert!(finding.contributing.contains(&"node:rtb".to_string()), "the route is part of the case: {:?}", finding.contributing);
        assert!(finding.evidence.iter().any(|record| record.evidence_id == "node:rtb"));
        assert!(!finding.assumptions.is_empty());
        let walk = finding.walk.as_ref().unwrap();
        assert_eq!((walk.from.as_str(), walk.to.as_str(), walk.direction, walk.max_hops), ("internet", "web", Direction::Both, 2));
        assert_eq!(walk.types, ["ALLOWS", "PROTECTED_BY"]);
    }

    #[test]
    fn a_security_group_rule_alone_is_not_potential_exposure() {
        // The same open rule, with each other fact taken away in turn.
        let no_address = set(exposed(), "web", "public_ip", None);
        let no_route = without_edge(exposed(), "ROUTES_VIA");
        let stopped = set(exposed(), "web", "state", Some(json!("stopped")));
        for (case, builder) in [("no address", no_address), ("no route", no_route), ("stopped", stopped)] {
            let report = builder.report();
            let finding = only(&report, RULE_DIRECT, "web");
            assert_eq!(status(finding, "open-inbound-rule"), Status::Met, "{case}");
            assert_eq!(finding.verdict, Verdict::NotIndicated, "{case}");
        }
    }

    #[test]
    fn no_verdict_ever_says_reachable() {
        // The names are the whole vocabulary, so this holds for any input.
        for verdict in [Verdict::PotentialExposure, Verdict::InsufficientData, Verdict::NotIndicated] {
            assert!(!serde_json::to_string(&verdict).unwrap().contains("reachable"));
        }
        assert!(CLAIM.contains("does not show that anything is reachable"));
    }

    #[test]
    fn what_was_not_read_is_unknown_not_absent() {
        // No group edges: every instance has a group, so they were not read.
        let report = without_edge(exposed(), "PROTECTED_BY").report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(finding.verdict, Verdict::InsufficientData);
        assert_eq!(status(finding, "open-inbound-rule"), Status::Unknown);
        assert_eq!(finding.unknowns.len(), 1);
        assert!(finding.path.is_none());

        // No route tables recorded at all.
        let report = without_edge(without_edge(exposed(), "USES_ROUTE_TABLE"), "ROUTES_VIA").report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(status(finding, "internet-route"), Status::Unknown);
        assert_eq!(finding.verdict, Verdict::InsufficientData);

        // No subnet, and no state.
        let report = without_edge(exposed(), "IN_SUBNET").report();
        assert_eq!(status(only(&report, RULE_DIRECT, "web"), "internet-route"), Status::Unknown);
        let report = set(exposed(), "web", "state", None).report();
        assert_eq!(status(only(&report, RULE_DIRECT, "web"), "instance-running"), Status::Unknown);
    }

    #[test]
    fn a_fact_that_rules_it_out_outweighs_an_unknown() {
        let report = set(without_edge(exposed(), "USES_ROUTE_TABLE"), "web", "public_ip", None).report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(finding.verdict, Verdict::NotIndicated);
        assert_eq!(finding.unknowns.len(), 1, "the unknown is still said");
    }

    #[test]
    fn a_subnet_without_its_own_table_uses_the_main_one() {
        let main = without_edge(exposed(), "USES_ROUTE_TABLE").edge("rtb", "IN_VPC", "vpc", json!({}));
        let report = set(main, "rtb", "is_main", Some(json!(true))).report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(status(finding, "internet-route"), Status::Met);
        assert_eq!(finding.verdict, Verdict::PotentialExposure);

        // A table in the VPC that is not the main one says nothing about it.
        let not_main = without_edge(exposed(), "USES_ROUTE_TABLE").edge("rtb", "IN_VPC", "vpc", json!({}));
        assert_eq!(status(only(&not_main.report(), RULE_DIRECT, "web"), "internet-route"), Status::Unknown);
    }

    fn with_route(destination: &str, state: &str) -> Builder {
        without_edge(exposed(), "ROUTES_VIA").edge("rtb", "ROUTES_VIA", "igw", json!({"destination": destination, "state": state, "rule": "r"}))
    }

    #[test]
    fn a_route_counts_only_when_active_default_and_of_the_address_family() {
        let route = |builder: Builder| status(only(&builder.report(), RULE_DIRECT, "web"), "internet-route");
        assert_eq!(route(with_route("0.0.0.0/0", "blackhole")), Status::NotMet);
        assert_eq!(route(with_route("203.0.113.0/24", "active")), Status::Unknown, "a narrower route is not a default one");
        assert_eq!(route(with_route("::/0", "active")), Status::NotMet, "an IPv4 address and a route for IPv6");

        let ipv6 = set(set(with_route("::/0", "active"), "web", "public_ip", None), "web", "ipv6_address", Some(json!("2001:db8::1")));
        let report = ipv6.report();
        assert_eq!(only(&report, RULE_DIRECT, "web").verdict, Verdict::PotentialExposure);
    }

    #[test]
    fn a_narrower_cidr_is_not_the_internet() {
        let mut builder = exposed();
        builder.data.nodes[0].properties.insert("cidr".into(), json!("203.0.113.0/24"));
        let report = builder.report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(status(finding, "open-inbound-rule"), Status::NotMet);
        assert!(finding.path.is_none());
    }

    fn balanced(target_rule: Value, target_source: &str, health: &str) -> Builder {
        Builder::default()
            .node("internet", "Cidr", json!({"name": "Internet", "cidr": "0.0.0.0/0"}))
            .node("office", "Cidr", json!({"name": "office", "cidr": "10.0.0.0/8"}))
            .node("sg-lb", "SecurityGroup", json!({"name": "sg-lb"}))
            .node("sg-app", "SecurityGroup", json!({"name": "sg-app"}))
            .node("lb", "LoadBalancer", json!({"name": "lb", "scheme": "internet-facing"}))
            .node("tg", "TargetGroup", json!({"name": "tg", "protocol": "HTTP", "port": 8080}))
            .node("app", "Instance", json!({"name": "app", "state": "running"}))
            .edge("internet", "ROUTES_TO", "lb", json!({}))
            .edge("internet", "ALLOWS", "sg-lb", json!({"protocol": "tcp", "ports": "443", "rule": "a"}))
            .edge("lb", "PROTECTED_BY", "sg-lb", json!({}))
            .edge("lb", "FORWARDS_TO", "tg", json!({}))
            .edge("tg", "ROUTES_TO", "app", json!({"health": health}))
            .edge("app", "PROTECTED_BY", "sg-app", json!({}))
            .edge(target_source, "ALLOWS", "sg-app", target_rule)
    }

    fn app_rule(ports: &str) -> Value {
        json!({"protocol": "tcp", "ports": ports, "rule": "b"})
    }

    #[test]
    fn a_target_behind_a_public_balancer_needs_its_own_group_to_allow_the_port() {
        let report = balanced(app_rule("8080"), "sg-lb", "healthy").report();
        let finding = only(&report, RULE_BALANCER, "app");
        assert_eq!(finding.verdict, Verdict::PotentialExposure, "{finding:#?}");
        assert_eq!(finding.path.as_ref().unwrap().hops, ["Internet -[ROUTES_TO]-> lb", "lb -[FORWARDS_TO]-> tg", "tg -[ROUTES_TO]-> app"]);
        assert_eq!(finding.walk.as_ref().unwrap().direction, Direction::Out);
        assert_eq!(only(&report, RULE_DIRECT, "app").verdict, Verdict::NotIndicated, "it has no public address of its own");

        // The port range matters, and so does whom the rule lets in.
        let status_for = |builder: Builder| status(only(&builder.report(), RULE_BALANCER, "app"), "target-allows-balancer");
        assert_eq!(status_for(balanced(app_rule("8000-8100"), "sg-lb", "healthy")), Status::Met);
        assert_eq!(status_for(balanced(app_rule("all"), "sg-lb", "healthy")), Status::Met);
        assert_eq!(status_for(balanced(app_rule("9000"), "sg-lb", "healthy")), Status::NotMet);
        assert_eq!(status_for(balanced(app_rule("8080"), "office", "healthy")), Status::Unknown, "a CIDR may or may not hold the balancer's addresses");
        assert_eq!(status_for(balanced(app_rule("8080"), "internet", "healthy")), Status::Met);
    }

    #[test]
    fn an_internal_balancer_or_an_unhealthy_target_does_not_give_a_potential_exposure() {
        let internal = set(balanced(app_rule("8080"), "sg-lb", "healthy"), "lb", "scheme", Some(json!("internal")));
        assert_eq!(only(&internal.report(), RULE_BALANCER, "app").verdict, Verdict::NotIndicated);

        let unhealthy = balanced(app_rule("8080"), "sg-lb", "unhealthy").report();
        let finding = only(&unhealthy, RULE_BALANCER, "app");
        assert_eq!(finding.verdict, Verdict::InsufficientData);
        assert_eq!(status(finding, "target-healthy"), Status::Unknown);

        // A network load balancer can have no security group to read.
        let no_groups = without_edge(balanced(app_rule("8080"), "internet", "healthy"), "PROTECTED_BY");
        let report = no_groups.report();
        let finding = only(&report, RULE_BALANCER, "app");
        assert_eq!(status(finding, "balancer-open-rule"), Status::Unknown);
    }

    /// `web` is exposed; `db` allows `web`'s group.
    fn with_database(builder: Builder) -> Builder {
        builder
            .node("sg-db", "SecurityGroup", json!({"name": "sg-db"}))
            .node("db", "Instance", json!({"name": "db", "state": "running"}))
            .edge("db", "PROTECTED_BY", "sg-db", json!({}))
            .edge("sg", "ALLOWS", "sg-db", json!({"protocol": "tcp", "ports": "5432", "rule": "c"}))
    }

    #[test]
    fn one_step_behind_an_exposed_instance_links_to_the_finding_it_rests_on() {
        let report = with_database(exposed()).report();
        let web = only(&report, RULE_DIRECT, "web");
        let db = only(&report, RULE_LATERAL, "db");
        assert_eq!(db.verdict, Verdict::PotentialExposure);
        assert_eq!(db.depends_on.as_ref(), Some(&web.finding_id));
        assert_eq!(
            db.path.as_ref().unwrap().hops,
            ["Internet -[ALLOWS]-> sg", "sg <-[PROTECTED_BY]- web", "web -[PROTECTED_BY]-> sg", "sg -[ALLOWS]-> sg-db", "sg-db <-[PROTECTED_BY]- db"]
        );
        assert_eq!(only(&report, RULE_DIRECT, "db").verdict, Verdict::NotIndicated);
    }

    #[test]
    fn a_step_behind_an_undecided_instance_is_undecided_and_behind_a_safe_one_is_silent() {
        let undecided = with_database(without_edge(without_edge(exposed(), "USES_ROUTE_TABLE"), "ROUTES_VIA")).report();
        let db = only(&undecided, RULE_LATERAL, "db");
        assert_eq!(db.verdict, Verdict::InsufficientData);
        assert_eq!(status(db, "source-potentially-exposed"), Status::Unknown);

        let not_exposed = with_database(set(exposed(), "web", "public_ip", None)).report();
        assert!(not_exposed.findings.iter().all(|finding| finding.rule_id != RULE_LATERAL), "{not_exposed:#?}");
    }

    #[test]
    fn a_stopped_target_of_a_lateral_rule_is_not_indicated() {
        let builder = set(with_database(exposed()), "db", "state", Some(json!("stopped")));
        assert_eq!(only(&builder.report(), RULE_LATERAL, "db").verdict, Verdict::NotIndicated);
    }

    #[test]
    fn findings_come_in_a_stated_order_and_input_order_does_not_change_them() {
        let builder = with_database(exposed()).node("idle", "Instance", json!({"name": "idle", "state": "running", "public_ip": "198.51.100.9"}));
        let report = builder.report();
        let order: Vec<(Verdict, &str, &str)> = report.findings.iter().map(|finding| (finding.verdict, finding.rule_id.as_str(), finding.resource.as_str())).collect();
        assert_eq!(
            order,
            [
                (Verdict::PotentialExposure, RULE_DIRECT, "node:web"),
                (Verdict::PotentialExposure, RULE_LATERAL, "node:db"),
                (Verdict::InsufficientData, RULE_DIRECT, "node:idle"),
                (Verdict::NotIndicated, RULE_DIRECT, "node:db"),
            ],
            "{report:#?}"
        );
        assert_eq!(report.summary, Summary { potential_exposure: 2, insufficient_data: 1, not_indicated: 1 });
        assert_eq!(report.instances, 3);

        let mut shuffled = builder.data.clone();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        assert_eq!(evaluate(&shuffled, &[], true), report);
    }

    #[test]
    fn the_same_records_keep_their_finding_id_and_a_cut_is_counted() {
        let report = exposed().report();
        assert_eq!(report.findings[0].finding_id, exposed().report().findings[0].finding_id);
        assert_ne!(report.findings[0].finding_id, only(&with_database(exposed()).report(), RULE_LATERAL, "db").finding_id);

        let (narrow, cut) = with_database(exposed()).report().narrowed(|_| true, 1);
        assert_eq!((narrow.findings.len(), cut), (1, 2));
        assert_eq!(narrow.summary.potential_exposure, 2, "the summary still counts everything");
    }

    #[test]
    fn secrets_on_a_contributing_record_stay_masked() {
        let report = set(exposed(), "web", "api_token", Some(json!("abc123"))).report();
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("abc123"), "{text}");
        assert!(text.contains("api_token"), "it is named as masked");
    }
}

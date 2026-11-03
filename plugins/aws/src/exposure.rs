//! Explainable exposure rules: which instances the configuration in a graph
//! leaves potentially open to the internet, and exactly why.
//!
//! A rule is a list of conditions, each met, not met or unknown, and each
//! naming the fields it read. The verdict follows from those alone: anything
//! not met gives [`NOT_INDICATED`], otherwise anything unknown gives
//! [`INSUFFICIENT_DATA`], otherwise [`POTENTIAL_EXPOSURE`]. There is no score,
//! and no verdict says "reachable": the graph holds configuration, and nothing
//! here sends traffic. See `docs/exposure-rules.md`.
//!
//! A record missing from the graph is not read as a fact about the account: a
//! saved response that was absent reads as empty, so an instance with no
//! security groups or a subnet with no route table gives "unknown".
//!
//! This all lives in the plugin because it is all about AWS. The host sends the
//! records named in [`info`], and checks and shows what comes back.

use std::collections::{BTreeMap, BTreeSet};

use khonsu_plugin_protocol::{
    fnv1a, AnalysisEdge, AnalysisInfo, AnalysisNode, AnalyzeRequest, ConditionRecord, ConditionStatus as Status, FieldRef, FindingRecord, HopRef, PathRef, RuleInfo, SourceCoverage, VerdictInfo, VerdictTone,
};
use serde_json::Value;

pub const ANALYSIS: &str = "exposure";
const RULE_DIRECT: &str = "aws.exposure.direct-instance";
const RULE_BALANCER: &str = "aws.exposure.public-load-balancer";
const RULE_LATERAL: &str = "aws.exposure.security-group-lateral";

/// Every condition is met by configuration. There is deliberately no "reachable".
const POTENTIAL_EXPOSURE: &str = "potential_exposure";
/// None ruled it out and at least one could not be evaluated, or a source behind
/// its records was not read completely.
const INSUFFICIENT_DATA: &str = "insufficient_data";
/// A condition is not met, so this rule does not point at it.
const NOT_INDICATED: &str = "not_indicated";

const CLAIM: &str = "Potential exposure worked out from configuration. No traffic was sent, so this does not show that anything is reachable, and a resource with no finding is not shown to be safe.";
const NOT_READ_NETWORK: &str = "Network ACLs, host firewalls and anything outside this AWS account are not read.";
const FAMILY: &str = "0.0.0.0/0 and ::/0 are one Internet record, so a rule's address family is not compared with the instance's address.";
const DERIVATION: &str = "the rule's candidate path";

fn rules() -> Vec<RuleInfo> {
    let rule = |id: &str, title: &str, statement: &str, assumptions: &[&str]| RuleInfo {
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

/// What the host needs to offer this analysis, read for it, and show its result.
pub fn info() -> AnalysisInfo {
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
    let verdict = |id: &str, title: &str, tone: VerdictTone| VerdictInfo { id: id.into(), title: title.into(), tone };
    let zh: BTreeMap<String, String> = [
        ("title", "暴露分析"),
        ("claim", "根据配置推断的潜在暴露。未发送任何流量，因此不能说明资源可达；没有发现也不能说明资源安全。"),
        ("verdict.potential_exposure", "潜在暴露"),
        ("verdict.insufficient_data", "数据不足"),
        ("verdict.not_indicated", "规则未指向"),
        ("rule.aws.exposure.direct-instance", "实例直接向互联网开放"),
        ("rule.aws.exposure.public-load-balancer", "位于面向互联网的负载均衡器之后"),
        ("rule.aws.exposure.security-group-lateral", "位于潜在暴露实例之后一条安全组规则"),
        ("condition.instance-running", "实例正在运行"),
        ("condition.public-address", "实例有公网地址"),
        ("condition.open-inbound-rule", "实例有向互联网开放的安全组规则"),
        ("condition.internet-route", "子网有通往互联网网关的活动默认路由"),
        ("condition.internet-facing", "负载均衡器面向互联网"),
        ("condition.balancer-open-rule", "负载均衡器有向互联网开放的安全组规则"),
        ("condition.target-healthy", "目标健康"),
        ("condition.target-allows-balancer", "目标的安全组允许来自负载均衡器的目标端口"),
        ("condition.source-potentially-exposed", "来源实例有潜在暴露发现"),
        ("condition.group-allows-source", "安全组规则允许来源实例的安全组"),
    ]
    .into_iter()
    .map(|(key, text)| (key.to_string(), text.to_string()))
    .collect();
    AnalysisInfo {
        id: ANALYSIS.into(),
        title: "Exposure".into(),
        description: "Which instances the configuration leaves potentially open to the internet, and why.".into(),
        claim: CLAIM.into(),
        subject_labels: names(&["Instance"]),
        // An instance with no relationships still counts as looked at.
        node_labels: names(&["Instance"]),
        relationship_types: names(&["ALLOWS", "PROTECTED_BY", "ROUTES_TO", "FORWARDS_TO", "IN_SUBNET", "IN_VPC", "USES_ROUTE_TABLE", "ROUTES_VIA"]),
        verdicts: vec![
            verdict(POTENTIAL_EXPOSURE, "Potential exposure", VerdictTone::Attention),
            verdict(INSUFFICIENT_DATA, "Insufficient data", VerdictTone::Undecided),
            verdict(NOT_INDICATED, "Not indicated by this rule", VerdictTone::Quiet),
        ],
        rules: rules(),
        translations: BTreeMap::from([("zh".to_string(), zh)]),
    }
}

type ByNode<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<&'a AnalysisEdge>>>;

/// Stable lookups over the records, sorted so that every choice is repeatable.
struct Index<'a> {
    nodes: BTreeMap<&'a str, &'a AnalysisNode>,
    /// Relationships by node then type, in the order of their references.
    out: ByNode<'a>,
    into: ByNode<'a>,
}

impl<'a> Index<'a> {
    fn new(request: &'a AnalyzeRequest) -> Self {
        let mut out: ByNode = BTreeMap::new();
        let mut into: ByNode = BTreeMap::new();
        for edge in &request.edges {
            out.entry(&edge.from).or_default().entry(&edge.rel_type).or_default().push(edge);
            into.entry(&edge.to).or_default().entry(&edge.rel_type).or_default().push(edge);
        }
        out.values_mut().chain(into.values_mut()).flat_map(BTreeMap::values_mut).for_each(|edges| edges.sort_by(|a, b| a.reference.cmp(&b.reference)));
        Self {
            nodes: request.nodes.iter().map(|node| (node.reference.as_str(), node)).collect(),
            out,
            into,
        }
    }

    fn with_label(&self, label: &str) -> Vec<&'a AnalysisNode> {
        self.nodes.values().copied().filter(|node| has_label(node, label)).collect()
    }

    fn out_of(&self, node: &str, rel_type: &'static str) -> &[&'a AnalysisEdge] {
        self.out.get(node).and_then(|types| types.get(rel_type)).map_or(&[], Vec::as_slice)
    }

    fn arriving_at(&self, node: &str, rel_type: &'static str) -> &[&'a AnalysisEdge] {
        self.into.get(node).and_then(|types| types.get(rel_type)).map_or(&[], Vec::as_slice)
    }

    fn node(&self, reference: &str) -> Option<&'a AnalysisNode> {
        self.nodes.get(reference).copied()
    }

    /// `ALLOWS` rules into `groups` from the internet.
    fn internet_rules(&self, groups: &[&AnalysisEdge]) -> Vec<&'a AnalysisEdge> {
        groups
            .iter()
            .flat_map(|protected| self.arriving_at(&protected.to, "ALLOWS"))
            .filter(|rule| self.node(&rule.from).is_some_and(is_internet))
            .copied()
            .collect()
    }

    /// Every (balancer, target group, group → instance, balancer → group) that
    /// sends traffic to `instance`.
    fn balancers_of(&self, instance: &AnalysisNode) -> Vec<(&'a AnalysisNode, &'a AnalysisNode, &'a AnalysisEdge, &'a AnalysisEdge)> {
        let mut found = Vec::new();
        for to_target in self.arriving_at(&instance.reference, "ROUTES_TO") {
            let Some(group) = self.node(&to_target.from).filter(|node| has_label(node, "TargetGroup")) else { continue };
            for forwards in self.arriving_at(&group.reference, "FORWARDS_TO") {
                if let Some(balancer) = self.node(&forwards.from) {
                    found.push((balancer, group, *to_target, *forwards));
                }
            }
        }
        found
    }

    /// The table a subnet uses: its own association, else its VPC's main one.
    fn route_table_of(&self, subnet: &AnalysisNode) -> Option<&'a AnalysisNode> {
        if let Some(explicit) = self.out_of(&subnet.reference, "USES_ROUTE_TABLE").first() {
            return self.node(&explicit.to);
        }
        let vpc = &self.out_of(&subnet.reference, "IN_VPC").first()?.to;
        self.arriving_at(vpc, "IN_VPC")
            .iter()
            .filter_map(|edge| self.node(&edge.from))
            .find(|node| has_label(node, "RouteTable") && node.properties.get("is_main") == Some(&Value::Bool(true)))
    }
}

fn has_label(node: &AnalysisNode, label: &str) -> bool {
    node.labels.iter().any(|name| name == label)
}

fn is_internet(node: &AnalysisNode) -> bool {
    has_label(node, "Cidr") && matches!(node.properties.get("cidr").and_then(Value::as_str), Some("0.0.0.0/0" | "::/0"))
}

fn field(record: &str, name: &str, source_field: &str) -> FieldRef {
    FieldRef { record: record.into(), field: name.into(), source_field: source_field.into() }
}

fn text<'a>(edge: &'a AnalysisEdge, key: &str) -> &'a str {
    edge.properties.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A rule's ports in words, as the plugin wrote them.
fn ports_of(rule: &AnalysisEdge) -> String {
    let word = |key: &str| rule.properties.get(key).and_then(Value::as_str).unwrap_or("unknown").to_string();
    format!("{} {}", word("protocol"), word("ports"))
}

/// Whether a security group rule covers `port` on `protocol`. The plugin writes
/// ports as `all`, `443` or `8000-8100`.
fn rule_covers(rule: &AnalysisEdge, protocol: &str, port: i64) -> bool {
    if text(rule, "protocol") != "all" && text(rule, "protocol") != protocol {
        return false;
    }
    let ports = text(rule, "ports");
    if ports == "all" {
        return true;
    }
    match ports.split_once('-') {
        Some((low, high)) => matches!((low.parse::<i64>(), high.parse::<i64>()), (Ok(low), Ok(high)) if (low..=high).contains(&port)),
        None => ports.parse() == Ok(port),
    }
}

/// The IP protocols a target group's traffic can use, which is more than one for
/// `TCP_UDP`. Its protocol is one of the names AWS gives: anything else that
/// reaches a security group is TCP (`HTTP`, `HTTPS`, `TCP`, `TLS`).
fn traffic_protocols(group_protocol: &str) -> &'static [&'static str] {
    match group_protocol {
        "UDP" | "QUIC" | "GENEVE" => &["udp"],
        "TCP_UDP" | "TCP_QUIC" => &["tcp", "udp"],
        _ => &["tcp"],
    }
}

fn condition(id: &str, statement: &str, status: Status, detail: impl Into<String>, fields: Vec<FieldRef>) -> ConditionRecord {
    ConditionRecord { id: id.into(), statement: statement.into(), status, detail: detail.into(), fields }
}

fn running(instance: &AnalysisNode) -> ConditionRecord {
    let (status, detail) = match instance.properties.get("state").and_then(Value::as_str) {
        Some("running") => (Status::Met, "The instance is running.".to_string()),
        Some(state) => (Status::NotMet, format!("The instance is {state}.")),
        None => (Status::Unknown, "The instance's state was not supplied.".to_string()),
    };
    condition("instance-running", "The instance is running.", status, detail, vec![field(&instance.reference, "state", "ec2 describe-instances: State.Name")])
}

/// Which security group each of `protected` is.
fn membership(protected: &[&AnalysisEdge], source_field: &str) -> Vec<FieldRef> {
    protected.iter().map(|edge| field(&edge.reference, "@to", source_field)).collect()
}

/// Whether the groups in `protected` have an open rule, with no groups at all
/// being unknown: every instance has one, so none means they were not read.
fn open_rule(index: &Index, id: &str, whose: &str, protected: &[&AnalysisEdge], aws_groups: &str) -> ConditionRecord {
    let statement = format!("{whose} has a security group rule open to the internet.");
    let rules = index.internet_rules(protected);
    if protected.is_empty() {
        let detail = "No security groups are recorded for it, so its rules could not be read.";
        return condition(id, &statement, Status::Unknown, detail, vec![]);
    }
    let mut fields = membership(protected, aws_groups);
    for rule in &rules {
        fields.push(field(&rule.reference, "protocol", "ec2 describe-security-groups: IpPermissions[].IpProtocol"));
        fields.push(field(&rule.reference, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
    }
    if rules.is_empty() {
        return condition(id, &statement, Status::NotMet, "None of its security groups has a rule from 0.0.0.0/0 or ::/0.", fields);
    }
    let ports: BTreeSet<String> = rules.iter().map(|rule| ports_of(rule)).collect();
    let detail = format!("Open to the internet on {}.", ports.into_iter().collect::<Vec<_>>().join(", "));
    condition(id, &statement, Status::Met, detail, fields)
}

fn verdict_of(conditions: &[ConditionRecord]) -> &'static str {
    if conditions.iter().any(|condition| condition.status == Status::NotMet) {
        NOT_INDICATED
    } else if conditions.iter().any(|condition| condition.status == Status::Unknown) {
        INSUFFICIENT_DATA
    } else {
        POTENTIAL_EXPOSURE
    }
}

/// A verdict's place in the order findings come in.
fn rank(verdict: &str) -> u8 {
    match verdict {
        POTENTIAL_EXPOSURE => 0,
        INSUFFICIENT_DATA => 1,
        _ => 2,
    }
}

fn public_address(instance: &AnalysisNode) -> ConditionRecord {
    let fields = vec![
        field(&instance.reference, "public_ip", "ec2 describe-instances: PublicIpAddress"),
        field(&instance.reference, "ipv6_address", "ec2 describe-instances: Ipv6Address"),
    ];
    let addresses: Vec<&str> = ["public_ip", "ipv6_address"].iter().filter_map(|key| instance.properties.get(*key).and_then(Value::as_str)).collect();
    let (status, detail) = if addresses.is_empty() {
        (Status::NotMet, "AWS reported no public IPv4 or IPv6 address.".to_string())
    } else {
        (Status::Met, format!("Public address {}.", addresses.join(", ")))
    };
    condition("public-address", "The instance has a public address.", status, detail, fields)
}

fn internet_route(index: &Index, instance: &AnalysisNode) -> (ConditionRecord, Vec<String>) {
    let id = "internet-route";
    let statement = "The instance's subnet has an active default route to an internet gateway.";
    let Some(subnet) = index.out_of(&instance.reference, "IN_SUBNET").first().and_then(|edge| index.node(&edge.to)) else {
        return (condition(id, statement, Status::Unknown, "No subnet is recorded for the instance.", vec![]), vec![]);
    };
    let Some(table) = index.route_table_of(subnet) else {
        let detail = "No route table is recorded for the subnet or as its VPC's main table, so its routes could not be read.";
        return (condition(id, statement, Status::Unknown, detail, vec![]), vec![subnet.reference.clone()]);
    };
    let wanted: Vec<&str> = match (instance.properties.contains_key("public_ip"), instance.properties.contains_key("ipv6_address")) {
        (true, false) => vec!["0.0.0.0/0"],
        (false, true) => vec!["::/0"],
        _ => vec!["0.0.0.0/0", "::/0"],
    };
    let routes = index.out_of(&table.reference, "ROUTES_VIA");
    let is_v6 = |destination: &str| destination.contains(':');
    // Only routes of the address's own family can carry its traffic.
    let active: Vec<&&AnalysisEdge> = routes
        .iter()
        .filter(|route| text(route, "state") == "active" && wanted.iter().any(|family| is_v6(family) == is_v6(text(route, "destination"))))
        .collect();
    let mut fields = vec![
        field(&table.reference, "ipv4_default_target", "ec2 describe-route-tables: Routes[DestinationCidrBlock=0.0.0.0/0]"),
        field(&table.reference, "ipv6_default_target", "ec2 describe-route-tables: Routes[DestinationIpv6CidrBlock=::/0]"),
    ];
    for route in routes {
        fields.push(field(&route.reference, "destination", "ec2 describe-route-tables: Routes[].DestinationCidrBlock"));
        fields.push(field(&route.reference, "state", "ec2 describe-route-tables: Routes[].State"));
    }
    let name = &table.name;
    let (status, detail) = if let Some(route) = active.iter().find(|route| wanted.contains(&text(route, "destination"))) {
        (Status::Met, format!("Route table {name} sends {} to an internet gateway.", text(route, "destination")))
    } else if let Some(route) = active.first() {
        (Status::Unknown, format!("Route table {name} sends only {} to an internet gateway, so whether it covers a given client is not known.", text(route, "destination")))
    } else {
        (Status::NotMet, format!("Route table {name} has no active default route to an internet gateway."))
    };
    let mut records = vec![subnet.reference.clone(), table.reference.clone()];
    records.extend(routes.iter().map(|route| route.reference.clone()));
    (condition(id, statement, status, detail, fields), records)
}

fn along(edge: &AnalysisEdge) -> HopRef {
    HopRef { edge: edge.reference.clone(), along: true }
}

fn against(edge: &AnalysisEdge) -> HopRef {
    HopRef { edge: edge.reference.clone(), along: false }
}

fn direct(request: &AnalyzeRequest, index: &Index, instance: &AnalysisNode) -> Built {
    let protected = index.out_of(&instance.reference, "PROTECTED_BY");
    let open = open_rule(index, "open-inbound-rule", "The instance", protected, "ec2 describe-instances: SecurityGroups[].GroupId");
    let (route, route_records) = internet_route(index, instance);
    let conditions = vec![running(instance), public_address(instance), open, route];

    // The first open rule, with the group it opens, is the candidate path.
    let hops = protected.iter().find_map(|edge| {
        let rule = index.arriving_at(&edge.to, "ALLOWS").iter().find(|rule| index.node(&rule.from).is_some_and(is_internet))?;
        Some(vec![along(rule), against(edge)])
    });
    let mut records = route_records;
    records.push(instance.reference.clone());
    records.extend(protected.iter().map(|edge| edge.reference.clone()));
    records.extend(index.internet_rules(protected).iter().map(|rule| rule.reference.clone()));
    build(request, instance, RULE_DIRECT, conditions, hops.unwrap_or_default(), records, None)
}

fn behind_balancer(request: &AnalyzeRequest, index: &Index, instance: &AnalysisNode, balancer: &AnalysisNode, group: &AnalysisNode, to_target: &AnalysisEdge, forwards: &AnalysisEdge) -> Built {
    let balancer_groups = index.out_of(&balancer.reference, "PROTECTED_BY");
    let instance_groups = index.out_of(&instance.reference, "PROTECTED_BY");

    let (status, detail) = match balancer.properties.get("scheme").and_then(Value::as_str) {
        Some("internet-facing") => (Status::Met, "The load balancer is internet-facing.".to_string()),
        Some(scheme) => (Status::NotMet, format!("The load balancer's scheme is {scheme}.")),
        None => (Status::Unknown, "The load balancer's scheme was not supplied.".to_string()),
    };
    let facing = condition("internet-facing", "The load balancer is internet-facing.", status, detail, vec![field(&balancer.reference, "scheme", "elbv2 describe-load-balancers: Scheme")]);
    let open = open_rule(index, "balancer-open-rule", "The load balancer", balancer_groups, "elbv2 describe-load-balancers: SecurityGroups[]");
    let health = {
        let read = field(&to_target.reference, "health", "elbv2 describe-target-health: TargetHealth.State");
        let (status, detail) = match to_target.properties.get("health").and_then(Value::as_str) {
            Some("healthy") => (Status::Met, "The target was healthy.".to_string()),
            Some(state) => (Status::Unknown, format!("The target's health was {state}, which can change without the configuration changing.")),
            None => (Status::Unknown, "The target's health was not supplied.".to_string()),
        };
        condition("target-healthy", "The target is healthy.", status, detail, vec![read])
    };
    let allows = target_allows_balancer(index, group, balancer_groups, instance_groups);
    let conditions = vec![running(instance), facing, open, health, allows];

    let opening = index.arriving_at(&balancer.reference, "ROUTES_TO").iter().find(|edge| index.node(&edge.from).is_some_and(is_internet));
    let hops = opening.map_or_else(Vec::new, |edge| vec![along(edge), along(forwards), along(to_target)]);
    let mut records = vec![instance.reference.clone(), balancer.reference.clone(), group.reference.clone(), to_target.reference.clone(), forwards.reference.clone()];
    records.extend(balancer_groups.iter().map(|edge| edge.reference.clone()));
    records.extend(index.internet_rules(balancer_groups).iter().map(|rule| rule.reference.clone()));
    build(request, instance, RULE_BALANCER, conditions, hops, records, Some(format!("{}\n{}", balancer.reference, group.reference)))
}

/// Whether the target's groups let the balancer in on the target group's port.
fn target_allows_balancer(index: &Index, group: &AnalysisNode, balancer_groups: &[&AnalysisEdge], instance_groups: &[&AnalysisEdge]) -> ConditionRecord {
    let id = "target-allows-balancer";
    let statement = "The target's security group allows the target group's port from the load balancer.";
    let port_field = field(&group.reference, "port", "elbv2 describe-target-groups: Port");
    let protocol_field = field(&group.reference, "protocol", "elbv2 describe-target-groups: Protocol");
    let Some(port) = group.properties.get("port").and_then(Value::as_i64) else {
        return condition(id, statement, Status::Unknown, "The target group's port was not supplied.", vec![port_field]);
    };
    if instance_groups.is_empty() {
        let detail = "No security groups are recorded for the target, so its rules could not be read.";
        return condition(id, statement, Status::Unknown, detail, vec![port_field]);
    }
    let Some(group_protocol) = group.properties.get("protocol").and_then(Value::as_str) else {
        return condition(id, statement, Status::Unknown, "The target group's protocol was not supplied.", vec![port_field, protocol_field]);
    };
    let protocols = traffic_protocols(group_protocol);
    let covering = |rule: &AnalysisEdge| protocols.iter().find(|protocol| rule_covers(rule, protocol, port)).copied();
    let from_balancer: BTreeSet<&str> = balancer_groups.iter().map(|edge| edge.to.as_str()).collect();
    let rules: Vec<&AnalysisEdge> = instance_groups
        .iter()
        .flat_map(|edge| index.arriving_at(&edge.to, "ALLOWS"))
        .filter(|rule| covering(rule).is_some())
        .copied()
        .collect();
    let matching = rules.iter().find(|rule| from_balancer.contains(rule.from.as_str()) || index.node(&rule.from).is_some_and(is_internet));
    let other_cidr = rules.iter().find(|rule| index.node(&rule.from).is_some_and(|node| has_label(node, "Cidr")));
    let mut fields = vec![port_field, protocol_field];
    // Which groups the target is in is part of the answer, whichever way it went.
    fields.extend(membership(instance_groups, "ec2 describe-instances: SecurityGroups[].GroupId"));
    if let Some(rule) = matching.or(other_cidr) {
        fields.push(field(&rule.reference, "protocol", "ec2 describe-security-groups: IpPermissions[].IpProtocol"));
        fields.push(field(&rule.reference, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
    }
    let wanted = protocols.join(" or ");
    let (status, detail) = if let Some(rule) = matching {
        (Status::Met, format!("A rule allows {} {port} from the load balancer's group or the internet.", covering(rule).unwrap_or(protocols[0])))
    } else if let Some(rule) = other_cidr {
        (Status::Unknown, format!("A CIDR rule allows {} {port}, and the load balancer's addresses are not read to compare with it.", covering(rule).unwrap_or(protocols[0])))
    } else if balancer_groups.is_empty() {
        // Nothing says the balancer's groups are not among those a rule names.
        (Status::Unknown, "No security groups are recorded for the load balancer, so whether a rule lets it in could not be read.".to_string())
    } else {
        (Status::NotMet, format!("No rule on the target's groups allows {wanted} {port} from the load balancer."))
    };
    condition(id, statement, status, detail, fields)
}

/// One finding for each instance one group rule behind an instance that a rule
/// left potentially exposed or undecided. Findings about instances the rules
/// do not point at would only be noise.
fn lateral_findings(request: &AnalyzeRequest, index: &Index, findings: &[Built]) -> Vec<Built> {
    // The strongest finding on each instance, first by verdict then by rule.
    let mut best: BTreeMap<&str, &Built> = BTreeMap::new();
    for found in findings.iter().filter(|found| found.record.verdict != NOT_INDICATED) {
        best.entry(found.record.resource.as_str())
            .and_modify(|held| {
                if (rank(&found.record.verdict), &found.record.rule_id) < (rank(&held.record.verdict), &held.record.rule_id) {
                    *held = found;
                }
            })
            .or_insert(found);
    }
    let mut out = Vec::new();
    for source in index.with_label("Instance") {
        let Some(found) = best.get(source.reference.as_str()) else { continue };
        let mut pairs: BTreeMap<&str, (&AnalysisEdge, &AnalysisEdge, Vec<&AnalysisEdge>)> = BTreeMap::new();
        for &own in index.out_of(&source.reference, "PROTECTED_BY") {
            for &rule in index.out_of(&own.to, "ALLOWS") {
                for &protected in index.arriving_at(&rule.to, "PROTECTED_BY") {
                    if protected.from != source.reference && index.node(&protected.from).is_some_and(|node| has_label(node, "Instance")) {
                        pairs.entry(protected.from.as_str()).or_insert((own, protected, Vec::new())).2.push(rule);
                    }
                }
            }
        }
        for (target, (own, protected, rules)) in pairs {
            let Some(instance) = index.node(target) else { continue };
            out.push(lateral(request, instance, source, found, own, protected, &rules));
        }
    }
    out
}

fn lateral(request: &AnalyzeRequest, instance: &AnalysisNode, source: &AnalysisNode, found: &Built, own: &AnalysisEdge, protected: &AnalysisEdge, rules: &[&AnalysisEdge]) -> Built {
    let name = &source.name;
    let rule_id = &found.record.rule_id;
    let (status, detail) = match found.record.verdict.as_str() {
        POTENTIAL_EXPOSURE => (Status::Met, format!("{name} has a potential exposure finding from {rule_id}.")),
        _ => (Status::Unknown, format!("{name} has insufficient data under {rule_id}.")),
    };
    let exposed = condition("source-potentially-exposed", "The source instance has a potential exposure finding.", status, detail, vec![]);
    let ports: BTreeSet<String> = rules.iter().map(|rule| ports_of(rule)).collect();
    let mut rule_fields = Vec::new();
    for rule in rules {
        rule_fields.push(field(&rule.reference, "protocol", "ec2 describe-security-groups: IpPermissions[].IpProtocol"));
        rule_fields.push(field(&rule.reference, "ports", "ec2 describe-security-groups: IpPermissions[].FromPort, ToPort"));
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
    let mut hops = found.hops.clone();
    if !hops.is_empty() {
        hops.extend([along(own), along(rules[0]), against(protected)]);
    }
    let mut records = vec![instance.reference.clone(), source.reference.clone(), own.reference.clone(), protected.reference.clone()];
    records.extend(rules.iter().map(|rule| rule.reference.clone()));
    let mut built = build(request, instance, RULE_LATERAL, conditions, hops, records, Some(source.reference.clone()));
    built.record.depends_on = Some(found.record.finding_id.clone());
    built
}

/// A finding with the hops a later rule continues from.
struct Built {
    record: FindingRecord,
    hops: Vec<HopRef>,
}

/// The sources behind every record a finding rests on, whose last sync was not
/// complete, with what that sync was.
fn incomplete_sources<'a>(request: &'a AnalyzeRequest, conditions: &[ConditionRecord], hops: &[HopRef], records: &[String]) -> Vec<(&'a str, &'a SourceCoverage)> {
    let mut cited: BTreeSet<&str> = records.iter().map(String::as_str).collect();
    cited.extend(conditions.iter().flat_map(|condition| condition.fields.iter().map(|field| field.record.as_str())));
    for hop in hops {
        cited.insert(&hop.edge);
        if let Some(edge) = request.edges.iter().find(|edge| edge.reference == hop.edge) {
            cited.extend([edge.from.as_str(), edge.to.as_str()]);
        }
    }
    let of_nodes = request.nodes.iter().filter(|node| cited.contains(node.reference.as_str())).filter_map(|node| node.source.as_deref());
    let of_edges = request.edges.iter().filter(|edge| cited.contains(edge.reference.as_str())).filter_map(|edge| edge.source.as_deref());
    let behind: BTreeSet<&str> = of_nodes.chain(of_edges).collect();
    behind.into_iter().filter_map(|source| Some((source, request.coverage.get(source).filter(|coverage| !coverage.is_complete())?))).collect()
}

fn build(request: &AnalyzeRequest, instance: &AnalysisNode, rule_id: &str, conditions: Vec<ConditionRecord>, hops: Vec<HopRef>, records: Vec<String>, subject: Option<String>) -> Built {
    let mut unknowns: Vec<String> = conditions.iter().filter(|condition| condition.status == Status::Unknown).map(|condition| condition.statement.clone()).collect();
    let mut verdict = verdict_of(&conditions);
    // What a partial source left out can be what would have met a condition,
    // so it cannot rule anything out. What it did report still stands.
    let incomplete = incomplete_sources(request, &conditions, &hops, &records);
    for (source, coverage) in &incomplete {
        unknowns.push(format!("Source {source}'s last sync ({}) was {}, so records may be missing or out of date.", coverage.run_id, coverage.status));
    }
    if !incomplete.is_empty() && verdict == NOT_INDICATED {
        verdict = INSUFFICIENT_DATA;
    }
    let id_text = [Some(rule_id.to_string()), Some(instance.reference.clone()), subject].into_iter().flatten().collect::<Vec<_>>().join("\n");
    let assumptions = rules().into_iter().find(|rule| rule.rule_id == rule_id).map(|rule| rule.assumptions).unwrap_or_default();
    Built {
        record: FindingRecord {
            finding_id: format!("finding:{:016x}", fnv1a(&id_text)),
            rule_id: rule_id.into(),
            verdict: verdict.into(),
            resource: instance.reference.clone(),
            conditions,
            unknowns,
            assumptions,
            path: (!hops.is_empty()).then(|| PathRef { derivation: DERIVATION.into(), hops: hops.clone() }),
            depends_on: None,
            records,
        },
        hops,
    }
}

/// Evaluates every rule. The result depends on nothing but the records, in
/// whatever order they came.
pub fn evaluate(request: &AnalyzeRequest) -> (Vec<FindingRecord>, u64) {
    let index = Index::new(request);
    let instances = index.with_label("Instance");
    let mut findings = Vec::new();
    for instance in &instances {
        findings.push(direct(request, &index, instance));
        for (balancer, group, to_target, forwards) in index.balancers_of(instance) {
            findings.push(behind_balancer(request, &index, instance, balancer, group, to_target, forwards));
        }
    }
    let lateral = lateral_findings(request, &index, &findings);
    findings.extend(lateral);
    let mut records: Vec<FindingRecord> = findings.into_iter().map(|built| built.record).collect();
    records.sort_by(|a, b| (rank(&a.verdict), &a.rule_id, &a.resource, &a.finding_id).cmp(&(rank(&b.verdict), &b.rule_id, &b.resource, &b.finding_id)));
    (records, instances.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};

    fn props(extra: Value) -> Map<String, Value> {
        extra.as_object().unwrap().clone()
    }

    /// Builds the records the host would send, by stable name.
    #[derive(Default, Clone)]
    struct Builder {
        request: AnalyzeRequest,
    }

    impl Builder {
        fn node(mut self, id: &str, label: &str, extra: Value) -> Self {
            let name = extra.get("name").and_then(Value::as_str).unwrap_or(id).to_string();
            self.request.nodes.push(AnalysisNode { reference: format!("node:{id}"), labels: vec![label.into()], name, source: None, properties: props(extra) });
            self
        }

        fn edge(mut self, from: &str, rel_type: &str, to: &str, extra: Value) -> Self {
            for end in [from, to] {
                assert!(self.request.nodes.iter().any(|node| node.reference == format!("node:{end}")), "no node {end}");
            }
            let reference = format!("rel:{from}|{rel_type}|{to}|{extra}");
            self.request.edges.push(AnalysisEdge { reference, from: format!("node:{from}"), to: format!("node:{to}"), rel_type: rel_type.into(), source: None, properties: props(extra) });
            self
        }

        fn report(&self) -> Vec<FindingRecord> {
            evaluate(&self.request).0
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
        builder.request.edges.retain(|edge| edge.rel_type != rel_type);
        builder
    }

    fn set(mut builder: Builder, id: &str, key: &str, value: Option<Value>) -> Builder {
        let node = builder.request.nodes.iter_mut().find(|node| node.reference == format!("node:{id}")).unwrap();
        match value {
            Some(value) => node.properties.insert(key.into(), value),
            None => node.properties.remove(key),
        };
        builder
    }

    fn only<'a>(report: &'a [FindingRecord], rule: &str, resource: &str) -> &'a FindingRecord {
        let mut found = report.iter().filter(|finding| finding.rule_id == rule && finding.resource == format!("node:{resource}"));
        let first = found.next().unwrap_or_else(|| panic!("no {rule} finding on {resource}: {report:#?}"));
        assert!(found.next().is_none());
        first
    }

    fn found_condition<'a>(finding: &'a FindingRecord, id: &str) -> &'a ConditionRecord {
        finding.conditions.iter().find(|condition| condition.id == id).unwrap_or_else(|| panic!("no condition {id}: {finding:#?}"))
    }

    fn status(finding: &FindingRecord, id: &str) -> Status {
        found_condition(finding, id).status
    }

    fn hops(finding: &FindingRecord) -> Vec<(&str, bool)> {
        finding.path.as_ref().unwrap().hops.iter().map(|hop| (hop.edge.split('|').nth(1).unwrap(), hop.along)).collect()
    }

    #[test]
    fn every_direct_condition_met_is_a_potential_exposure_with_its_path() {
        let report = exposed().report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(finding.verdict, POTENTIAL_EXPOSURE);
        assert!(finding.unknowns.is_empty());
        assert_eq!(hops(finding), [("ALLOWS", true), ("PROTECTED_BY", false)]);
        assert!(finding.records.contains(&"node:rtb".to_string()), "the route is part of the case: {:?}", finding.records);
        assert!(!finding.assumptions.is_empty());
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
            assert_eq!(finding.verdict, NOT_INDICATED, "{case}");
        }
    }

    #[test]
    fn no_verdict_ever_says_reachable() {
        for verdict in info().verdicts {
            assert!(!verdict.id.contains("reachable") && !verdict.title.to_lowercase().contains("reachable"));
        }
        assert!(info().claim.contains("does not show that anything is reachable"));
    }

    #[test]
    fn what_was_not_read_is_unknown_not_absent() {
        // No group edges: every instance has a group, so they were not read.
        let report = without_edge(exposed(), "PROTECTED_BY").report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(finding.verdict, INSUFFICIENT_DATA);
        assert_eq!(status(finding, "open-inbound-rule"), Status::Unknown);
        assert_eq!(finding.unknowns.len(), 1);
        assert!(finding.path.is_none());

        // No route tables recorded at all.
        let report = without_edge(without_edge(exposed(), "USES_ROUTE_TABLE"), "ROUTES_VIA").report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(status(finding, "internet-route"), Status::Unknown);
        assert_eq!(finding.verdict, INSUFFICIENT_DATA);

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
        assert_eq!(finding.verdict, NOT_INDICATED);
        assert_eq!(finding.unknowns.len(), 1, "the unknown is still said");
    }

    #[test]
    fn a_subnet_without_its_own_table_uses_the_main_one() {
        let main = without_edge(exposed(), "USES_ROUTE_TABLE").edge("rtb", "IN_VPC", "vpc", json!({}));
        let report = set(main, "rtb", "is_main", Some(json!(true))).report();
        let finding = only(&report, RULE_DIRECT, "web");
        assert_eq!(status(finding, "internet-route"), Status::Met);
        assert_eq!(finding.verdict, POTENTIAL_EXPOSURE);

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
        assert_eq!(only(&report, RULE_DIRECT, "web").verdict, POTENTIAL_EXPOSURE);
    }

    #[test]
    fn a_narrower_cidr_is_not_the_internet() {
        let builder = set(exposed(), "internet", "cidr", Some(json!("203.0.113.0/24")));
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
            .node("sg-other", "SecurityGroup", json!({"name": "sg-other"}))
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

    const ALLOWS_TARGET: &str = "target-allows-balancer";

    #[test]
    fn a_target_behind_a_public_balancer_needs_its_own_group_to_allow_the_port() {
        let report = balanced(app_rule("8080"), "sg-lb", "healthy").report();
        let finding = only(&report, RULE_BALANCER, "app");
        assert_eq!(finding.verdict, POTENTIAL_EXPOSURE, "{finding:#?}");
        assert_eq!(hops(finding), [("ROUTES_TO", true), ("FORWARDS_TO", true), ("ROUTES_TO", true)]);
        assert_eq!(only(&report, RULE_DIRECT, "app").verdict, NOT_INDICATED, "it has no public address of its own");

        // The port range matters, and so does whom the rule lets in.
        let status_for = |builder: Builder| status(only(&builder.report(), RULE_BALANCER, "app"), ALLOWS_TARGET);
        assert_eq!(status_for(balanced(app_rule("8000-8100"), "sg-lb", "healthy")), Status::Met);
        assert_eq!(status_for(balanced(app_rule("all"), "sg-lb", "healthy")), Status::Met);
        assert_eq!(status_for(balanced(app_rule("9000"), "sg-lb", "healthy")), Status::NotMet);
        assert_eq!(status_for(balanced(app_rule("8080"), "office", "healthy")), Status::Unknown, "a CIDR may or may not hold the balancer's addresses");
        assert_eq!(status_for(balanced(app_rule("8080"), "internet", "healthy")), Status::Met);
    }

    /// With no group recorded for the balancer, a rule from some other group
    /// says nothing either way. This was once read as "does not allow it", which
    /// cleared the finding on data that was simply missing.
    #[test]
    fn missing_balancer_groups_leave_the_target_rule_unknown_rather_than_unmet() {
        let no_balancer_groups = |builder: Builder| {
            let mut builder = builder;
            builder.request.edges.retain(|edge| !(edge.rel_type == "PROTECTED_BY" && edge.from == "node:lb"));
            builder
        };
        // A rule from another group, and the balancer's own groups not known.
        let report = no_balancer_groups(balanced(app_rule("8080"), "sg-other", "healthy")).report();
        let finding = only(&report, RULE_BALANCER, "app");
        assert_eq!(status(finding, ALLOWS_TARGET), Status::Unknown, "{finding:#?}");
        assert_eq!(finding.verdict, INSUFFICIENT_DATA);
        assert!(finding.unknowns.iter().any(|text| text.contains("allows the target group's port")));

        // No rule covers the port either: still not known to be the balancer's.
        let report = no_balancer_groups(balanced(app_rule("9000"), "sg-other", "healthy")).report();
        assert_eq!(status(only(&report, RULE_BALANCER, "app"), ALLOWS_TARGET), Status::Unknown);

        // With the groups known, the same rule from another group does rule it out.
        let report = balanced(app_rule("8080"), "sg-other", "healthy").report();
        let finding = only(&report, RULE_BALANCER, "app");
        assert_eq!(status(finding, ALLOWS_TARGET), Status::NotMet);
        assert_eq!(finding.verdict, NOT_INDICATED);

        // An open rule needs no knowledge of the balancer's groups.
        let report = no_balancer_groups(balanced(app_rule("8080"), "internet", "healthy")).report();
        assert_eq!(status(only(&report, RULE_BALANCER, "app"), ALLOWS_TARGET), Status::Met);
    }

    /// A `TCP_UDP` target group carries both, so a UDP rule alone lets traffic in.
    #[test]
    fn udp_through_a_tcp_udp_target_group_counts() {
        let with_protocol = |group: &str, rule: Value| set(balanced(rule, "sg-lb", "healthy"), "tg", "protocol", Some(json!(group)));
        let udp_rule = || json!({"protocol": "udp", "ports": "8080", "rule": "b"});
        let tcp_rule = || json!({"protocol": "tcp", "ports": "8080", "rule": "b"});
        let status_for = |builder: Builder| status(only(&builder.report(), RULE_BALANCER, "app"), ALLOWS_TARGET);

        for group in ["TCP_UDP", "TCP_QUIC"] {
            assert_eq!(status_for(with_protocol(group, udp_rule())), Status::Met, "{group} over udp");
            assert_eq!(status_for(with_protocol(group, tcp_rule())), Status::Met, "{group} over tcp");
        }
        for group in ["UDP", "QUIC", "GENEVE"] {
            assert_eq!(status_for(with_protocol(group, udp_rule())), Status::Met, "{group} over udp");
            assert_eq!(status_for(with_protocol(group, tcp_rule())), Status::NotMet, "{group} is not tcp");
        }
        for group in ["HTTP", "HTTPS", "TCP", "TLS"] {
            assert_eq!(status_for(with_protocol(group, tcp_rule())), Status::Met, "{group} over tcp");
            assert_eq!(status_for(with_protocol(group, udp_rule())), Status::NotMet, "{group} is not udp");
        }

        let report = with_protocol("TCP_UDP", udp_rule()).report();
        assert!(found_condition(only(&report, RULE_BALANCER, "app"), ALLOWS_TARGET).detail.contains("udp 8080"), "it says which");
        let report = with_protocol("TCP_UDP", json!({"protocol": "icmp", "ports": "all", "rule": "b"})).report();
        assert!(found_condition(only(&report, RULE_BALANCER, "app"), ALLOWS_TARGET).detail.contains("tcp or udp 8080"));

        // Not knowing the protocol is not the same as it being TCP.
        let report = set(balanced(app_rule("8080"), "sg-lb", "healthy"), "tg", "protocol", None).report();
        assert_eq!(status(only(&report, RULE_BALANCER, "app"), ALLOWS_TARGET), Status::Unknown);
    }

    /// The target's security groups are what the condition turns on, so they are
    /// part of its evidence whichever way it went.
    #[test]
    fn the_targets_group_membership_is_evidence_for_the_rule() {
        let membership_of = |finding: &FindingRecord| -> Vec<String> {
            found_condition(finding, ALLOWS_TARGET).fields.iter().filter(|field| field.field == "@to").map(|field| field.record.clone()).collect()
        };
        for (case, builder) in [("allowed", balanced(app_rule("8080"), "sg-lb", "healthy")), ("refused", balanced(app_rule("9000"), "sg-lb", "healthy"))] {
            let report = builder.report();
            let found = membership_of(only(&report, RULE_BALANCER, "app"));
            assert_eq!(found.len(), 1, "{case}");
            assert!(found[0].contains("app|PROTECTED_BY|sg-app"), "{case}: {found:?}");
        }
    }

    #[test]
    fn an_internal_balancer_or_an_unhealthy_target_does_not_give_a_potential_exposure() {
        let internal = set(balanced(app_rule("8080"), "sg-lb", "healthy"), "lb", "scheme", Some(json!("internal")));
        assert_eq!(only(&internal.report(), RULE_BALANCER, "app").verdict, NOT_INDICATED);

        let unhealthy = balanced(app_rule("8080"), "sg-lb", "unhealthy").report();
        let finding = only(&unhealthy, RULE_BALANCER, "app");
        assert_eq!(finding.verdict, INSUFFICIENT_DATA);
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
        assert_eq!(db.verdict, POTENTIAL_EXPOSURE);
        assert_eq!(db.depends_on.as_ref(), Some(&web.finding_id));
        assert_eq!(hops(db), [("ALLOWS", true), ("PROTECTED_BY", false), ("PROTECTED_BY", true), ("ALLOWS", true), ("PROTECTED_BY", false)]);
        assert_eq!(only(&report, RULE_DIRECT, "db").verdict, NOT_INDICATED);
    }

    #[test]
    fn a_step_behind_an_undecided_instance_is_undecided_and_behind_a_safe_one_is_silent() {
        let undecided = with_database(without_edge(without_edge(exposed(), "USES_ROUTE_TABLE"), "ROUTES_VIA")).report();
        let db = only(&undecided, RULE_LATERAL, "db");
        assert_eq!(db.verdict, INSUFFICIENT_DATA);
        assert_eq!(status(db, "source-potentially-exposed"), Status::Unknown);

        let not_exposed = with_database(set(exposed(), "web", "public_ip", None)).report();
        assert!(not_exposed.iter().all(|finding| finding.rule_id != RULE_LATERAL), "{not_exposed:#?}");
    }

    #[test]
    fn a_stopped_target_of_a_lateral_rule_is_not_indicated() {
        let builder = set(with_database(exposed()), "db", "state", Some(json!("stopped")));
        assert_eq!(only(&builder.report(), RULE_LATERAL, "db").verdict, NOT_INDICATED);
    }

    #[test]
    fn findings_come_in_a_stated_order_and_input_order_does_not_change_them() {
        let builder = with_database(exposed()).node("idle", "Instance", json!({"name": "idle", "state": "running", "public_ip": "198.51.100.9"}));
        let (report, subjects) = evaluate(&builder.request);
        let order: Vec<(&str, &str, &str)> = report.iter().map(|finding| (finding.verdict.as_str(), finding.rule_id.as_str(), finding.resource.as_str())).collect();
        assert_eq!(
            order,
            [
                (POTENTIAL_EXPOSURE, RULE_DIRECT, "node:web"),
                (POTENTIAL_EXPOSURE, RULE_LATERAL, "node:db"),
                (INSUFFICIENT_DATA, RULE_DIRECT, "node:idle"),
                (NOT_INDICATED, RULE_DIRECT, "node:db"),
            ],
            "{report:#?}"
        );
        assert_eq!(subjects, 3);

        let mut shuffled = builder.request.clone();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        assert_eq!(evaluate(&shuffled).0, report);
    }

    #[test]
    fn the_same_records_keep_their_finding_id() {
        let report = exposed().report();
        assert_eq!(report[0].finding_id, exposed().report()[0].finding_id);
        assert_ne!(report[0].finding_id, only(&with_database(exposed()).report(), RULE_LATERAL, "db").finding_id);
        // The ID format, so that changing it shows up here.
        let id_text = format!("{RULE_DIRECT}\nnode:web");
        assert_eq!(only(&report, RULE_DIRECT, "web").finding_id, format!("finding:{:016x}", fnv1a(&id_text)));
    }

    fn tagged(mut builder: Builder, status: &str) -> Builder {
        for node in &mut builder.request.nodes {
            node.source = Some("shop".into());
        }
        for edge in &mut builder.request.edges {
            edge.source = Some("shop".into());
        }
        builder.request.coverage.insert("shop".into(), SourceCoverage { run_id: "shop#1".into(), status: status.into(), gaps: vec![] });
        builder
    }

    #[test]
    fn a_partial_source_cannot_clear_anything() {
        // No public address, so the rule would not point here on complete data.
        let bare = || set(exposed(), "web", "public_ip", None);
        let whole = tagged(bare(), "complete").report();
        assert_eq!(only(&whole, RULE_DIRECT, "web").verdict, NOT_INDICATED);

        for status in ["partial", "failed", "cancelled"] {
            let report = tagged(bare(), status).report();
            let finding = only(&report, RULE_DIRECT, "web");
            assert_eq!(finding.verdict, INSUFFICIENT_DATA, "{status}");
            assert!(finding.unknowns.iter().any(|text| text.contains("shop#1") && text.contains(status)), "{:?}", finding.unknowns);
        }
    }

    #[test]
    fn what_a_partial_source_did_report_still_counts() {
        let report = tagged(exposed(), "partial").report();
        assert_eq!(only(&report, RULE_DIRECT, "web").verdict, POTENTIAL_EXPOSURE);
    }

    #[test]
    fn a_source_with_no_record_of_its_runs_is_not_called_incomplete() {
        let mut builder = tagged(set(exposed(), "web", "public_ip", None), "complete");
        builder.request.coverage.clear();
        assert_eq!(only(&builder.report(), RULE_DIRECT, "web").verdict, NOT_INDICATED);
    }

    #[test]
    fn a_partial_source_behind_a_path_counts_even_when_no_condition_read_it() {
        // Only the node a hop arrives at comes from the partial source.
        let mut builder = set(exposed(), "web", "public_ip", None);
        let internet = builder.request.nodes.iter_mut().find(|node| node.reference == "node:internet").unwrap();
        internet.source = Some("other".into());
        builder.request.coverage.insert("other".into(), SourceCoverage { run_id: "other#2".into(), status: "partial".into(), gaps: vec![] });
        let report = builder.report();
        assert_eq!(only(&report, RULE_DIRECT, "web").verdict, INSUFFICIENT_DATA);
    }

    #[test]
    fn every_rule_and_verdict_a_finding_uses_is_declared_and_translated() {
        let declared = info();
        let builders = [with_database(exposed()), balanced(app_rule("8080"), "sg-lb", "healthy")];
        let mut rules_seen = BTreeSet::new();
        for finding in builders.iter().flat_map(Builder::report) {
            rules_seen.insert(finding.rule_id.clone());
            assert!(declared.rules.iter().any(|rule| rule.rule_id == finding.rule_id));
            assert!(declared.verdicts.iter().any(|verdict| verdict.id == finding.verdict));
            assert!(declared.translated("zh", &format!("rule.{}", finding.rule_id)).is_some(), "{}", finding.rule_id);
            assert!(declared.translated("zh", &format!("verdict.{}", finding.verdict)).is_some(), "{}", finding.verdict);
            for condition in &finding.conditions {
                assert!(declared.translated("zh", &format!("condition.{}", condition.id)).is_some(), "{}", condition.id);
            }
        }
        assert_eq!(rules_seen.len(), 3, "the builders reach every rule");
    }
}

//! Official AWS data source: an account's network and compute topology, read
//! through the `aws` CLI with read-only calls, as a graph.
//!
//! ```text
//! (Region)<-[IN_REGION]-(Vpc)<-[IN_VPC]-(Subnet)<-[IN_SUBNET]-(Instance)
//! (Instance)-[PROTECTED_BY]->(SecurityGroup)
//! (Cidr | SecurityGroup)-[ALLOWS {protocol, ports}]->(SecurityGroup)
//! (InternetGateway)-[ATTACHED_TO]->(Vpc)
//! (Instance)-[ASSUMES]->(Role)          through its instance profile
//! (LoadBalancer)-[IN_VPC]->(Vpc), (LoadBalancer)-[PROTECTED_BY]->(SecurityGroup)
//! (LoadBalancer)-[FORWARDS_TO]->(TargetGroup)-[ROUTES_TO]->(Instance)
//! ```
//!
//! `0.0.0.0/0` and `::/0` are one `Cidr` node named Internet, so "what can the
//! internet reach" is a path query. Stable IDs are ARNs where AWS has them,
//! `<account>:<region>:<resource id>` otherwise.
//!
//! Credentials: a named `profile`, or an access key kept in the system
//! keychain and passed only in this process's environment. `fixture_dir`
//! reads saved `aws ... --output json` responses instead of calling AWS, for
//! offline imports and tests: `<dir>/<region>/<service>-<operation>.json`,
//! and `<dir>/iam-list-roles.json`, `<dir>/iam-list-instance-profiles.json`,
//! `<dir>/sts-get-caller-identity.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use khonsu_plugin_protocol::{
    read_request, ConfigField, Describe, EdgeRecord, Emitter, FieldKind, Message, NodeRecord, Permissions, Request,
};
use serde_json::{json, Map, Value};

const INTERNET: &str = "cidr:internet";

fn describe() -> Describe {
    let field = |key: &str, label: &str, kind: FieldKind, secret: bool, help: &str| ConfigField {
        key: key.into(),
        label: label.into(),
        kind,
        required: false,
        secret,
        help: help.into(),
        default: None,
    };
    Describe {
        name: "AWS".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "VPCs, subnets, security groups, EC2 instances, load balancers and IAM roles, read with the aws CLI. Read-only credentials are enough.".into(),
        config: vec![
            ConfigField {
                required: true,
                default: Some(json!("us-east-1")),
                ..field("regions", "Regions", FieldKind::String, false, "Comma-separated, e.g. us-east-1,eu-west-1")
            },
            field("profile", "Profile", FieldKind::String, false, "A profile from ~/.aws/config; leave empty to use the access key below"),
            field("access_key_id", "Access key ID", FieldKind::String, true, "Read-only key; stored in the system keychain"),
            field("secret_access_key", "Secret access key", FieldKind::String, true, "Stored in the system keychain"),
            field("session_token", "Session token", FieldKind::String, true, "For temporary credentials"),
            field("fixture_dir", "Saved responses", FieldKind::File, false, "Read saved aws --output json responses from this folder instead of calling AWS"),
        ],
        permissions: Permissions {
            network: vec!["*.amazonaws.com".into()],
            read_files: false,
            commands: vec!["aws".into()],
            notes: vec!["Read-only: ec2:Describe*, elasticloadbalancing:Describe*, iam:List*, sts:GetCallerIdentity".into()],
        },
        labels: ["Account", "Region", "Vpc", "Subnet", "SecurityGroup", "Cidr", "Instance", "InternetGateway", "LoadBalancer", "TargetGroup", "Role"]
            .map(String::from)
            .to_vec(),
        relationship_types: ["IN_REGION", "IN_VPC", "IN_SUBNET", "PROTECTED_BY", "ALLOWS", "ATTACHED_TO", "ASSUMES", "FORWARDS_TO", "ROUTES_TO", "OWNS"]
            .map(String::from)
            .to_vec(),
        supports_incremental: false,
    }
}

/// Where responses come from: the aws CLI, or saved files.
enum Source {
    Cli {
        profile: Option<String>,
        env: Vec<(String, String)>,
    },
    Fixture(PathBuf),
}

impl Source {
    fn from_request(request: &Request) -> Result<Self, String> {
        let text = |key: &str, map: &Map<String, Value>| {
            map.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
        };
        if let Some(dir) = text("fixture_dir", &request.config) {
            let dir = PathBuf::from(dir);
            // Missing files read as empty responses, so a wrong folder would
            // otherwise sync an empty account without complaint.
            if !dir.is_dir() {
                return Err(format!("saved responses folder not found: {}", dir.display()));
            }
            return Ok(Source::Fixture(dir));
        }
        let mut env = Vec::new();
        for (key, var) in [
            ("access_key_id", "AWS_ACCESS_KEY_ID"),
            ("secret_access_key", "AWS_SECRET_ACCESS_KEY"),
            ("session_token", "AWS_SESSION_TOKEN"),
        ] {
            if let Some(value) = text(key, &request.secrets) {
                env.push((var.to_string(), value));
            }
        }
        let profile = text("profile", &request.config);
        if profile.is_none() && !env.iter().any(|(k, _)| k == "AWS_ACCESS_KEY_ID") {
            return Err("set a profile, or an access key ID and secret access key".into());
        }
        Ok(Source::Cli { profile, env })
    }

    /// One read-only call, as JSON. `region` is `None` for global services.
    fn call(&self, region: Option<&str>, service: &str, operation: &str, extra: &[&str]) -> Result<Value, String> {
        match self {
            Source::Fixture(dir) => {
                let file = format!("{service}-{operation}.json");
                let path = match region {
                    Some(region) => dir.join(region).join(file),
                    None => dir.join(file),
                };
                match std::fs::read_to_string(&path) {
                    Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
                    // A response not saved is an empty one: the fixture did not use that service.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
                    Err(e) => Err(format!("{}: {e}", path.display())),
                }
            }
            Source::Cli { profile, env } => {
                let mut command = Command::new("aws");
                command.arg(service).arg(operation).args(extra).args(["--output", "json", "--no-cli-pager"]);
                if let Some(region) = region {
                    command.args(["--region", region]);
                }
                if let Some(profile) = profile {
                    command.args(["--profile", profile]);
                }
                for (key, value) in env {
                    command.env(key, value);
                }
                let output = command.output().map_err(|e| format!("cannot run the aws CLI: {e}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "aws {service} {operation}{}: {}",
                        region.map(|r| format!(" ({r})")).unwrap_or_default(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                let text = String::from_utf8_lossy(&output.stdout);
                if text.trim().is_empty() {
                    return Ok(json!({}));
                }
                serde_json::from_str(&text).map_err(|e| format!("aws {service} {operation}: {e}"))
            }
        }
    }
}

fn regions(request: &Request) -> Vec<String> {
    request
        .config
        .get("regions")
        .and_then(Value::as_str)
        .unwrap_or("us-east-1")
        .split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn s<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The `Name` tag, which is what people call a resource.
fn name_tag(value: &Value) -> Option<String> {
    items(value, "Tags").find(|t| s(t, "Key") == Some("Name")).and_then(|t| s(t, "Value")).map(str::to_string)
}

fn props(pairs: &[(&str, Option<Value>)]) -> Map<String, Value> {
    pairs
        .iter()
        .filter_map(|(k, v)| v.clone().filter(|v| !v.is_null()).map(|v| (k.to_string(), v)))
        .collect()
}

/// Records collected before they are written, so an edge is only written
/// when both of its ends exist.
#[derive(Default)]
struct Graph {
    nodes: BTreeMap<String, NodeRecord>,
    edges: BTreeMap<String, EdgeRecord>,
}

impl Graph {
    fn node(&mut self, id: impl Into<String>, label: &str, properties: Map<String, Value>) -> String {
        let id = id.into();
        self.nodes.entry(id.clone()).or_insert(NodeRecord {
            id: id.clone(),
            label: label.into(),
            properties,
        });
        id
    }

    fn edge(&mut self, source: &str, rel_type: &str, target: &str, properties: Map<String, Value>) {
        let id = format!("{source}|{rel_type}|{target}");
        let key = match properties.get("rule") {
            Some(rule) => format!("{id}|{rule}"),
            None => id,
        };
        self.edges.entry(key.clone()).or_insert(EdgeRecord {
            id: key,
            source: source.into(),
            target: target.into(),
            rel_type: rel_type.into(),
            properties,
        });
    }

    fn emit<W: Write>(self, out: &mut Emitter<W>) -> std::io::Result<(u64, u64)> {
        let mut nodes = 0;
        for node in self.nodes.values() {
            out.node(node.clone())?;
            nodes += 1;
        }
        let mut edges = 0;
        for edge in self.edges.into_values() {
            if self.nodes.contains_key(&edge.source) && self.nodes.contains_key(&edge.target) {
                out.edge(edge)?;
                edges += 1;
            }
        }
        Ok((nodes, edges))
    }
}

fn port_range(permission: &Value) -> String {
    let protocol = s(permission, "IpProtocol").unwrap_or("-1");
    if protocol == "-1" {
        return "all".into();
    }
    match (permission.get("FromPort").and_then(Value::as_i64), permission.get("ToPort").and_then(Value::as_i64)) {
        (Some(a), Some(b)) if a == b => a.to_string(),
        (Some(a), Some(b)) => format!("{a}-{b}"),
        _ => "all".into(),
    }
}

fn collect<W: Write>(source: &Source, regions: &[String], out: &mut Emitter<W>) -> Result<Graph, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut g = Graph::default();
    let identity = source.call(None, "sts", "get-caller-identity", &[])?;
    let account = s(&identity, "Account").unwrap_or("unknown").to_string();
    let account_id = g.node(format!("account:{account}"), "Account", props(&[("account_id", Some(json!(account)))]));
    g.node(INTERNET, "Cidr", props(&[("name", Some(json!("Internet"))), ("cidr", Some(json!("0.0.0.0/0")))]));

    // IAM is global: roles, and which instance profile carries which role.
    out.progress("IAM roles", None, None).map_err(io)?;
    let roles = source.call(None, "iam", "list-roles", &[])?;
    for role in items(&roles, "Roles") {
        let Some(arn) = s(role, "Arn") else { continue };
        let trust = role.get("AssumeRolePolicyDocument").map(|d| d.to_string());
        let id = g.node(arn, "Role", props(&[("name", s(role, "RoleName").map(|v| json!(v))), ("arn", Some(json!(arn))), ("path", s(role, "Path").map(|v| json!(v))), ("trust_policy", trust.map(|t| json!(t)))]));
        g.edge(&account_id, "OWNS", &id, Map::new());
    }
    let profiles = source.call(None, "iam", "list-instance-profiles", &[])?;
    let mut profile_roles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for profile in items(&profiles, "InstanceProfiles") {
        let Some(arn) = s(profile, "Arn") else { continue };
        profile_roles.insert(arn.to_string(), items(profile, "Roles").filter_map(|r| s(r, "Arn")).map(str::to_string).collect());
    }

    for (ix, region) in regions.iter().enumerate() {
        out.progress(format!("region {region}"), Some(ix as u64), Some(regions.len() as u64)).map_err(io)?;
        let scoped = |id: &str| format!("{account}:{region}:{id}");
        let region_id = g.node(format!("region:{account}:{region}"), "Region", props(&[("name", Some(json!(region)))]));
        g.edge(&account_id, "OWNS", &region_id, Map::new());

        for vpc in items(&source.call(Some(region), "ec2", "describe-vpcs", &[])?, "Vpcs") {
            let Some(vpc_id) = s(vpc, "VpcId") else { continue };
            let id = g.node(scoped(vpc_id), "Vpc", props(&[("name", Some(json!(name_tag(vpc).unwrap_or_else(|| vpc_id.to_string())))), ("vpc_id", Some(json!(vpc_id))), ("cidr", s(vpc, "CidrBlock").map(|v| json!(v))), ("is_default", vpc.get("IsDefault").cloned())]));
            g.edge(&id, "IN_REGION", &region_id, Map::new());
        }
        for subnet in items(&source.call(Some(region), "ec2", "describe-subnets", &[])?, "Subnets") {
            let Some(subnet_id) = s(subnet, "SubnetId") else { continue };
            let id = g.node(
                s(subnet, "SubnetArn").map(str::to_string).unwrap_or_else(|| scoped(subnet_id)),
                "Subnet",
                props(&[("name", Some(json!(name_tag(subnet).unwrap_or_else(|| subnet_id.to_string())))), ("subnet_id", Some(json!(subnet_id))), ("cidr", s(subnet, "CidrBlock").map(|v| json!(v))), ("availability_zone", s(subnet, "AvailabilityZone").map(|v| json!(v))), ("public_ip_on_launch", subnet.get("MapPublicIpOnLaunch").cloned())]),
            );
            if let Some(vpc_id) = s(subnet, "VpcId") {
                g.edge(&id, "IN_VPC", &scoped(vpc_id), Map::new());
            }
        }
        for igw in items(&source.call(Some(region), "ec2", "describe-internet-gateways", &[])?, "InternetGateways") {
            let Some(igw_id) = s(igw, "InternetGatewayId") else { continue };
            let id = g.node(scoped(igw_id), "InternetGateway", props(&[("name", Some(json!(name_tag(igw).unwrap_or_else(|| igw_id.to_string()))))]));
            for attachment in items(igw, "Attachments") {
                if let Some(vpc_id) = s(attachment, "VpcId") {
                    g.edge(&id, "ATTACHED_TO", &scoped(vpc_id), Map::new());
                    g.edge(INTERNET, "ROUTES_TO", &id, Map::new());
                }
            }
        }
        let groups = source.call(Some(region), "ec2", "describe-security-groups", &[])?;
        for sg in items(&groups, "SecurityGroups") {
            let Some(group_id) = s(sg, "GroupId") else { continue };
            let id = g.node(scoped(group_id), "SecurityGroup", props(&[("name", s(sg, "GroupName").map(|v| json!(v))), ("group_id", Some(json!(group_id))), ("description", s(sg, "Description").map(|v| json!(v)))]));
            if let Some(vpc_id) = s(sg, "VpcId") {
                g.edge(&id, "IN_VPC", &scoped(vpc_id), Map::new());
            }
        }
        // Ingress rules after every group exists, so a rule naming another group finds it.
        for sg in items(&groups, "SecurityGroups") {
            let Some(group_id) = s(sg, "GroupId") else { continue };
            let target = scoped(group_id);
            for permission in items(sg, "IpPermissions") {
                let rule = props(&[("protocol", s(permission, "IpProtocol").map(|p| json!(if p == "-1" { "all" } else { p }))), ("ports", Some(json!(port_range(permission))))]);
                let with_rule = |source_id: String, g: &mut Graph| {
                    let mut properties = rule.clone();
                    properties.insert("rule".into(), json!(format!("{}:{}", properties["protocol"], properties["ports"])));
                    g.edge(&source_id, "ALLOWS", &target, properties);
                };
                for range in items(permission, "IpRanges").chain(items(permission, "Ipv6Ranges")) {
                    let cidr = s(range, "CidrIp").or_else(|| s(range, "CidrIpv6")).unwrap_or_default();
                    let source_id = if cidr == "0.0.0.0/0" || cidr == "::/0" {
                        INTERNET.to_string()
                    } else {
                        g.node(format!("cidr:{cidr}"), "Cidr", props(&[("name", Some(json!(cidr))), ("cidr", Some(json!(cidr)))]))
                    };
                    with_rule(source_id, &mut g);
                }
                for pair in items(permission, "UserIdGroupPairs") {
                    if let Some(other) = s(pair, "GroupId") {
                        with_rule(scoped(other), &mut g);
                    }
                }
            }
        }
        let instances = source.call(Some(region), "ec2", "describe-instances", &[])?;
        for reservation in items(&instances, "Reservations") {
            for instance in items(reservation, "Instances") {
                let Some(instance_id) = s(instance, "InstanceId") else { continue };
                let id = g.node(
                    scoped(instance_id),
                    "Instance",
                    props(&[
                        ("name", Some(json!(name_tag(instance).unwrap_or_else(|| instance_id.to_string())))),
                        ("instance_id", Some(json!(instance_id))),
                        ("type", s(instance, "InstanceType").map(|v| json!(v))),
                        ("state", instance.pointer("/State/Name").cloned()),
                        ("private_ip", s(instance, "PrivateIpAddress").map(|v| json!(v))),
                        ("public_ip", s(instance, "PublicIpAddress").map(|v| json!(v))),
                        ("image_id", s(instance, "ImageId").map(|v| json!(v))),
                        ("launched_at", s(instance, "LaunchTime").map(|v| json!(v))),
                    ]),
                );
                if let Some(subnet_id) = s(instance, "SubnetId") {
                    // Subnets are keyed by ARN when AWS gave one; find it by its ID.
                    let subnet = g
                        .nodes
                        .values()
                        .find(|n| n.label == "Subnet" && n.properties.get("subnet_id") == Some(&json!(subnet_id)))
                        .map(|n| n.id.clone())
                        .unwrap_or_else(|| scoped(subnet_id));
                    g.edge(&id, "IN_SUBNET", &subnet, Map::new());
                }
                for group in items(instance, "SecurityGroups") {
                    if let Some(group_id) = s(group, "GroupId") {
                        g.edge(&id, "PROTECTED_BY", &scoped(group_id), Map::new());
                    }
                }
                if let Some(profile) = instance.pointer("/IamInstanceProfile/Arn").and_then(Value::as_str) {
                    for role in profile_roles.get(profile).into_iter().flatten() {
                        g.edge(&id, "ASSUMES", role, props(&[("instance_profile", Some(json!(profile)))]));
                    }
                }
            }
        }
        let balancers = source.call(Some(region), "elbv2", "describe-load-balancers", &[])?;
        for lb in items(&balancers, "LoadBalancers") {
            let Some(arn) = s(lb, "LoadBalancerArn") else { continue };
            let id = g.node(arn, "LoadBalancer", props(&[("name", s(lb, "LoadBalancerName").map(|v| json!(v))), ("dns_name", s(lb, "DNSName").map(|v| json!(v))), ("scheme", s(lb, "Scheme").map(|v| json!(v))), ("type", s(lb, "Type").map(|v| json!(v)))]));
            if let Some(vpc_id) = s(lb, "VpcId") {
                g.edge(&id, "IN_VPC", &scoped(vpc_id), Map::new());
            }
            for group in lb.get("SecurityGroups").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                g.edge(&id, "PROTECTED_BY", &scoped(group), Map::new());
            }
            if s(lb, "Scheme") == Some("internet-facing") {
                g.edge(INTERNET, "ROUTES_TO", &id, Map::new());
            }
        }
        let targets = source.call(Some(region), "elbv2", "describe-target-groups", &[])?;
        for tg in items(&targets, "TargetGroups") {
            let Some(arn) = s(tg, "TargetGroupArn") else { continue };
            let id = g.node(arn, "TargetGroup", props(&[("name", s(tg, "TargetGroupName").map(|v| json!(v))), ("protocol", s(tg, "Protocol").map(|v| json!(v))), ("port", tg.get("Port").cloned())]));
            for lb in tg.get("LoadBalancerArns").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                g.edge(lb, "FORWARDS_TO", &id, Map::new());
            }
            let health = match source {
                // Saved responses keep each group's health under its name.
                Source::Fixture(_) => source.call(Some(region), "elbv2", &format!("describe-target-health-{}", s(tg, "TargetGroupName").unwrap_or_default()), &[])?,
                Source::Cli { .. } => source.call(Some(region), "elbv2", "describe-target-health", &["--target-group-arn", arn])?,
            };
            for description in items(&health, "TargetHealthDescriptions") {
                if let Some(target) = description.pointer("/Target/Id").and_then(Value::as_str) {
                    if target.starts_with("i-") {
                        g.edge(&id, "ROUTES_TO", &scoped(target), props(&[("health", description.pointer("/TargetHealth/State").cloned())]));
                    }
                }
            }
        }
    }
    Ok(g)
}

fn test(source: &Source) -> Result<String, String> {
    let identity = source.call(None, "sts", "get-caller-identity", &[])?;
    Ok(format!(
        "signed in as {} in account {}",
        s(&identity, "Arn").unwrap_or("?"),
        s(&identity, "Account").unwrap_or("?")
    ))
}

fn run<W: Write>(command: &str, out: &mut Emitter<W>, request: Option<Request>) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    match command {
        "describe" => out.emit(&Message::Describe(describe())).map_err(io),
        "test" => {
            let request = request.ok_or("no request")?;
            let message = Source::from_request(&request).and_then(|source| test(&source));
            let (ok, message) = match message {
                Ok(m) => (true, m),
                Err(m) => (false, m),
            };
            out.emit(&Message::TestResult { ok, message }).map_err(io)
        }
        "sync" => {
            let request = request.ok_or("no request")?;
            let source = Source::from_request(&request)?;
            let regions = regions(&request);
            let graph = collect(&source, &regions, out)?;
            let labels: BTreeSet<&str> = graph.nodes.values().map(|n| n.label.as_str()).collect();
            out.emit(&Message::Log {
                level: "info".into(),
                message: format!("labels: {}", labels.into_iter().collect::<Vec<_>>().join(", ")),
            })
            .map_err(io)?;
            let (nodes, edges) = graph.emit(out).map_err(io)?;
            out.emit(&Message::Done { nodes, edges }).map_err(io)
        }
        other => Err(format!("unknown command {other:?}; expected describe, test or sync")),
    }
}

fn main() -> ExitCode {
    let mut out = Emitter::stdout();
    let command = std::env::args().nth(1).unwrap_or_default();
    let request = if command == "describe" { None } else { read_request().ok() };
    match run(&command, &mut out, request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = out.emit(&Message::Error { message: message.clone() });
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use khonsu_plugin_protocol::{parse_line, PROTOCOL_VERSION};

    fn fixture_request() -> Request {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
        let mut config = Map::new();
        config.insert("fixture_dir".into(), json!(dir.display().to_string()));
        config.insert("regions".into(), json!("us-east-1"));
        Request {
            protocol: PROTOCOL_VERSION,
            config,
            ..Request::default()
        }
    }

    fn sync() -> (Vec<NodeRecord>, Vec<EdgeRecord>) {
        let mut out = Emitter::new(Vec::new());
        run("sync", &mut out, Some(fixture_request())).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for message in text.lines().filter_map(|l| parse_line(l).map(|m| m.unwrap())) {
            match message {
                Message::Node(n) => nodes.push(n),
                Message::Edge(e) => edges.push(e),
                _ => {}
            }
        }
        (nodes, edges)
    }

    #[test]
    fn the_internet_reaches_an_instance_through_its_security_group_and_load_balancer() {
        let (nodes, edges) = sync();
        let label = |label: &str| nodes.iter().filter(|n| n.label == label).count();
        assert_eq!(label("Vpc"), 1);
        assert_eq!(label("Subnet"), 2);
        assert_eq!(label("Instance"), 2);
        assert_eq!(label("SecurityGroup"), 2);
        assert_eq!(label("LoadBalancer"), 1);
        let has = |source: &str, rel: &str, target_contains: &str| {
            edges.iter().any(|e| e.source.contains(source) && e.rel_type == rel && e.target.contains(target_contains))
        };
        assert!(has(INTERNET, "ALLOWS", "sg-web"), "{edges:#?}");
        assert!(has("sg-web", "ALLOWS", "sg-db"));
        assert!(has("i-web1", "PROTECTED_BY", "sg-web"));
        assert!(has("i-web1", "ASSUMES", "role/web-app"));
        assert!(has("i-db1", "IN_SUBNET", "subnet-private"));
        assert!(has(INTERNET, "ROUTES_TO", "loadbalancer/app/shop"));
        assert!(has("targetgroup/web", "ROUTES_TO", "i-web1"));
        // Every edge's ends exist.
        let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(edges.iter().all(|e| ids.contains(e.source.as_str()) && ids.contains(e.target.as_str())));
    }

    #[test]
    fn credentials_are_required_without_saved_responses() {
        let request = Request {
            protocol: PROTOCOL_VERSION,
            ..Request::default()
        };
        assert!(Source::from_request(&request).is_err());
    }
}

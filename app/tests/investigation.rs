//! The offline investigation gate: the AWS example, from saved responses to
//! the answers an investigator reads, compared by stable IDs and rel types.
//!
//! `examples/aws/golden.json` holds the expected answers, and
//! `tests/fixtures/graph-hawdb-*.zip` holds graphs written by earlier HawDB
//! versions. After a deliberate change, regenerate both and review the diff:
//!
//! ```sh
//! KHONSU_BLESS=1 cargo test --locked --workspace --test investigation
//! ```
//!
//! No AWS access is needed. See `docs/investigation-gates.md`.

use khonsu_core::plugin::{self, PluginError};
use khonsu_core::protocol::SyncMode;
use khonsu_core::{settings, AccessMode, Graph};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

const INTERNET: &str = "shop/cidr:internet";
const WEB_1: &str = "shop/123456789012:us-east-1:i-web1";
const DB_1: &str = "shop/123456789012:us-east-1:i-db1";
const ALLOWS_443: &str = r#"shop/cidr:internet|ALLOWS|123456789012:us-east-1:sg-web|"\"tcp\":\"443\"""#;

/// Questions an investigator asks of the fixture. The name says what each
/// result means; several are meant to find nothing.
const QUERIES: &[(&str, &str)] = &[
    (
        "topology",
        "MATCH (a)-[r]->(b) RETURN a.khonsu_id AS source, type(r) AS type, b.khonsu_id AS target ORDER BY source, type, target",
    ),
    (
        "rules_into_security_groups",
        "MATCH (c:Cidr)-[a:ALLOWS]->(g:SecurityGroup) RETURN c.khonsu_id AS source, a.protocol AS protocol, a.ports AS ports, g.khonsu_id AS target ORDER BY source, ports",
    ),
    (
        "candidate_path_through_the_load_balancer",
        "MATCH (c:Cidr {khonsu_id: $internet})-[:ROUTES_TO]->(l:LoadBalancer)-[:FORWARDS_TO]->(t:TargetGroup)-[:ROUTES_TO]->(i:Instance) RETURN c.khonsu_id AS source, l.khonsu_id AS balancer, t.khonsu_id AS target_group, i.khonsu_id AS instance",
    ),
    (
        "candidate_path_through_security_groups",
        "MATCH (c:Cidr {khonsu_id: $internet})-[:ALLOWS]->(g:SecurityGroup)-[:ALLOWS]->(h:SecurityGroup)<-[:PROTECTED_BY]-(i:Instance) RETURN c.khonsu_id AS source, g.khonsu_id AS first, h.khonsu_id AS second, i.khonsu_id AS instance",
    ),
    (
        "shortest_path_forward",
        "MATCH p = shortestPath((a:Cidr {khonsu_id: $internet})-[*..3]->(b:Instance {khonsu_id: $web})) RETURN [n IN nodes(p) | n.khonsu_id] AS hops, [r IN relationships(p) | r.type] AS types",
    ),
    (
        "shortest_path_against_the_arrows_finds_nothing",
        "MATCH p = shortestPath((a:Instance {khonsu_id: $web})-[*..3]->(b:Cidr {khonsu_id: $internet})) RETURN length(p) AS hops",
    ),
    (
        "shortest_path_inbound_reads_the_same_path",
        "MATCH p = shortestPath((a:Instance {khonsu_id: $web})<-[*..3]-(b:Cidr {khonsu_id: $internet})) RETURN length(p) AS hops",
    ),
    (
        "shortest_path_under_the_hop_cap_finds_nothing",
        "MATCH p = shortestPath((a:Cidr {khonsu_id: $internet})-[*..2]->(b:Instance {khonsu_id: $web})) RETURN length(p) AS hops",
    ),
    (
        "shortest_path_of_one_type_finds_nothing",
        "MATCH p = shortestPath((a:Cidr {khonsu_id: $internet})-[:ROUTES_TO*..3]->(b:Instance {khonsu_id: $web})) RETURN length(p) AS hops",
    ),
    (
        "no_directed_path_to_the_database",
        "MATCH p = shortestPath((a:Cidr {khonsu_id: $internet})-[*..6]->(b:Instance {khonsu_id: $db})) RETURN length(p) AS hops",
    ),
];

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        build_plugins();
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        Self {
            dir: tempfile::tempdir_in(base).unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn config(&self) -> PathBuf {
        self.path("config")
    }

    /// Runs `khonsu` and returns (exit code, stdout JSON).
    fn khonsu(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_khonsugraph"))
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.config())
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.code().unwrap_or(-1), json)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, out) = self.khonsu(args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }

    /// A new graph with a source named `shop` reading `fixture`.
    fn add_source(&self, db: &Path, fixture: &Path) {
        self.ok(&["init", text(db)]);
        self.ok(&[
            "source", "add", "-d", text(db), "--plugin", "aws", "--name", "shop",
            "-c", &format!("fixture_dir={}", serde_json::to_string(text(fixture)).unwrap()),
            "-c", "regions=us-east-1",
        ]);
    }

    fn synced_graph(&self, name: &str) -> PathBuf {
        let db = self.path(name);
        self.add_source(&db, &fixture());
        self.ok(&["plugin", "sync", "--source", "shop"]);
        db
    }

    fn query(&self, db: &Path, cypher: &str) -> Value {
        let mut args = vec!["query", "-d", text(db), "--cypher", cypher];
        // Pass only the parameters this query names.
        let params = [("internet", INTERNET), ("web", WEB_1), ("db", DB_1)].map(|(k, v)| (k, format!("{k}={v}")));
        for (name, param) in &params {
            if cypher.contains(&format!("${name}")) {
                args.extend(["-p", param.as_str()]);
            }
        }
        self.ok(&args)
    }

    fn answers(&self, db: &Path) -> Value {
        let answers: Map<String, Value> = QUERIES
            .iter()
            .map(|(name, cypher)| {
                let out = self.query(db, cypher);
                ((*name).to_string(), json!({"columns": out["columns"], "rows": out["rows"]}))
            })
            .collect();
        Value::Object(answers)
    }

    fn count(&self, db: &Path, cypher: &str) -> i64 {
        self.query(db, cypher)["rows"][0][0].as_i64().unwrap()
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn fixture() -> PathBuf {
    repo().join("plugins/aws/tests/fixture")
}

/// The saved responses with a positive, a negative and an undecided case, and
/// route tables. See `docs/exposure-rules.md`.
fn exposure_fixture() -> PathBuf {
    repo().join("plugins/aws/tests/fixture-exposure")
}

fn golden_path() -> PathBuf {
    repo().join("examples/aws/golden.json")
}

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn blessing() -> bool {
    std::env::var_os("KHONSU_BLESS").is_some()
}

/// `cargo test` builds a plugin only as a test harness; the CLI looks for the
/// real executable beside itself in target/<profile>.
fn build_plugins() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "--locked", "-p", "khonsu-plugin-aws", "-p", "khonsu-plugin-csv"])
            .status()
            .unwrap();
        assert!(status.success());
    });
}

/// Drops what legitimately differs between runs and releases: timings, sync
/// times, paths and plugin versions. Floats are rounded so the last bits of a platform's
/// arithmetic do not count as a change of meaning.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !matches!(key.as_str(), "elapsed_ms" | "load_ms" | "khonsu_synced_at" | "khonsu_plugin" | "plugin" | "file" | "line" | "record_written_at" | "source_last_successful_sync"))
                .map(|(key, value)| (key.clone(), normalize(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Number(number) if number.is_f64() => {
            let rounded = (number.as_f64().unwrap() * 1e9).round() / 1e9;
            json!(rounded)
        }
        other => other.clone(),
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn assert_same(section: &str, actual: &Value, expected: &Value) {
    assert_eq!(
        actual, expected,
        "\n{section} differs from the golden file. If the change is meant, run with KHONSU_BLESS=1 and review the diff.\nactual:\n{}\n",
        serde_json::to_string_pretty(actual).unwrap()
    );
}

fn golden() -> Value {
    let text = std::fs::read_to_string(golden_path()).expect("examples/aws/golden.json");
    serde_json::from_str(&text).unwrap()
}

fn investigation(env: &Env) -> Value {
    let db = env.path("graph");
    env.add_source(&db, &fixture());
    let first = env.ok(&["plugin", "sync", "--source", "shop"]);
    let second = env.ok(&["plugin", "sync", "--source", "shop"]);

    // The IDs `analyze path` takes are internal, so look them up by stable ID.
    let id = |stable: &str| {
        let out = env.query(&db, &format!("MATCH (n {{khonsu_id: '{stable}'}}) RETURN id(n) AS id"));
        out["rows"][0][0].as_i64().unwrap().to_string()
    };
    let path = env.ok(&["analyze", "path", "-d", text(&db), "--from", &id(INTERNET), "--to", &id(WEB_1)]);
    let dash = repo().join("examples/aws/exposure.dash");
    let exposure_report = {
        let other = Env::new();
        let graph = other.path("graph");
        other.add_source(&graph, &exposure_fixture());
        other.ok(&["plugin", "sync", "--source", "shop"]);
        other.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&graph)])
    };

    normalize(&json!({
        "sync": first,
        "resync": second,
        "schema": env.ok(&["schema", "-d", text(&db)]),
        "answers": env.answers(&db),
        "analyze": {
            "summary": env.ok(&["analyze", "summary", "-d", text(&db)]),
            "degree": env.ok(&["analyze", "degree", "-d", text(&db), "--top", "5"]),
            "pagerank": env.ok(&["analyze", "pagerank", "-d", text(&db), "--top", "5"]),
            "path": path,
        },
        "dashboard": env.ok(&["check", text(&dash), "-d", text(&db)]),
        "evidence": {
            "web_1": env.ok(&["evidence", "-d", text(&db), "--node", WEB_1]),
            "internet_to_web_group": env.ok(&["evidence", "-d", text(&db), "--rel", ALLOWS_443]),
        },
        "explore": {
            "exposure_of_web_1": env.ok(&[
                "explore", "-d", text(&db), "--from", INTERNET, "--to", WEB_1,
                "--type", "ROUTES_TO,FORWARDS_TO", "--max-hops", "4",
            ]),
            "what_points_at_web_1": env.ok(&["explore", "-d", text(&db), "--from", WEB_1, "--direction", "in", "--max-hops", "1"]),
        },
        "exposure": {
            // Saved responses with no route tables, as a graph from before they were read.
            "shop_fixture": exposure_brief(&env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)])),
            "exposure_fixture": exposure_report,
        },
    }))
}

/// (rule, resource, verdict) for every finding, which is what changes meaning.
fn exposure_brief(report: &Value) -> Value {
    let findings = report["findings"].as_array().unwrap().iter();
    Value::Array(findings.map(|finding| json!([finding["rule_id"], finding["resource"], finding["verdict"]])).collect())
}

/// Writes a graph as a zip without the lock files of the process that made it.
fn write_snapshot(db: &Path, target: &Path) {
    fn add(zip: &mut zip::ZipWriter<std::fs::File>, root: &Path, dir: &Path) {
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let name = path.strip_prefix(root).unwrap().to_str().unwrap().replace('\\', "/");
            if path.is_dir() {
                zip.add_directory(name, options).unwrap();
                add(zip, root, &path);
            } else if !name.ends_with(".lock") && name != "khonsugraph.owner.json" {
                zip.start_file(name, options).unwrap();
                zip.write_all(&std::fs::read(&path).unwrap()).unwrap();
            }
        }
    }
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(target).unwrap());
    add(&mut zip, db, db);
    zip.finish().unwrap();
}

#[test]
fn the_offline_investigation_gives_the_golden_answers() {
    let env = Env::new();
    let actual = investigation(&env);
    if blessing() {
        let text = serde_json::to_string_pretty(&actual).unwrap() + "\n";
        std::fs::write(golden_path(), text).unwrap();
        let name = format!("graph-hawdb-{}.zip", khonsu_core::HAWDB_VERSION);
        // A snapshot stays what its HawDB version wrote, so only add a missing one.
        let snapshot = snapshots_dir().join(name);
        if !snapshot.exists() {
            write_snapshot(&env.path("graph"), &snapshot);
        }
        return;
    }
    let expected = golden();
    for (section, value) in actual.as_object().unwrap() {
        assert_same(section, value, &expected[section]);
    }
}

/// The answers themselves, spelled out so that blessing a wrong golden file
/// would still have to get past this test.
#[test]
fn candidate_paths_follow_the_arrows_their_hop_cap_and_their_types() {
    let env = Env::new();
    let db = env.synced_graph("graph");
    let rows = |name: &str| {
        let cypher = QUERIES.iter().find(|(query, _)| *query == name).unwrap().1;
        env.query(&db, cypher)["rows"].clone()
    };

    let balancer = "shop/arn:aws:elasticloadbalancing:us-east-1:123456789012:loadbalancer/app/shop/abc";
    let target_group = "shop/arn:aws:elasticloadbalancing:us-east-1:123456789012:targetgroup/web/def";
    assert_eq!(
        rows("shortest_path_forward"),
        json!([[[INTERNET, balancer, target_group, WEB_1], ["ROUTES_TO", "FORWARDS_TO", "ROUTES_TO"]]])
    );
    assert_eq!(rows("candidate_path_through_the_load_balancer"), json!([[INTERNET, balancer, target_group, WEB_1]]));
    assert_eq!(rows("shortest_path_inbound_reads_the_same_path"), json!([[3]]));

    // Zero matches is an empty answer, not an error.
    for name in [
        "shortest_path_against_the_arrows_finds_nothing",
        "shortest_path_under_the_hop_cap_finds_nothing",
        "shortest_path_of_one_type_finds_nothing",
        "no_directed_path_to_the_database",
    ] {
        assert_eq!(rows(name), json!([]), "{name}");
    }

    // Configured access: the rules chain to the database's group, and it is
    // not the same claim as the directed path above finding nothing.
    let rules = rows("candidate_path_through_security_groups");
    assert_eq!(rules[0][3], json!(DB_1));
}

/// The AWS acceptance case of #32: a typed walk traces the resource, explains
/// each hop, and reports when a cap cut it short.
#[test]
fn explore_traces_a_resource_through_typed_hops() {
    let env = Env::new();
    let db = env.synced_graph("graph");
    let trace = ["explore", "-d", text(&db), "--from", INTERNET, "--to", WEB_1, "--type", "ROUTES_TO,FORWARDS_TO"];

    let out = env.ok(&trace);
    assert_eq!(out["path"]["found"], true, "{out}");
    let words: Vec<&str> = out["path"]["hops"].as_array().unwrap().iter().map(|hop| hop["text"].as_str().unwrap()).collect();
    assert_eq!(words, ["Internet -[ROUTES_TO]-> shop", "shop -[FORWARDS_TO]-> web", "web -[ROUTES_TO]-> web-1"]);
    assert_eq!(out["complete"], true, "{out}");

    // Short of the hop it needs, it says the answer is cut, not that no path exists.
    let mut short = trace.to_vec();
    short.extend(["--max-hops", "2"]);
    let out = env.ok(&short);
    assert_eq!(out["path"]["found"], false, "{out}");
    assert_eq!(out["complete"], false, "{out}");
    assert_eq!(out["limits"], json!(["hops"]), "{out}");

    let (code, out) = env.khonsu(&["explore", "-d", text(&db), "--from", "shop/no-such-resource"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
    let (code, out) = env.khonsu(&["explore", "-d", text(&db), "--from", INTERNET, "--max-hops", "99"]);
    assert_ne!(code, 0, "{out}");
}

/// #33 on the fixture: where a relationship came from, and the path that
/// rests on it, by IDs a script can keep.
#[test]
fn evidence_is_machine_readable_and_states_what_is_unknown() {
    let env = Env::new();
    let db = env.synced_graph("graph");

    let out = env.ok(&["evidence", "-d", text(&db), "--rel", ALLOWS_443]);
    let record = &out["evidence"][0];
    assert_eq!(record["evidence_id"], format!("rel:{ALLOWS_443}"), "{out}");
    assert_eq!(record["kind"], "relationship");
    assert_eq!(record["types"], json!(["ALLOWS"]));
    assert_eq!(record["from"]["value"], format!("node:{INTERNET}"));
    assert_eq!(record["source"], json!({"status": "known", "value": "shop"}));
    assert_eq!(record["assertion"], "explicit");
    assert_eq!(record["properties"]["ports"], "443");
    assert_eq!(record["source_snapshot"], json!({"status": "unknown"}));
    assert_eq!(record["record_written_at"]["status"], "known");
    assert_eq!(record["source_last_successful_sync"]["status"], "known");

    // By its internal ID, the same record.
    let internal = record["internal_id"].as_i64().unwrap().to_string();
    let again = env.ok(&["evidence", "-d", text(&db), "--rel", &internal]);
    assert_eq!(again["evidence"][0]["evidence_id"], record["evidence_id"]);

    // Source configuration stays out of what gets copied around.
    let everything = serde_json::to_string(&env.ok(&["evidence", "-d", text(&db), "--node", WEB_1, "--rel", ALLOWS_443])).unwrap();
    assert!(!everything.contains("fixture_dir") && !everything.contains(text(&fixture())), "{everything}");

    let walk = env.ok(&["explore", "-d", text(&db), "--from", INTERNET, "--to", WEB_1, "--type", "ROUTES_TO,FORWARDS_TO"]);
    let path = &walk["path"]["evidence"];
    assert_eq!(path["assertion"], "derived", "{walk}");
    assert_eq!(path["rule_id"], json!({"status": "unknown"}));
    assert_eq!(path["supporting"].as_array().unwrap().len(), 7, "four nodes and three relationships: {path}");
    assert!(path["evidence_id"].as_str().unwrap().starts_with("path:"));

    let (code, out) = env.khonsu(&["evidence", "-d", text(&db), "--rel", "shop/no-such-relationship"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
    let (code, out) = env.khonsu(&["evidence", "-d", text(&db)]);
    assert_eq!(code, 2, "naming nothing is a usage mistake: {out}");
}

/// Written by hand, a record has no provenance to report, and a secret-shaped
/// property does not get out.
#[test]
fn evidence_for_a_record_written_by_hand_masks_secrets() {
    let env = Env::new();
    let db = env.path("graph");
    env.ok(&["init", text(&db)]);
    env.ok(&["query", "-d", text(&db), "--write", "--cypher", "CREATE (:Host {name: 'web', api_token: 'abc123'})"]);
    let id = env.query(&db, "MATCH (n:Host) RETURN id(n) AS id")["rows"][0][0].as_i64().unwrap().to_string();

    let out = env.ok(&["evidence", "-d", text(&db), "--node", &id]);
    let record = &out["evidence"][0];
    assert_eq!(record["stable"], false, "{out}");
    assert_eq!(record["assertion"], "unknown");
    for field in ["source", "plugin", "record_written_at", "source_last_successful_sync"] {
        assert_eq!(record[field], json!({"status": "unknown"}), "{field}");
    }
    assert_eq!(record["redacted"], json!(["api_token"]));
    assert!(!out.to_string().contains("abc123"), "{out}");
}

/// The finding about `resource` under `rule`.
fn finding<'a>(report: &'a Value, rule: &str, resource: &str) -> &'a Value {
    let wanted = format!("node:{resource}");
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["rule_id"] == rule && finding["resource"] == wanted)
        .unwrap_or_else(|| panic!("no {rule} finding on {resource}: {report}"))
}

fn condition_status<'a>(finding: &'a Value, id: &str) -> &'a str {
    let conditions = finding["conditions"].as_array().unwrap();
    conditions.iter().find(|condition| condition["id"] == id).unwrap_or_else(|| panic!("no {id}: {finding}"))["status"].as_str().unwrap()
}

const DIRECT: &str = "aws.exposure.direct-instance";
const BALANCER: &str = "aws.exposure.public-load-balancer";
const LATERAL: &str = "aws.exposure.security-group-lateral";

fn instance(id: &str) -> String {
    format!("shop/123456789012:us-east-1:{id}")
}

/// #35's three cases, spelled out so that blessing a wrong golden file would
/// still have to get past this test.
#[test]
fn the_exposure_fixture_has_a_positive_a_negative_and_an_undecided_case() {
    let env = Env::new();
    let db = env.path("graph");
    env.add_source(&db, &exposure_fixture());
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let report = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]);
    assert_eq!(report["complete"], true);
    assert_eq!(report["truncated"], 0);
    assert_eq!(report["subjects"], 5);
    assert!(report["claim"].as_str().unwrap().contains("does not show that anything is reachable"));

    // Positive: every fact the rule asks for is in the configuration.
    let web = finding(&report, DIRECT, &instance("i-web1"));
    assert_eq!(web["verdict"], "potential_exposure", "{web}");
    assert_eq!(web["path"]["hops"], json!(["Internet -[ALLOWS]-> web", "web <-[PROTECTED_BY]- web-1"]));
    assert_eq!(web["path"]["rule_id"], json!({"status": "known", "value": DIRECT}));
    assert_eq!(web["path"]["assertion"], "derived");
    assert!(web["assumptions"].as_array().unwrap().iter().any(|text| text.as_str().unwrap().contains("Network ACLs")));
    // Exact fields, with the name AWS gives them.
    let address = web["conditions"].as_array().unwrap().iter().find(|condition| condition["id"] == "public-address").unwrap();
    assert_eq!(address["fields"][0]["field"], "public_ip");
    assert_eq!(address["fields"][0]["source_field"], "ec2 describe-instances: PublicIpAddress");
    assert_eq!(address["fields"][0]["value"], json!({"status": "known", "value": "198.51.100.10"}));

    let app = finding(&report, BALANCER, &instance("i-app1"));
    assert_eq!(app["verdict"], "potential_exposure", "{app}");
    assert_eq!(app["path"]["hops"].as_array().unwrap().len(), 3);
    let db_1 = finding(&report, LATERAL, &instance("i-db1"));
    assert_eq!(db_1["verdict"], "potential_exposure", "{db_1}");
    assert_eq!(db_1["depends_on"], web["finding_id"]);

    // Negative: an open rule, and on its own that is not exposure.
    let batch = finding(&report, DIRECT, &instance("i-batch1"));
    assert_eq!(condition_status(batch, "open-inbound-rule"), "met");
    assert_eq!(condition_status(batch, "public-address"), "not_met");
    assert_eq!(condition_status(batch, "internet-route"), "not_met", "its subnet uses the main table, whose default goes to a NAT gateway");
    assert_eq!(batch["verdict"], "not_indicated", "{batch}");

    // Undecided: its VPC's route tables are not in the saved responses.
    let legacy = finding(&report, DIRECT, &instance("i-legacy1"));
    assert_eq!(condition_status(legacy, "open-inbound-rule"), "met");
    assert_eq!(condition_status(legacy, "public-address"), "met");
    assert_eq!(condition_status(legacy, "internet-route"), "unknown");
    assert_eq!(legacy["verdict"], "insufficient_data", "{legacy}");
    assert_eq!(legacy["unknowns"].as_array().unwrap().len(), 1);

    // The three verdicts are the only words a finding can use.
    let verdicts: std::collections::BTreeSet<&str> = report["findings"].as_array().unwrap().iter().map(|finding| finding["verdict"].as_str().unwrap()).collect();
    assert_eq!(verdicts, ["insufficient_data", "not_indicated", "potential_exposure"].into_iter().collect());
    assert_eq!(report["summary"], json!({"potential_exposure": 3, "insufficient_data": 1, "not_indicated": 3}));

    // Filters narrow what is printed, not what is counted.
    let only = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db), "--verdict", "insufficient-data"]);
    assert_eq!(exposure_brief(&only), json!([[DIRECT, format!("node:{}", instance("i-legacy1")), "insufficient_data"]]));
    let one = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db), "--resource", &instance("i-web1")]);
    assert_eq!(one["findings"].as_array().unwrap().len(), 1);
    let (code, out) = env.khonsu(&["plugin", "analyze", "aws", "exposure", "-d", text(&db), "--resource", "shop/no-such-instance"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "a mistyped ID is not an empty answer: {out}");
    let cut = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db), "--max-findings", "2"]);
    assert_eq!((cut["findings"].as_array().unwrap().len(), cut["truncated"].as_i64()), (2, Some(5)));
    assert_eq!(cut["summary"], report["summary"]);
}

/// A finding leads into the other two tools: its walk finds the resource in
/// Path Explorer, and every record it cites can be looked up as evidence.
#[test]
fn a_finding_opens_in_path_explorer_and_its_records_resolve_as_evidence() {
    let env = Env::new();
    let db = env.path("graph");
    env.add_source(&db, &exposure_fixture());
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let report = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]);

    let mut walked = 0;
    for finding in report["findings"].as_array().unwrap() {
        let name = &finding["finding_id"];
        let Value::Object(walk) = &finding["walk"] else {
            assert!(finding["path"].is_null(), "{finding}");
            continue;
        };
        walked += 1;
        let types = walk["types"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect::<Vec<_>>().join(",");
        let hops = walk["max_hops"].to_string();
        let out = env.ok(&[
            "explore", "-d", text(&db),
            "--from", walk["from"].as_str().unwrap(),
            "--to", walk["to"].as_str().unwrap(),
            "--direction", walk["direction"].as_str().unwrap(),
            "--type", &types,
            "--max-hops", &hops,
        ]);
        assert_eq!(out["path"]["found"], true, "{name}: {out}");

        let cited = finding["contributing"].as_array().unwrap();
        let records: Vec<&str> = finding["evidence"].as_array().unwrap().iter().map(|record| record["evidence_id"].as_str().unwrap()).collect();
        assert_eq!(records.len(), cited.len(), "{name}: every record it cites is included");
        for id in cited {
            let id = id.as_str().unwrap();
            let (flag, stable) = match id.split_once(':').unwrap() {
                ("node", stable) => ("--node", stable),
                ("rel", stable) => ("--rel", stable),
                other => panic!("{other:?}"),
            };
            let found = env.ok(&["evidence", "-d", text(&db), flag, stable]);
            assert_eq!(found["evidence"][0]["evidence_id"], id, "{name}");
        }
    }
    assert!(walked >= 3, "{report}");
}

/// A response that is missing reads as empty (#38), so the rules must not read
/// the records it would have given as absent from the account.
#[test]
fn a_missing_response_makes_a_finding_undecided_rather_than_clearing_it() {
    let env = Env::new();
    let saved = env.path("saved");
    copy_dir(&exposure_fixture(), &saved);
    let db = env.path("graph");
    env.add_source(&db, &saved);
    env.ok(&["plugin", "sync", "--source", "shop"]);
    assert_eq!(finding(&env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]), DIRECT, &instance("i-web1"))["verdict"], "potential_exposure");

    std::fs::remove_file(saved.join("us-east-1/ec2-describe-route-tables.json")).unwrap();
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let report = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]);
    let web = finding(&report, DIRECT, &instance("i-web1"));
    assert_eq!(web["verdict"], "insufficient_data", "{web}");
    assert_eq!(condition_status(web, "internet-route"), "unknown");

    std::fs::remove_file(saved.join("us-east-1/ec2-describe-security-groups.json")).unwrap();
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let report = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]);
    let web = finding(&report, DIRECT, &instance("i-web1"));
    assert_eq!(web["verdict"], "insufficient_data", "{web}");
    assert_eq!(condition_status(web, "open-inbound-rule"), "unknown");
    assert!(web["path"].is_null(), "{web}");
}

/// The shop fixture has a load balancer path that the graph can walk, and whose
/// target group's port the target's security group does not allow. The path
/// exists; the rule says why that does not make it exposure.
#[test]
fn a_candidate_path_is_not_a_finding_until_the_rules_conditions_hold() {
    let env = Env::new();
    let db = env.synced_graph("graph");
    let walk = env.ok(&["explore", "-d", text(&db), "--from", INTERNET, "--to", WEB_1, "--type", "ROUTES_TO,FORWARDS_TO"]);
    assert_eq!(walk["path"]["found"], true, "{walk}");

    let report = env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]);
    let balancer = finding(&report, BALANCER, WEB_1);
    assert_eq!(condition_status(balancer, "target-allows-balancer"), "not_met", "{balancer}");
    assert_eq!(balancer["verdict"], "not_indicated");
    assert_eq!(balancer["path"]["hops"].as_array().unwrap().len(), 3, "the path is still shown");
    assert_eq!(finding(&report, DIRECT, WEB_1)["verdict"], "insufficient_data", "no route tables in these responses");
}

#[test]
fn exposure_is_the_same_across_syncs() {
    let run = || {
        let env = Env::new();
        let db = env.path("graph");
        env.add_source(&db, &exposure_fixture());
        env.ok(&["plugin", "sync", "--source", "shop"]);
        normalize(&env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]))
    };
    assert_eq!(run(), run());
}

#[test]
fn two_syncs_of_the_same_input_agree_and_a_resync_changes_nothing() {
    let env = Env::new();
    let a = env.path("a");
    env.add_source(&a, &fixture());
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let other = Env::new();
    let b = other.path("b");
    other.add_source(&b, &fixture());
    other.ok(&["plugin", "sync", "--source", "shop"]);

    assert_eq!(normalize(&env.answers(&a)), normalize(&other.answers(&b)));
    let by_id = "MATCH (n) RETURN id(n) AS id, n.khonsu_id AS stable ORDER BY stable";
    assert_eq!(env.query(&a, by_id)["rows"], other.query(&b, by_id)["rows"]);

    let before = env.answers(&a);
    let again = env.ok(&["plugin", "sync", "--source", "shop"]);
    assert_eq!(again["merge"]["edges_written"], 0, "{again}");
    assert_eq!(again["merge"]["unchanged_edges"], 24, "{again}");
    assert_eq!(again["merge"]["stale_nodes_removed"], 0, "{again}");
    assert_eq!(normalize(&env.answers(&a)), normalize(&before));
}

#[test]
fn results_report_when_a_limit_was_reached() {
    let env = Env::new();
    let db = env.synced_graph("graph");
    let out = env.ok(&["query", "-d", text(&db), "--cypher", "MATCH (n) RETURN n.khonsu_id AS id", "--max-rows", "3"]);
    assert_eq!(out["truncated"], true, "{out}");
    assert_eq!(out["rows"].as_array().unwrap().len(), 3);
    assert_eq!(out["row_count"], 16, "row_count counts what the query found: {out}");

    let out = env.ok(&["query", "-d", text(&db), "--cypher", "MATCH (n) RETURN n.khonsu_id AS id", "--max-rows", "16"]);
    assert_eq!(out["truncated"], false, "{out}");
}

#[test]
fn traversal_ends_on_a_cycle() {
    let env = Env::new();
    let db = env.path("graph");
    env.ok(&["init", text(&db)]);
    for name in ["a", "b", "c"] {
        env.ok(&["query", "-d", text(&db), "--write", "--cypher", &format!("CREATE (:N {{name: '{name}'}})")]);
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "a")] {
        env.ok(&[
            "query", "-d", text(&db), "--write", "--cypher",
            &format!("MATCH (x:N {{name: '{from}'}}), (y:N {{name: '{to}'}}) CREATE (x)-[:NEXT]->(y)"),
        ]);
    }
    let out = env.query(
        &db,
        "MATCH p = shortestPath((x:N {name: 'a'})-[:NEXT*..50]->(y:N {name: 'c'})) RETURN [n IN nodes(p) | n.name] AS hops",
    );
    assert_eq!(out["rows"], json!([[["a", "b", "c"]]]));
    // A trail uses each relationship once, so a hop cap beyond the cycle's
    // length still ends: a→b, a→b→c, a→b→c→a.
    let out = env.query(&db, "MATCH (x:N {name: 'a'})-[:NEXT*1..50]->(y:N) RETURN y.name AS name ORDER BY name");
    assert_eq!(out["rows"], json!([["a"], ["b"], ["c"]]));
}

/// The other offline example, on the CSV demo that ships in the repository.
#[test]
fn the_csv_demo_dashboard_finds_rows() {
    let env = Env::new();
    let db = env.path("graph");
    let csv = repo().join("examples/csv");
    let file = |name: &str| csv.join(name).to_str().unwrap().replace('\\', "/");
    env.ok(&["init", text(&db)]);
    env.ok(&[
        "source", "add", "-d", text(&db), "--plugin", "csv", "--name", "infra",
        "-c", &format!(r#"nodes=[{{"path":"{}","label_column":"kind","id_column":"id"}}]"#, file("assets.csv")),
        "-c", &format!(r#"edges=[{{"path":"{}","type_column":"type","source_column":"source","target_column":"target","id_column":"id"}}]"#, file("links.csv")),
    ]);
    let out = env.ok(&["plugin", "sync", "--source", "infra"]);
    assert_eq!((out["received_nodes"].as_i64(), out["received_edges"].as_i64()), (Some(12), Some(14)), "{out}");

    let dash = repo().join("examples/infra/blast_radius.dash");
    let out = env.ok(&["check", text(&dash), "-d", text(&db)]);
    let rows: i64 = out["queries"].as_array().unwrap().iter().map(|query| query["rows"].as_i64().unwrap()).sum();
    assert!(rows > 0, "{out}");
}

/// A malformed response fails the sync and the graph keeps what it had.
#[test]
fn corrupt_input_fails_without_changing_the_graph() {
    let env = Env::new();
    let saved = env.path("saved");
    copy_dir(&fixture(), &saved);
    let db = env.path("graph");
    env.add_source(&db, &saved);
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let before = env.answers(&db);

    std::fs::write(saved.join("us-east-1/ec2-describe-instances.json"), "{not json").unwrap();
    let (code, out) = env.khonsu(&["plugin", "sync", "--source", "shop"]);
    assert_ne!(code, 0, "{out}");
    assert_eq!(out["error"]["kind"], "plugin", "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("ec2-describe-instances.json"), "{out}");
    assert_eq!(env.answers(&db), before);

    std::fs::remove_dir_all(&saved).unwrap();
    let (code, out) = env.khonsu(&["plugin", "sync", "--source", "shop"]);
    assert_ne!(code, 0, "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("not found"), "{out}");
    assert_eq!(env.answers(&db), before);

    // A dashboard with a mistake is a spec error, not a crash.
    let dash = env.path("bad.dash");
    std::fs::write(&dash, "plot \"x\" {\n  type = \"nonsense\"\n}\n").unwrap();
    let (code, out) = env.khonsu(&["check", text(&dash), "-d", text(&db)]);
    assert_eq!((code, out["error"]["kind"].as_str()), (2, Some("spec")), "{out}");

    // A damaged graph is refused with a reason.
    let damaged = env.path("damaged");
    copy_dir(&db, &damaged);
    std::fs::write(damaged.join("manifest.hawdb"), [0xff_u8; 64]).unwrap();
    let (code, out) = env.khonsu(&["query", "-d", text(&damaged), "--cypher", "MATCH (n) RETURN count(n) AS c"]);
    assert_ne!(code, 0, "{out}");
    assert_eq!(out["error"]["kind"], "open", "{out}");
}

/// KNOWN GAP, recorded so that #38 has to change it on purpose: a saved
/// response that is missing reads as an empty one, so a partial export syncs
/// without complaint, and a full re-sync removes what the missing file used
/// to describe. An absent edge here is not proof of absence.
#[test]
fn a_missing_response_is_read_as_empty_and_nothing_says_so() {
    let env = Env::new();
    let saved = env.path("saved");
    copy_dir(&fixture(), &saved);
    let db = env.path("graph");
    env.add_source(&db, &saved);
    env.ok(&["plugin", "sync", "--source", "shop"]);
    let allows = "MATCH ()-[r:ALLOWS]->() RETURN count(r) AS c";
    assert_eq!(env.count(&db, allows), 3);

    std::fs::remove_file(saved.join("us-east-1/ec2-describe-security-groups.json")).unwrap();
    let out = env.ok(&["plugin", "sync", "--source", "shop"]);
    assert_eq!(out["merge"]["warnings"], json!([]), "{out}");
    assert_eq!(env.count(&db, allows), 0);
}

#[test]
fn a_cancelled_sync_reports_it_and_leaves_the_graph_alone() {
    let env = Env::new();
    let db = env.synced_graph("graph");
    let before = env.answers(&db);

    // The only test here that reads settings in this process; the others hand
    // the directory to the `khonsu` they spawn.
    std::env::set_var("KHONSU_CONFIG_DIR", env.config());
    let source = settings::load_sources().into_iter().find(|source| source.id == "shop").unwrap();
    {
        let mut graph = Graph::open(&db, AccessMode::ReadWrite, "test").unwrap();
        let cancel = AtomicBool::new(false);
        let result = plugin::sync_source(&mut graph, &source, SyncMode::Full, &cancel, |_| cancel.store(true, Ordering::Relaxed));
        assert!(matches!(result, Err(PluginError::Cancelled)), "{result:?}");
    }
    assert_eq!(env.answers(&db), before);
    let (_, sources) = env.khonsu(&["source", "list"]);
    assert_eq!(sources["sources"][0]["last_sync"]["ok"], false, "{sources}");
}

/// Every graph kept in tests/fixtures still reads the same and takes a write,
/// which is what a HawDB upgrade can break. Bumping the pin needs a snapshot
/// of the new version too, so the next upgrade is tested against this one.
#[test]
fn graphs_written_by_earlier_hawdb_versions_keep_their_meaning() {
    let current = snapshots_dir().join(format!("graph-hawdb-{}.zip", khonsu_core::HAWDB_VERSION));
    assert!(blessing() || current.is_file(), "no snapshot for HawDB {}; run with KHONSU_BLESS=1", khonsu_core::HAWDB_VERSION);

    let snapshots = std::fs::read_dir(snapshots_dir()).into_iter().flatten().flatten().map(|entry| entry.path());
    let expected = golden();
    for zip_path in snapshots.filter(|path| path.extension().is_some_and(|ext| ext == "zip")) {
        let env = Env::new();
        let db = env.path("graph");
        let file = std::fs::File::open(&zip_path).unwrap();
        zip::ZipArchive::new(file).unwrap().extract(&db).unwrap();

        let name = zip_path.file_name().unwrap().to_string_lossy().into_owned();
        assert_same(&format!("{name}: answers"), &normalize(&env.answers(&db)), &expected["answers"]);
        assert_same(&format!("{name}: schema"), &normalize(&env.ok(&["schema", "-d", text(&db)])), &expected["schema"]);
        assert_same(&format!("{name}: exposure"), &normalize(&exposure_brief(&env.ok(&["plugin", "analyze", "aws", "exposure", "-d", text(&db)]))), &expected["exposure"]["shop_fixture"]);

        env.ok(&["query", "-d", text(&db), "--write", "--cypher", "CREATE (:Note {name: 'after upgrade'})"]);
        assert_eq!(env.count(&db, "MATCH (n) RETURN count(n) AS c"), 17, "{name}");
    }
}

/// Builds the example analyzer, and installs it as a plugin. Approved unless asked otherwise.
fn install_analyzer(env: &Env, approve: bool) {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "--locked", "-p", "khonsu-plugin-protocol", "--example", "analyzer"])
            .status()
            .unwrap();
        assert!(status.success());
    });
    let exe_name = format!("analyzer{}", std::env::consts::EXE_SUFFIX);
    let built = Path::new(env!("CARGO_BIN_EXE_khonsugraph")).parent().unwrap().join("examples").join(&exe_name);
    let dir = env.path("analyzer-plugin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(built, dir.join(&exe_name)).unwrap();
    let manifest = json!({"id": "analyzer", "name": "Analyzer", "version": "1.0.0", "protocol": 1, "entry": ["analyzer"]});
    std::fs::write(dir.join("khonsu-plugin.json"), manifest.to_string()).unwrap();
    let mut args = vec!["plugin", "install", text(&dir)];
    if approve {
        args.push("--yes");
    }
    env.ok(&args);
}

/// `a -> b -> c`, with nothing to do with any provider.
fn graph_of_things(env: &Env) -> PathBuf {
    let db = env.path("things");
    env.ok(&["init", text(&db)]);
    for name in ["a", "b", "c"] {
        env.ok(&["query", "-d", text(&db), "--write", "--cypher", &format!("CREATE (:Thing {{khonsu_id: '{name}', name: '{name}'}})")]);
    }
    for (from, to) in [("a", "b"), ("b", "c")] {
        let link = format!("MATCH (x:Thing {{khonsu_id: '{from}'}}), (y:Thing {{khonsu_id: '{to}'}}) CREATE (x)-[:LINKS {{khonsu_id: '{from}{to}'}}]->(y)");
        env.ok(&["query", "-d", text(&db), "--write", "--cypher", &link]);
    }
    db
}

/// The shared interface works with no AWS anywhere in it.
#[test]
fn an_analysis_from_a_plugin_that_is_nothing_to_do_with_aws_works_the_same_way() {
    let env = Env::new();
    install_analyzer(&env, true);
    let db = graph_of_things(&env);

    let described = env.ok(&["plugin", "describe", "analyzer"]);
    assert_eq!(described["analyses"][0]["id"], "dead-ends");

    let report = env.ok(&["plugin", "analyze", "analyzer", "dead-ends", "-d", text(&db)]);
    assert_eq!(report["plugin"], "analyzer");
    assert_eq!(report["subjects"], 3);
    assert_eq!(report["complete"], true);
    assert_eq!(report["summary"], json!({"dead_end": 1, "continues": 2}));
    assert_eq!(report["truncated"], 0);

    let dead_end = &report["findings"][0];
    assert_eq!((dead_end["resource"].as_str(), dead_end["verdict"].as_str()), (Some("node:c"), Some("dead_end")), "{report}");
    assert_eq!(dead_end["resource_name"], "c");
    assert_eq!(dead_end["conditions"][0]["fields"][0]["value"], json!({"status": "known", "value": "c"}), "read from the graph");

    // Its path is rebuilt from references, and it leads into the other tools.
    assert_eq!(dead_end["path"]["hops"], json!(["b -[LINKS]-> c"]));
    assert_eq!(dead_end["path_refs"], json!([{"edge": "rel:bc", "along": true}]));
    let walk = &dead_end["walk"];
    let out = env.ok(&[
        "explore", "-d", text(&db),
        "--from", walk["from"].as_str().unwrap(),
        "--to", walk["to"].as_str().unwrap(),
        "--direction", walk["direction"].as_str().unwrap(),
        "--type", "LINKS",
        "--max-hops", "1",
    ]);
    assert_eq!(out["path"]["found"], true, "{out}");
    for id in dead_end["contributing"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        let (flag, stable) = if let Some(stable) = id.strip_prefix("node:") { ("--node", stable) } else { ("--rel", id.strip_prefix("rel:").unwrap()) };
        assert_eq!(env.ok(&["evidence", "-d", text(&db), flag, stable])["evidence"][0]["evidence_id"], id);
    }

    // Filters are in the analysis's own words.
    let narrow = env.ok(&["plugin", "analyze", "analyzer", "dead-ends", "-d", text(&db), "--verdict", "dead-end"]);
    assert_eq!(narrow["findings"].as_array().unwrap().len(), 1);
    assert_eq!(narrow["summary"], report["summary"]);
    let (code, out) = env.khonsu(&["plugin", "analyze", "analyzer", "dead-ends", "-d", text(&db), "--verdict", "reachable"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (2, Some("usage")), "{out}");
}

#[test]
fn a_result_that_misbehaves_is_refused_and_says_why() {
    let env = Env::new();
    install_analyzer(&env, true);
    let db = graph_of_things(&env);
    for (analysis, expected) in [("forged", "node:forged"), ("unfinished", "without finishing"), ("fails", "could not run")] {
        let (code, out) = env.khonsu(&["plugin", "analyze", "analyzer", analysis, "-d", text(&db)]);
        assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("plugin")), "{analysis}: {out}");
        let message = out["error"]["message"].as_str().unwrap();
        assert!(message.contains(expected), "{analysis}: {message}");
    }
    let (code, out) = env.khonsu(&["plugin", "analyze", "analyzer", "no-such", "-d", text(&db)]);
    assert_eq!(code, 1, "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("offers dead-ends"), "{out}");
}

#[test]
fn an_analysis_needs_the_same_approval_as_a_sync() {
    let env = Env::new();
    install_analyzer(&env, false);
    let db = graph_of_things(&env);
    let (code, out) = env.khonsu(&["plugin", "analyze", "analyzer", "dead-ends", "-d", text(&db)]);
    assert_eq!(code, 1, "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("approval"), "{out}");
}

/// A data-source plugin with no analyses goes on working, and says plainly that
/// it has none.
#[test]
fn a_plugin_with_no_analyses_still_syncs_and_says_it_has_none() {
    let env = Env::new();
    let db = env.path("graph");
    let csv = env.path("hosts.csv");
    std::fs::write(&csv, "id,name
1,web
2,db
").unwrap();
    env.ok(&["init", text(&db)]);
    env.ok(&["source", "add", "-d", text(&db), "--plugin", "csv", "--name", "hosts", "-c", &format!("node_file={}", text(&csv)), "-c", "node_label=Host"]);
    env.ok(&["plugin", "sync", "--source", "hosts"]);
    assert_eq!(env.query(&db, "MATCH (h:Host) RETURN count(h) AS hosts")["rows"][0][0], 2);

    assert!(env.ok(&["plugin", "describe", "csv"]).get("analyses").is_none());
    let (code, out) = env.khonsu(&["plugin", "analyze", "csv", "anything", "-d", text(&db)]);
    assert_eq!(code, 1, "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("offers no analyses"), "{out}");
}

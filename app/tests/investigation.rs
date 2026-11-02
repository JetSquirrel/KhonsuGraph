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

/// Drops what legitimately differs between runs and machines: timings, sync
/// times and paths. Floats are rounded so the last bits of a platform's
/// arithmetic do not count as a change of meaning.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !matches!(key.as_str(), "elapsed_ms" | "load_ms" | "khonsu_synced_at" | "file" | "line"))
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
        "explore": {
            "exposure_of_web_1": env.ok(&[
                "explore", "-d", text(&db), "--from", INTERNET, "--to", WEB_1,
                "--type", "ROUTES_TO,FORWARDS_TO", "--max-hops", "4",
            ]),
            "what_points_at_web_1": env.ok(&["explore", "-d", text(&db), "--from", WEB_1, "--direction", "in", "--max-hops", "1"]),
        },
    }))
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

        env.ok(&["query", "-d", text(&db), "--write", "--cypher", "CREATE (:Note {name: 'after upgrade'})"]);
        assert_eq!(env.count(&db, "MATCH (n) RETURN count(n) AS c"), 17, "{name}");
    }
}

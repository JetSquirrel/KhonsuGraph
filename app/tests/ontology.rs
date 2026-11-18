//! `khonsu graph ontology` over a small graph shaped like the security example, with
//! the example's ATT&CK pack and mapping (`examples/security/ontology`).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        Self { dir: tempfile::tempdir_in(base).unwrap() }
    }

    fn graph(&self) -> PathBuf {
        self.dir.path().join("graph")
    }

    /// Runs `khonsu` and returns (exit code, stdout JSON).
    fn khonsu(&self, args: &[&str]) -> (i32, Value) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_khonsugraph"));
        command.env_remove("KHONSU_AGENT");
        for (variable, _) in khonsu_dash::presence::AGENT_VARIABLES {
            command.env_remove(variable);
        }
        let output = command
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.dir.path().join("config"))
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.code().unwrap_or(-1), json)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, out) = self.khonsu(args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn pack() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/security/ontology").to_string_lossy().into_owned()
}

/// A technique a process indicates, the process ran on a host and was started
/// by a user, and it connected to an address. No Location anywhere.
fn security_graph(env: &Env) -> PathBuf {
    let db = env.graph();
    env.ok(&["graph", "init", text(&db)]);
    let write = |cypher: &str| {
        env.ok(&["query", "-d", text(&db), "--write", "--cypher", cypher]);
    };
    write("CREATE (:Technique {name: 'LSASS Memory', external_id: 'T1003.001'})");
    write("CREATE (:Process {name: 'mimikatz.exe'})");
    write("CREATE (:Host {name: 'WORKSTATION5'})");
    write("CREATE (:User {name: 'THESHIRE\\\\pgustavo'})");
    write("CREATE (:IP {name: '10.0.0.7'})");
    write("MATCH (p:Process), (t:Technique) CREATE (p)-[:INDICATES]->(t)");
    write("MATCH (p:Process), (h:Host) CREATE (p)-[:RAN_ON]->(h)");
    write("MATCH (u:User), (p:Process) CREATE (u)-[:STARTED]->(p)");
    write("MATCH (p:Process), (i:IP) CREATE (p)-[:CONNECTED_TO]->(i)");
    db
}

#[test]
fn the_library_loads_the_pack_and_describes_a_concept() {
    let env = Env::new();
    let pack = pack();
    let out = env.ok(&["graph", "ontology", "describe", "--brief", "--ontology", &pack]);
    assert!(out.get("problems").is_none(), "{out}");
    let ids: Vec<&str> = out["ontology_versions"].as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["khonsu-core", "attack", "security-example"]);

    let out = env.ok(&["graph", "ontology", "describe", "--concept", "device", "--ontology", &pack]);
    assert_eq!(out["concept"]["name"], "Device");
    assert_eq!(out["concept"]["ancestors"], serde_json::json!(["Asset", "Entity"]));
    assert_eq!(out["concept"]["labels"][0]["label"], "Host");
    assert!(out["drafts"].as_array().unwrap().iter().any(|d| d == "concept:Device"), "{out}");

    let (code, out) = env.khonsu(&["graph", "ontology", "describe", "--concept", "devise"]);
    assert_eq!(code, 1);
    assert_eq!(out["error"]["kind"], "not_found");
    assert_eq!(out["error"]["detail"]["suggestions"][0], "Device");
}

#[test]
fn words_resolve_to_concepts_and_the_labels_that_hold_them() {
    let env = Env::new();
    let out = env.ok(&["graph", "ontology", "resolve", "host", "lsass", "--ontology", &pack()]);
    let host = &out["words"][0];
    assert!(host["matches"].as_array().unwrap().iter().any(|m| m["name"] == "Device"), "{out}");
    assert_eq!(host["labels"], serde_json::json!(["Host"]));
    let lsass = &out["words"][1];
    assert_eq!(lsass["matches"], serde_json::json!([]));
}

#[test]
fn check_says_what_the_graph_cannot_answer_and_exits_3() {
    let env = Env::new();
    let db = security_graph(&env);
    let pack = pack();

    let out = env.ok(&["graph", "ontology", "check", "-d", text(&db), "--entities", "technique,process,host,account,ip", "--ontology", &pack]);
    assert_eq!(out["investigable"], true);
    assert_eq!(out["coverage"]["covered"], 5);

    let (code, out) = env.khonsu(&["graph", "ontology", "check", "-d", text(&db), "--entities", "account,location,blorp", "--ontology", &pack]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(out["error"]["kind"], "not_investigable");
    let missing: Vec<&Value> = out["error"]["detail"]["coverage"]["terms"].as_array().unwrap().iter().map(|t| &t["missing"]).collect();
    assert_eq!(missing, [&Value::Null, &Value::from("no_mapping"), &Value::from("unknown_term")]);

    // Without the mapping the graph's labels mean nothing.
    let (code, out) = env.khonsu(&["graph", "ontology", "check", "-d", text(&db), "--entities", "account"]);
    assert_eq!(code, 3, "{out}");
}

#[test]
fn path_gives_declared_routes_and_cypher_over_the_graph() {
    let env = Env::new();
    let db = security_graph(&env);
    let pack = pack();

    let out = env.ok(&["graph", "ontology", "path", "--from", "Technique", "--to", "Device", "-d", text(&db), "--ontology", &pack]);
    let patterns: Vec<&str> = out["graph"]["paths"].as_array().unwrap().iter().map(|p| p["pattern"].as_str().unwrap()).collect();
    assert_eq!(patterns, ["(a:Technique)<-[:INDICATES]-(:Process)-[:RAN_ON]->(b:Host)"]);
    assert!(!out["declared"].as_array().unwrap().is_empty(), "{out}");

    // The pattern runs as it is.
    let query = format!("MATCH {} RETURN a.external_id AS technique, b.name AS host", patterns[0]);
    let rows = env.ok(&["query", "-d", text(&db), "--cypher", &query]);
    assert_eq!(rows["rows"], serde_json::json!([["T1003.001", "WORKSTATION5"]]));

    let (code, _) = env.khonsu(&["graph", "ontology", "path", "--from", "Technique", "--to", "Device", "--max-hops", "9"]);
    assert_eq!(code, 2);
}

#[test]
fn describe_with_a_graph_lists_what_is_mapped_and_what_is_not() {
    let env = Env::new();
    let db = security_graph(&env);
    env.ok(&["query", "-d", text(&db), "--write", "--cypher", "CREATE (:Printer {name: 'p1'})"]);
    let out = env.ok(&["graph", "ontology", "describe", "--brief", "-d", text(&db), "--ontology", &pack()]);
    let unmapped: Vec<&str> = out["graph"]["unmapped"].as_array().unwrap().iter().map(|l| l["label"].as_str().unwrap()).collect();
    assert_eq!(unmapped, ["Printer"]);
    let absent: Vec<&str> = out["graph"]["mapped_but_absent"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    assert!(absent.contains(&"Mitigation") && !absent.contains(&"Host"), "{out}");
}

#[test]
fn a_semantic_query_runs_in_the_ontologys_words() {
    let env = Env::new();
    let db = security_graph(&env);
    let pack = pack();

    let out = env.ok(&[
        "query", "-d", text(&db), "--semantic", "--ontology", &pack, "--cypher",
        "MATCH (t:Technique)<-[:indicates]-(p:Process)-[:runs_on]->(d:Device) RETURN t.external_id AS technique, d.name AS device",
    ]);
    assert_eq!(out["rows"], serde_json::json!([["T1003.001", "WORKSTATION5"]]), "{out}");
    assert_eq!(
        out["semantic"]["statement"],
        "MATCH (t:Technique)<-[:INDICATES]-(p:Process)-[:RAN_ON]->(d:Host) RETURN t.external_id AS technique, d.name AS device"
    );
    assert_eq!(out["semantic"]["valid"], true);
    let written: Vec<&str> = out["semantic"]["replacements"].as_array().unwrap().iter().map(|r| r["written"].as_str().unwrap()).collect();
    assert!(written.contains(&"Device") && written.contains(&"runs_on"), "{out}");
    assert!(out["semantic"]["drafts"].as_array().unwrap().iter().any(|d| d == "concept:Device"));

    // Nothing in the graph is a Location: refused, not an empty answer.
    let (code, out) = env.khonsu(&["query", "-d", text(&db), "--semantic", "--ontology", &pack, "--cypher", "MATCH (l:Location) RETURN l"]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(out["error"]["detail"]["missing"], serde_json::json!(["Location"]));
}

#[test]
fn validate_only_checks_patterns_against_the_graph_and_runs_nothing() {
    let env = Env::new();
    let db = security_graph(&env);

    let out = env.ok(&["query", "-d", text(&db), "--validate-only", "--cypher", "MATCH (p:Process)-[:RAN_ON]->(h:Host) RETURN h"]);
    assert_eq!(out["valid"], true);
    assert!(out.get("rows").is_none(), "nothing runs: {out}");

    let (code, out) = env.khonsu(&["query", "-d", text(&db), "--validate-only", "--cypher", "MATCH (h:Host)-[:RAN_ON]->(p:Process) RETURN p"]);
    assert_eq!(code, 2, "{out}");
    assert_eq!(out["error"]["kind"], "invalid_query");
    assert_eq!(out["error"]["detail"]["issues"][0]["kind"], "wrong_direction");

    // Warnings don't stop a statement.
    let out = env.ok(&["query", "-d", text(&db), "--validate-only", "--cypher", "MATCH (u:User)-[:STARTED*]->(p) RETURN p"]);
    assert_eq!(out["valid"], true);
    assert_eq!(out["issues"][0]["kind"], "unbounded");
}

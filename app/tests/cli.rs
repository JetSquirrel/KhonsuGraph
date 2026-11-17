use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        Self {
            dir: tempfile::tempdir_in(base).unwrap(),
        }
    }

    fn graph(&self) -> PathBuf {
        self.dir.path().join("graph")
    }

    /// Runs `khonsu` and returns (success, stdout JSON).
    fn khonsu(&self, args: &[&str]) -> (bool, Value) {
        let output = as_a_person(Command::new(env!("CARGO_BIN_EXE_khonsugraph")))
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.dir.path().join("config"))
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.success(), json)
    }

    /// Runs `khonsu` as the agent `agent` says it is, and returns (exit code, stdout JSON).
    fn khonsu_as_agent(&self, agent: &str, args: &[&str]) -> (i32, Value) {
        let output = as_a_person(Command::new(env!("CARGO_BIN_EXE_khonsugraph")))
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.dir.path().join("config"))
            .env("KHONSU_AGENT", agent)
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.code().unwrap_or(-1), json)
    }
}

/// The command as a person would run it: without the variables that show an
/// agent, which a test inherits when an agent runs it.
fn as_a_person(mut command: Command) -> Command {
    command.env_remove("KHONSU_AGENT");
    for (variable, _) in khonsu_dash::presence::AGENT_VARIABLES {
        command.env_remove(variable);
    }
    command
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn init_write_query_schema_and_explain() {
    let env = Env::new();
    let db = env.graph();
    let (ok, out) = env.khonsu(&["init", path(&db)]);
    assert!(ok, "{out}");

    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--cypher", "CREATE (:Host {name: 'web'})"]);
    assert!(!ok, "writes need --write: {out}");

    let (ok, out) = env.khonsu(&[
        "query", "-d", path(&db), "--write", "--cypher",
        "CREATE (:Host {name: $name})-[:RUNS]->(:Service {name: 'api'})", "-p", "name=web",
    ]);
    assert!(ok, "{out}");

    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--cypher", "MATCH (h:Host)-[r]->(s) RETURN h, r, s"]);
    assert!(ok, "{out}");
    assert_eq!(out["row_count"], 1);
    assert_eq!(out["truncated"], false);
    assert_eq!(out["graph"]["edges"][0]["rel_type"], "RUNS");

    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--cypher", "MATCH (n) RETURN n", "--max-rows", "1"]);
    assert!(ok);
    assert_eq!(out["rows"].as_array().unwrap().len(), 1);
    assert_eq!(out["truncated"], true);

    let (ok, out) = env.khonsu(&["schema", "-d", path(&db)]);
    assert!(ok, "{out}");
    let labels: Vec<&str> = out["labels"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(labels, ["Host", "Service"]);

    let (ok, out) = env.khonsu(&["explain", "-d", path(&db), "--cypher", "MATCH (n:Host) RETURN n"]);
    assert!(ok);
    assert!(out["plan"].as_str().unwrap().contains("label:Host"));

    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--cypher", "MATCH (n) RETURN n LIMIT"]);
    assert!(!ok);
    assert_eq!(out["error"]["kind"], "parse");
    assert_eq!(out["error"]["detail"]["line"], 1);
}

/// HawDB 0.8 leases a graph directory to one process, readers included, so
/// the CLI reports who has the graph open instead of failing obscurely.
#[test]
fn cli_reports_the_app_holding_the_graph() {
    let env = Env::new();
    let mut app = Graph::create(env.graph(), "KhonsuGraph").unwrap();
    app.run(Language::Cypher, "CREATE (:Host {name: 'web'})", &Map::new(), QueryOptions::default())
        .unwrap();

    let graph = env.graph();
    for write in [false, true] {
        let mut args = vec!["query", "-d", path(&graph), "--cypher", "MATCH (n:Host) RETURN n.name AS name"];
        if write {
            args.push("--write");
        }
        let (ok, out) = env.khonsu(&args);
        assert!(!ok);
        assert_eq!(out["error"]["kind"], "locked", "{out}");
        assert_eq!(out["error"]["detail"]["owner"]["program"], "KhonsuGraph");
        assert_eq!(out["error"]["detail"]["owner"]["pid"], std::process::id());
    }
    drop(app);

    let (ok, out) = env.khonsu(&["query", "-d", path(&env.graph()), "--cypher", "MATCH (n:Host) RETURN n.name AS name"]);
    assert!(ok, "{out}");
    assert_eq!(out["rows"][0][0], "web");
}

/// `cargo test` builds the plugin only as a test harness; the CLI looks for
/// the real executable beside itself in target/<profile>.
fn build_csv_plugin() {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "khonsu-plugin-csv"])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn csv_source_add_and_sync() {
    build_csv_plugin();
    let env = Env::new();
    let db = env.graph();
    env.khonsu(&["init", path(&db)]);
    let data = env.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("hosts.csv"), "id,name\nh1,web\nh2,db\nh3,cache\n").unwrap();
    std::fs::write(data.join("deps.csv"), "source,target\nh1,h2\nh1,h3\n").unwrap();

    let (ok, out) = env.khonsu(&["plugin", "list"]);
    assert!(ok);
    assert!(out["plugins"].as_array().unwrap().iter().any(|p| p["id"] == "csv" && p["builtin"] == true), "{out}");

    let (ok, out) = env.khonsu(&[
        "source", "add", "-d", path(&db), "--plugin", "csv", "--name", "Lab hosts", "--test",
        "-c", &format!("node_file={}", path(&data.join("hosts.csv"))),
        "-c", "node_label=Host",
        "-c", &format!("edge_file={}", path(&data.join("deps.csv"))),
        "-c", "edge_type=DEPENDS_ON",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["added"]["id"], "lab-hosts");

    let (ok, out) = env.khonsu(&["plugin", "sync", "--source", "lab-hosts", "--dry-run"]);
    assert!(ok, "{out}");
    assert_eq!(out["preview"]["nodes"], 3);

    let (ok, out) = env.khonsu(&["plugin", "sync", "--source", "lab-hosts"]);
    assert!(ok, "{out}");
    assert_eq!(out["merge"]["nodes_written"], 3);
    assert_eq!(out["merge"]["edges_written"], 2);

    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--cypher", "MATCH (a:Host)-[r:DEPENDS_ON]->(b:Host) RETURN count(r) AS c"]);
    assert!(ok, "{out}");
    assert_eq!(out["rows"][0][0], 2);
}

/// A plugin folder that installs: its entry is never run by these tests.
fn plugin_folder(env: &Env, id: &str) -> PathBuf {
    let dir = env.dir.path().join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({"id": id, "name": id, "version": "1", "protocol": 1, "entry": ["plugin.sh"]});
    std::fs::write(dir.join("khonsu-plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(dir.join("plugin.sh"), "").unwrap();
    dir
}

fn approval(env: &Env, id: &str) -> Value {
    let (ok, out) = env.khonsu(&["plugin", "list"]);
    assert!(ok, "{out}");
    let plugin = out["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap_or_else(|| panic!("{id} not listed: {out}")).clone();
    plugin["approval"].clone()
}

/// Approving lets a plugin run as the person, so an agent's approval is
/// refused with exit 4, while what it asked besides (the install) is done.
#[test]
fn an_agent_installs_but_does_not_approve() {
    let env = Env::new();
    let folder = plugin_folder(&env, "tool");

    let (code, out) = env.khonsu_as_agent("Tester", &["plugin", "install", path(&folder), "--yes"]);
    assert_eq!(code, 4, "{out}");
    assert_eq!(out["error"]["kind"], "needs_confirmation");
    assert_eq!(out["error"]["detail"]["agent"], "Tester");
    assert_eq!(out["error"]["detail"]["done"]["installed"], "tool");
    assert_eq!(out["error"]["detail"]["done"]["approved"], false);
    assert!(out["error"]["detail"]["done"]["note"].as_str().unwrap().contains("Ask the person"), "{out}");
    assert_ne!(approval(&env, "tool"), "approved");

    let (code, out) = env.khonsu_as_agent("Tester", &["plugin", "approve", "tool"]);
    assert_eq!(code, 4, "{out}");
    assert_eq!(out["error"]["kind"], "needs_confirmation");
    assert_ne!(approval(&env, "tool"), "approved");

    // Without --yes there is nothing to refuse: the install is the agent's to make.
    let (code, out) = env.khonsu_as_agent("Tester", &["plugin", "install", path(&folder)]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["approved"], false);

    // The person approves.
    let (ok, out) = env.khonsu(&["plugin", "approve", "tool"]);
    assert!(ok, "{out}");
    assert_eq!(approval(&env, "tool"), "approved");
}

/// A statement written by hand is not in any source, so it goes in the log of
/// writes, with who ran it and secrets masked. Reads and refused writes do not.
#[test]
fn writes_by_hand_are_logged() {
    let env = Env::new();
    let db = env.graph();
    let (ok, out) = env.khonsu(&["init", path(&db)]);
    assert!(ok, "{out}");
    let log = env.dir.path().join("config").join("writes.jsonl");

    let (ok, _) = env.khonsu(&["query", "-d", path(&db), "--cypher", "CREATE (:Host {name: 'refused'})"]);
    assert!(!ok);
    let (ok, _) = env.khonsu(&["query", "-d", path(&db), "--write", "--cypher", "MATCH (n) RETURN count(n) AS n"]);
    assert!(ok);
    assert!(!log.exists(), "a read is not a write");

    let (code, out) = env.khonsu_as_agent(
        "Tester",
        &["query", "-d", path(&db), "--write", "--cypher", "CREATE (:Key {id: 'AKIAIOSFODNN7EXAMPLE', secret: $secret})", "-p", "secret=hunter2"],
    );
    assert_eq!(code, 0, "{out}");
    let (ok, out) = env.khonsu(&["query", "-d", path(&db), "--write", "--cypher", "CREATE (:Host {name: 'web'})"]);
    assert!(ok, "{out}");

    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0]["agent"], "Tester");
    assert_eq!(lines[0]["via"], "cli");
    assert_eq!(lines[0]["language"], "cypher");
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE") && !text.contains("hunter2"), "{text}");
    assert!(lines[1].get("agent").is_none(), "{text}");
}

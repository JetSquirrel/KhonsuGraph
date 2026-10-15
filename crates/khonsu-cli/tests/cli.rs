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
        let output = Command::new(env!("CARGO_BIN_EXE_khonsu"))
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.dir.path().join("config"))
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.success(), json)
    }
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

#[test]
fn csv_source_add_and_sync() {
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

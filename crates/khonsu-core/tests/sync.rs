mod common;

use common::TestDir;
use khonsu_core::plugin::{self, SyncEvent};
use khonsu_core::protocol::SyncMode;
use khonsu_core::settings::{self, Source};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{json, Map, Value};
use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;

fn build_csv_plugin() {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-q", "-p", "khonsu-plugin-csv"])
        .status()
        .unwrap();
    assert!(status.success());
}

fn count(graph: &mut Graph, query: &str) -> i64 {
    let result = graph.run(Language::Cypher, query, &Map::new(), QueryOptions::default()).unwrap();
    result.rows[0][0].as_i64().unwrap()
}

fn write_csvs(dir: &std::path::Path, hosts: usize, skip_edge: Option<usize>) -> usize {
    let mut nodes = String::from("id,name,cpu,region\n");
    for i in 0..hosts {
        writeln!(nodes, "h{i},host {i},{},eu-{}", i % 8 + 1, i % 3).unwrap();
    }
    nodes.push_str("svc-api,api,,\nsvc-db,db,,\n");
    std::fs::write(dir.join("nodes.csv"), nodes).unwrap();
    let mut edges = String::from("id,source,target,type,port\n");
    let mut written = 0;
    for i in 0..hosts {
        if Some(i) == skip_edge {
            continue;
        }
        let target = if i % 2 == 0 { "svc-api" } else { "svc-db" };
        writeln!(edges, "e{i},h{i},{target},RUNS,{}", 8000 + i).unwrap();
        written += 1;
    }
    writeln!(edges, "e-dep,svc-api,svc-db,DEPENDS_ON,5432").unwrap();
    std::fs::write(dir.join("edges.csv"), edges).unwrap();
    written + 1
}

#[test]
#[ignore = "CSV plugin tests are skipped for now; run with --ignored"]
fn csv_source_syncs_counts_match_rows_and_resync_is_idempotent() {
    build_csv_plugin();
    let dir = TestDir::new();
    std::env::set_var("KHONSU_CONFIG_DIR", dir.0.path().join("config"));
    let data = dir.0.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let edge_rows = write_csvs(&data, 60, None);

    let csv = plugin::find("csv").expect("built-in CSV plugin is discoverable");
    assert!(csv.builtin);
    let describe = plugin::describe(&csv).unwrap();
    assert_eq!(describe.name, "CSV");

    let mut config = Map::new();
    config.insert("base_dir".into(), json!(data.display().to_string()));
    config.insert(
        "nodes".into(),
        json!([{"path": "nodes.csv", "label_column": null, "label": "Asset", "id_column": "id"}]),
    );
    config.insert(
        "edges".into(),
        json!([{"path": "edges.csv", "type_column": "type", "source_column": "source", "target_column": "target", "id_column": "id"}]),
    );
    let source = Source {
        id: "lab".into(),
        name: "Lab".into(),
        plugin: "csv".into(),
        graph: dir.graph(),
        config,
        secret_keys: Vec::new(),
        cursor: None,
        patterns: Vec::new(),
        last_sync: None,
    };
    settings::upsert_source(source.clone()).unwrap();

    let request = plugin::request_for(&source, SyncMode::Full).unwrap();
    let cancel = AtomicBool::new(false);
    let preview = plugin::preview(&csv, &request, &cancel, |_| {}).unwrap();
    assert_eq!(preview.nodes, 62);
    assert_eq!(preview.edges as usize, edge_rows);
    assert_eq!(preview.relationship_types["RUNS"], 60);

    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let mut events = Vec::new();
    let report = plugin::sync_source(&mut graph, &source, SyncMode::Full, &cancel, |e| events.push(e)).unwrap();
    assert_eq!(report.merge.nodes_written, 62);
    assert_eq!(report.merge.edges_written as usize, edge_rows);
    assert!(events.iter().any(|e| matches!(e, SyncEvent::Counts { .. })));
    assert_eq!(count(&mut graph, "MATCH (n:Asset) RETURN count(n) AS c"), 62);
    assert_eq!(count(&mut graph, "MATCH ()-[r:RUNS]->() RETURN count(r) AS c"), 60);
    assert_eq!(count(&mut graph, "MATCH ()-[r:DEPENDS_ON]->() RETURN count(r) AS c"), 1);

    // Properties and provenance are on the elements.
    let result = graph
        .run(Language::Cypher, "MATCH (a:Asset)-[r:RUNS]->(s:Asset) WHERE a.id = 'h3' RETURN a, r, s", &Map::new(), QueryOptions::default())
        .unwrap();
    let data_graph = result.graph.unwrap();
    let host = data_graph.nodes.iter().find(|n| n.properties["id"] == "h3").unwrap();
    assert_eq!(host.properties["cpu"], 4);
    assert_eq!(host.properties["khonsu_id"], "lab/h3");
    assert_eq!(host.properties["khonsu_source"], "lab");
    assert_eq!(data_graph.edges[0].properties["port"], 8003);

    // Re-sync does not duplicate anything.
    let source = settings::load_sources().into_iter().find(|s| s.id == "lab").unwrap();
    assert!(source.last_sync.as_ref().unwrap().ok);
    assert!(!source.patterns.is_empty());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    plugin::sync_source(&mut graph, &source, SyncMode::Full, &cancel, |_| {}).unwrap();
    assert_eq!(count(&mut graph, "MATCH (n:Asset) RETURN count(n) AS c"), 62);
    assert_eq!(count(&mut graph, "MATCH ()-[r:RUNS]->() RETURN count(r) AS c"), 60);

    // Rows removed from the files disappear on the next full sync.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_csvs(&data, 59, Some(10));
    let source = settings::load_sources().into_iter().find(|s| s.id == "lab").unwrap();
    let report = plugin::sync_source(&mut graph, &source, SyncMode::Full, &cancel, |_| {}).unwrap();
    assert_eq!(report.merge.stale_nodes_removed, 1, "{report:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Asset) RETURN count(n) AS c"), 61);
    assert_eq!(count(&mut graph, "MATCH ()-[r:RUNS]->() RETURN count(r) AS c"), 58);

    // A broken mapping fails the sync and is recorded on the source.
    let mut broken = source.clone();
    broken.config.insert("edges".into(), Value::String(r#"[{"path": "edges.csv", "rel_type": "X", "source_column": "nope", "target_column": "target"}]"#.into()));
    let err = plugin::sync_source(&mut graph, &broken, SyncMode::Full, &cancel, |_| {}).unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
    let saved = settings::load_sources().into_iter().find(|s| s.id == "lab").unwrap();
    assert!(!saved.last_sync.unwrap().ok);
    assert_eq!(count(&mut graph, "MATCH (n:Asset) RETURN count(n) AS c"), 61);
}

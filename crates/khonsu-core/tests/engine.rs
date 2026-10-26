mod common;

use common::TestDir;
use khonsu_core::{AccessMode, Graph, Language, OpenError, QueryOptions};
use serde_json::{json, Map};

fn run(graph: &mut Graph, language: Language, text: &str) -> khonsu_core::QueryResult {
    graph.run(language, text, &Map::new(), QueryOptions::default()).unwrap()
}

#[test]
fn create_query_and_read_back_nodes_and_relationships() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    run(&mut graph, Language::Cypher, "CREATE (:Host {name: 'web'})-[:RUNS {since: 2020}]->(:Service {name: 'api'})");

    let result = run(&mut graph, Language::Cypher, "MATCH (h:Host)-[r:RUNS]->(s:Service) RETURN h, r, s");
    assert_eq!(result.row_count, 1);
    let data = result.graph.expect("graph elements");
    assert_eq!(data.nodes.len(), 2);
    assert_eq!(data.edges.len(), 1);
    assert_eq!(data.edges[0].rel_type, "RUNS");
    assert!(data.dangling_endpoints().is_empty());

    let scalar = run(&mut graph, Language::Cypher, "MATCH (n:Host) RETURN n.name AS name");
    assert_eq!(scalar.rows, vec![vec![json!("web")]]);
    assert!(scalar.graph.is_none());

    let host = data.nodes.iter().find(|n| n.labels == ["Host"]).unwrap().id;
    let around = graph.neighbors(host, 50).unwrap();
    assert_eq!(around.nodes.len(), 2);
    assert_eq!(around.edges.len(), 1);
}

#[test]
fn cypher_columns_keep_the_return_order() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    run(&mut graph, Language::Cypher, "CREATE (:Service {name: 'cart', cpu: '200m'})");

    let result = run(&mut graph, Language::Cypher, "MATCH (s:Service) RETURN s.name AS service, s.cpu, count(s) AS n");
    assert_eq!(result.columns, ["service", "s.cpu", "n"]);
    assert_eq!(result.rows, vec![vec![json!("cart"), json!("200m"), json!(1)]]);
}

#[test]
fn parameters_truncation_and_error_positions() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    for i in 0..5 {
        let mut params = Map::new();
        params.insert("k".into(), json!(i));
        graph.run(Language::Cypher, "CREATE (:Item {k: $k})", &params, QueryOptions::default()).unwrap();
    }
    let result = graph
        .run(Language::Cypher, "MATCH (n:Item) RETURN n.k AS k", &Map::new(), QueryOptions { max_rows: 2 })
        .unwrap();
    assert_eq!(result.row_count, 5);
    assert_eq!(result.rows.len(), 2);
    assert!(result.truncated);

    let err = graph.run(Language::Cypher, "MATCH (n) RETURN n LIMIT", &Map::new(), QueryOptions::default()).unwrap_err();
    assert_eq!(err.kind, "parse");
    assert_eq!(err.line, Some(1));
    assert!(err.column.is_some());

    let err = graph.run(Language::Sql, "SELEC 1", &Map::new(), QueryOptions::default()).unwrap_err();
    assert_eq!((err.line, err.column), (Some(1), Some(1)));
}

#[test]
fn sql_tables_explain_and_schema() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    run(&mut graph, Language::Sql, "CREATE TABLE assets (id BIGINT PRIMARY KEY, name TEXT)");
    run(&mut graph, Language::Sql, "INSERT INTO assets (id, name) VALUES (1, 'db')");
    let mut params = Map::new();
    params.insert("1".into(), json!(1));
    let rows = graph.run(Language::Sql, "SELECT name FROM assets WHERE id = $1", &params, QueryOptions::default()).unwrap();
    assert_eq!(rows.rows, vec![vec![json!("db")]]);

    run(&mut graph, Language::Cypher, "CREATE (:Host {name: 'a'})-[:RUNS {port: 1}]->(:Service {name: 'b'})");
    run(&mut graph, Language::Cypher, "CREATE (:Host {name: 'c', cpu: 4})");

    let plan = graph.explain(Language::Cypher, "MATCH (n:Host) RETURN n").unwrap();
    assert!(plan.contains("label:Host"), "{plan}");
    let explained = run(&mut graph, Language::Cypher, "EXPLAIN MATCH (n:Host) RETURN n");
    assert_eq!(explained.columns, ["plan"]);

    let schema = graph.schema().unwrap();
    let host = schema.labels.iter().find(|l| l.name == "Host").expect("Host label");
    assert_eq!(host.count, 2);
    assert!(host.properties.contains(&"cpu".to_string()), "{schema:?}");
    let runs = schema.relationship_types.iter().find(|t| t.name == "RUNS").expect("RUNS type");
    assert_eq!(runs.count, 1);
    let assets = schema.tables.iter().find(|t| t.name == "assets").expect("assets table");
    assert_eq!(assets.columns, ["id", "name"]);
}

#[test]
fn writer_lock_and_read_only_access() {
    let dir = TestDir::new();
    let mut writer = Graph::create(dir.graph(), "KhonsuGraph").unwrap();
    run(&mut writer, Language::Cypher, "CREATE (:Host {name: 'a'})");

    match Graph::open(dir.graph(), AccessMode::ReadWrite, "khonsu-cli") {
        Err(OpenError::Locked(Some(owner))) => assert_eq!(owner.program, "KhonsuGraph"),
        other => panic!("expected a lock error, got {other:?}"),
    }
    let message = Graph::open(dir.graph(), AccessMode::ReadWrite, "khonsu-cli").unwrap_err().to_string();
    assert!(message.contains("KhonsuGraph") && message.contains("one process"), "{message}");
    assert!(matches!(Graph::open(dir.graph(), AccessMode::ReadOnly, "khonsu-cli"), Err(OpenError::Locked(_))));

    drop(writer);
    let mut reader = Graph::open(dir.graph(), AccessMode::ReadOnly, "khonsu-cli").unwrap();
    assert_eq!(run(&mut reader, Language::Cypher, "MATCH (n) RETURN n").row_count, 1);
    let refused = reader
        .run(Language::Cypher, "CREATE (:Host {name: 'b'})", &Map::new(), QueryOptions::default())
        .unwrap_err();
    assert_eq!(refused.kind, "read_only", "{refused:?}");
    drop(reader);
    Graph::open(dir.graph(), AccessMode::ReadWrite, "khonsu-cli").unwrap();
}

#[test]
fn open_rejects_missing_and_non_graph_directories() {
    let dir = TestDir::new();
    assert!(matches!(Graph::open(dir.graph(), AccessMode::ReadOnly, "t"), Err(OpenError::Missing(_))));
    std::fs::create_dir_all(dir.graph()).unwrap();
    std::fs::write(dir.graph().join("notes.txt"), "x").unwrap();
    assert!(matches!(Graph::open(dir.graph(), AccessMode::ReadOnly, "t"), Err(OpenError::NotAGraph(_))));
    assert!(matches!(Graph::create(dir.graph(), "t"), Err(OpenError::NotEmpty(_))));
}

#[test]
fn reopen_keeps_data() {
    let dir = TestDir::new();
    {
        let mut graph = Graph::create(dir.graph(), "t").unwrap();
        run(&mut graph, Language::Cypher, "CREATE (:Host {name: 'kept'})");
    }
    let mut graph = Graph::open(dir.graph(), AccessMode::ReadWrite, "t").unwrap();
    let result = run(&mut graph, Language::Cypher, "MATCH (n:Host) RETURN n.name AS name");
    assert_eq!(result.rows, vec![vec![json!("kept")]]);
}

#[test]
fn scalar_in_and_json_values_survive_reopen() {
    let dir = TestDir::new();
    {
        let mut graph = Graph::create(dir.graph(), "test").unwrap();
        let params = Map::from_iter([("metadata".into(), json!(r#"{"source":"audit","count":3,"flag":true}"#))]);
        graph.run(
            Language::Cypher,
            "CREATE (:Record {metadata: $metadata})",
            &params,
            QueryOptions::default(),
        ).unwrap();
    }
    let mut graph = Graph::open(dir.graph(), AccessMode::ReadOnly, "test").unwrap();
    let params = Map::from_iter([("sources".into(), json!(["audit"]))]);
    let result = graph.run(
        Language::Cypher,
        "MATCH (n:Record) WHERE json_value(n.metadata, '$.source') IN $sources RETURN json_value(n.metadata, '$.count') AS count, json_value(n.metadata, '$.flag') AS flag, json_value(n.metadata, '$.missing') AS missing",
        &params,
        QueryOptions::default(),
    ).unwrap();
    assert_eq!(result.columns, ["count", "flag", "missing"]);
    assert_eq!(result.rows, vec![vec![json!(3), json!(true), json!(null)]]);
}

#[test]
fn vector_results_are_json_arrays_after_reopen() {
    let dir = TestDir::new();
    {
        let mut database = hawdb::Database::open(dir.graph()).unwrap();
        let vector = hawdb::Value::Vector(hawdb::Vector::new(&[1.25, -2.5, 0.1]).unwrap());
        database.sql_session().query(
            "CREATE TABLE embeddings (id BIGINT PRIMARY KEY, embedding VECTOR(3))", &[],
        ).unwrap();
        database.sql_session().query(
            "INSERT INTO embeddings (id, embedding) VALUES (1, $1)", &[vector.clone()],
        ).unwrap();
        database.query_with_params(
            "CREATE (:Embedding {embedding: $vector})",
            &std::collections::BTreeMap::from([("vector".into(), vector)]),
        ).unwrap();
    }
    let mut graph = Graph::open(dir.graph(), AccessMode::ReadOnly, "test").unwrap();
    let expected = json!([1.25, -2.5, 0.1]);
    let sql = run(&mut graph, Language::Sql, "SELECT embedding FROM embeddings");
    assert_eq!(sql.rows, vec![vec![expected.clone()]]);
    let cypher = run(&mut graph, Language::Cypher, "MATCH (n:Embedding) RETURN n.embedding AS embedding, n");
    assert_eq!(cypher.rows[0][0], expected);
    assert_eq!(cypher.graph.unwrap().nodes[0].properties["embedding"], expected);
}

#[test]
fn read_row_ceiling_is_enforced_before_display_truncation() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    // Use bounded write batches so fixture creation does not hit the read cap.
    for start in (0..100_001).step_by(4096) {
        let items: Vec<_> = (start..(start + 4096).min(100_001)).map(|n| json!({"n": n})).collect();
        let params = Map::from_iter([("items".into(), json!(items))]);
        graph.run(
            Language::Cypher,
            "UNWIND $items AS item CREATE (:BudgetItem {n: item.n})",
            &params,
            QueryOptions::default(),
        ).unwrap();
    }
    let error = graph.run(
        Language::Cypher,
        "MATCH (n:BudgetItem) RETURN n.n AS value",
        &Map::new(),
        QueryOptions { max_rows: 2 },
    ).unwrap_err();
    assert_eq!(error.kind, "read_budget", "{error}");
    assert!(error.message.contains("100000"), "{error}");
}

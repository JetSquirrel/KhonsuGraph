mod common;

use common::TestDir;
use khonsu_core::analysis::{self, LoadSpec};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{json, Map};

fn run(graph: &mut Graph, text: &str) -> khonsu_core::QueryResult {
    graph.run(Language::Cypher, text, &Map::new(), QueryOptions { max_rows: 1000 }).unwrap()
}

/// Two triangles joined by one relationship, as people and teams, plus a
/// node on its own.
fn two_triangles(graph: &mut Graph) {
    for (label, name) in [("Person", "a"), ("Person", "b"), ("Person", "c"), ("Team", "x"), ("Team", "y"), ("Team", "z"), ("Person", "alone")] {
        run(graph, &format!("CREATE (:{label} {{name: '{name}'}})"));
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "a"), ("x", "y"), ("y", "z"), ("z", "x"), ("c", "x")] {
        run(graph, &format!("MATCH (s {{name: '{from}'}}), (t {{name: '{to}'}}) CREATE (s)-[:KNOWS]->(t)"));
    }
}

#[test]
fn whole_graph_labels_and_types_load_as_networkx_would_build_them() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    two_triangles(&mut graph);

    let whole = analysis::load(&mut graph, &LoadSpec::default()).unwrap();
    assert_eq!((whole.node_count(), whole.edge_count()), (7, 7));

    let people = analysis::load(&mut graph, &LoadSpec { labels: vec!["Person".into()], ..LoadSpec::default() }).unwrap();
    assert_eq!((people.node_count(), people.edge_count()), (4, 3));

    // A type alone keeps only the nodes it joins: "alone" is not one.
    let knows = analysis::load(&mut graph, &LoadSpec { types: vec!["KNOWS".into()], ..LoadSpec::default() }).unwrap();
    assert_eq!((knows.node_count(), knows.edge_count()), (6, 7));

    let subgraph = analysis::load(
        &mut graph,
        &LoadSpec {
            cypher: Some("MATCH (a:Team)-[r:KNOWS]->(b:Team) RETURN a, r, b".into()),
            ..LoadSpec::default()
        },
    )
    .unwrap();
    assert_eq!((subgraph.node_count(), subgraph.edge_count()), (3, 3));
}

#[test]
fn communities_are_written_back_as_a_node_property() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    two_triangles(&mut graph);

    let net = analysis::load(&mut graph, &LoadSpec::default()).unwrap();
    let (partition, _) = analysis::louvain(&net, 1.0, 42);
    let values: Vec<_> = partition.of.iter().map(|&c| json!(c)).collect();
    assert_eq!(analysis::write_back(&mut graph, &net, "community", &values).unwrap(), 7);

    let rows = run(&mut graph, "MATCH (n) RETURN n.name AS name, n.community AS community ORDER BY name").rows;
    let community = |name: &str| rows.iter().find(|r| r[0] == json!(name)).map(|r| r[1].clone()).unwrap();
    // Each triangle is one community, and the two differ.
    assert_eq!(community("a"), community("b"));
    assert_eq!(community("b"), community("c"));
    assert_eq!(community("x"), community("z"));
    assert_ne!(community("a"), community("x"));
    assert!(community("alone").is_number());
}

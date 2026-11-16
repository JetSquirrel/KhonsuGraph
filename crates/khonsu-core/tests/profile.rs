mod common;

use common::TestDir;
use khonsu_core::analysis::{load_profiles, LoadSpec};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::Map;

/// Profiles read from a real graph: labels come from the nodes, types from
/// the relationships, and `--type` and `--cypher` narrow what is counted.
#[test]
fn profiles_are_read_from_a_graph() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let mut write = |text: &str| {
        graph
            .run(Language::Cypher, text, &Map::new(), QueryOptions::default())
            .unwrap()
    };
    write("CREATE (:Host {name: 'web'})-[:RAN]->(:Process {name: 'sh'})");
    write("CREATE (:Ip {name: '10.0.0.9'})");
    for _ in 0..2 {
        write("MATCH (h:Host {name: 'web'}), (i:Ip {name: '10.0.0.9'}) CREATE (h)-[:CONNECTED_TO]->(i)");
    }
    write("CREATE (:Host {name: 'idle'})");

    let id = |graph: &mut Graph, name: &str| {
        let text = format!("MATCH (n {{name: '{name}'}}) RETURN id(n) AS id");
        graph
            .run(
                Language::Cypher,
                &text,
                &Map::new(),
                QueryOptions::default(),
            )
            .unwrap()
            .rows[0][0]
            .as_i64()
            .unwrap()
    };
    let (web, ip, idle) = (
        id(&mut graph, "web"),
        id(&mut graph, "10.0.0.9"),
        id(&mut graph, "idle"),
    );

    let all = load_profiles(&mut graph, &LoadSpec::default()).unwrap();
    assert_eq!(
        (
            all.node_count(),
            all.relationship_count(),
            all.feature_count()
        ),
        (4, 3, 4)
    );
    let web_ix = all.index_of(web).unwrap();
    assert_eq!(all.labels(web_ix), "Host");
    assert_eq!(
        all.profile(web_ix),
        vec![
            ("out:CONNECTED_TO:Ip".to_string(), 2),
            ("out:RAN:Process".to_string(), 1)
        ]
    );
    assert_eq!(
        all.profile(all.index_of(ip).unwrap()),
        vec![("in:CONNECTED_TO:Host".to_string(), 2)]
    );
    assert!(all.profile(all.index_of(idle).unwrap()).is_empty());

    let ran = load_profiles(
        &mut graph,
        &LoadSpec {
            types: vec!["RAN".into()],
            ..LoadSpec::default()
        },
    )
    .unwrap();
    assert_eq!(
        ran.profile(ran.index_of(web).unwrap()),
        vec![("out:RAN:Process".to_string(), 1)]
    );

    let query = "MATCH (h:Host)-[r:CONNECTED_TO]->(i:Ip) RETURN h, r, i";
    let sub = load_profiles(
        &mut graph,
        &LoadSpec {
            cypher: Some(query.into()),
            ..LoadSpec::default()
        },
    )
    .unwrap();
    assert_eq!((sub.node_count(), sub.relationship_count()), (2, 2));
    assert_eq!(
        sub.profile(sub.index_of(web).unwrap()),
        vec![("out:CONNECTED_TO:Ip".to_string(), 2)]
    );
}

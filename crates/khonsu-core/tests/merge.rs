mod common;

use common::TestDir;
use khonsu_core::merge::{EdgePattern, MergeStats, Merger};
use khonsu_core::protocol::{EdgeRecord, NodeRecord};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{json, Map};

fn count(graph: &mut Graph, query: &str) -> i64 {
    let result = graph.run(Language::Cypher, query, &Map::new(), QueryOptions::default()).unwrap();
    result.rows[0][0].as_i64().unwrap()
}

/// `players` Player nodes and one ON_TEAM relationship each, to two teams.
fn sync(
    graph: &mut Graph,
    source: &str,
    synced_at: i64,
    players: usize,
    jersey: impl Fn(usize) -> i64,
    known: &[EdgePattern],
) -> (MergeStats, Vec<EdgePattern>) {
    // A page of 3 rows makes every read-back cross page bounds.
    let mut merger = Merger::new(graph, source, "test@0", synced_at).with_page_size(3);
    for team in ["KC", "DET"] {
        let node = NodeRecord {
            id: team.into(),
            label: "Team".into(),
            properties: Map::new(),
        };
        merger.add_node(node).unwrap();
    }
    for i in 0..players {
        let properties = json!({ "name": format!("player {i}") }).as_object().unwrap().clone();
        merger.add_node(NodeRecord { id: format!("p{i}"), label: "Player".into(), properties }).unwrap();
        merger.add_edge(EdgeRecord {
            id: format!("p{i}-team"),
            source: format!("p{i}"),
            target: if i % 2 == 0 { "KC" } else { "DET" }.into(),
            rel_type: "ON_TEAM".into(),
            properties: json!({ "jersey": jersey(i) }).as_object().unwrap().clone(),
        });
    }
    merger.finish(true, known).unwrap()
}

#[test]
fn identical_repeats_are_written_once_and_a_changed_repeat_wins() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let play = |down: i64| NodeRecord {
        id: "2023:56".into(),
        label: "Play".into(),
        properties: json!({ "down": down }).as_object().unwrap().clone(),
    };
    let mut merger = Merger::new(&mut graph, "nfl", "test@0", 1);
    for down in [1, 1, 1, 2] {
        merger.add_node(play(down)).unwrap();
    }
    let (stats, _) = merger.finish(true, &[]).unwrap();
    assert_eq!((stats.nodes_written, stats.duplicate_nodes), (2, 3), "{stats:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Play) RETURN count(n) AS c"), 1);
    assert_eq!(count(&mut graph, "MATCH (n:Play) RETURN max(n.down) AS c"), 2);
}

#[test]
fn resync_reads_back_in_pages_and_only_rewrites_changes() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();

    // Another source's relationship on the same pattern is never touched.
    let (other, _) = sync(&mut graph, "other", 1, 1, |_| 99, &[]);
    assert_eq!(other.edges_written, 1, "{other:?}");

    let (first, patterns) = sync(&mut graph, "nfl", 10, 10, |i| i as i64, &[]);
    assert_eq!((first.nodes_written, first.edges_written), (12, 10));
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);

    let (same, _) = sync(&mut graph, "nfl", 11, 10, |i| i as i64, &patterns);
    assert_eq!((same.unchanged_edges, same.edges_written), (10, 0), "{same:?}");

    let (changed, _) = sync(&mut graph, "nfl", 12, 9, |i| if i == 4 { 40 } else { i as i64 }, &patterns);
    assert_eq!(changed.unchanged_edges, 8, "{changed:?}");
    assert_eq!(changed.edges_written, 1);
    assert_eq!(changed.stale_edges_removed, 1);
    assert_eq!(changed.stale_nodes_removed, 1);

    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() RETURN count(r) AS c"), 10);
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() WHERE r.khonsu_source = 'nfl' RETURN count(r) AS c"), 9);
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() WHERE r.jersey = 40 RETURN count(r) AS c"), 1);
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() WHERE r.jersey = 99 RETURN count(r) AS c"), 1);
}

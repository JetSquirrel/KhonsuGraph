mod common;

use common::TestDir;
use khonsu_core::merge::{next_sync_stamp, EdgePattern, MergeStats, Merger};
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

fn stamp(graph: &mut Graph, id: &str) -> i64 {
    count(graph, &format!("MATCH (n {{khonsu_id: 'lab/{id}'}}) RETURN n.khonsu_synced_at AS c"))
}

/// One Host node per entry, by id and `state`.
fn sync_hosts(graph: &mut Graph, synced_at: i64, hosts: &[(&str, &str)]) -> MergeStats {
    let mut merger = Merger::new(graph, "lab", "test@0", synced_at);
    for (id, state) in hosts {
        let properties = json!({ "state": state }).as_object().unwrap().clone();
        merger.add_node(NodeRecord { id: (*id).into(), label: "Host".into(), properties }).unwrap();
    }
    merger.finish(true, &[]).unwrap().0
}

/// Rewriting every node on every sync made the graph grow by its own size each
/// time, and later reads slower.
#[test]
fn an_unchanged_node_is_not_written_again() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let first = sync_hosts(&mut graph, 1, &[("a", "up"), ("b", "up")]);
    assert_eq!((first.nodes_written, first.unchanged_nodes), (2, 0), "{first:?}");

    let same = sync_hosts(&mut graph, 2, &[("a", "up"), ("b", "up")]);
    assert_eq!((same.nodes_written, same.unchanged_nodes, same.stale_nodes_removed), (0, 2, 0), "{same:?}");
    assert_eq!(stamp(&mut graph, "a"), 1, "left alone");

    let changed = sync_hosts(&mut graph, 3, &[("a", "down"), ("b", "up")]);
    assert_eq!((changed.nodes_written, changed.unchanged_nodes), (1, 1), "{changed:?}");
    assert_eq!((stamp(&mut graph, "a"), stamp(&mut graph, "b")), (3, 1));
    assert_eq!(count(&mut graph, "MATCH (n:Host) WHERE n.state = 'down' RETURN count(n) AS c"), 1);
}

/// Being left alone must not read as having gone, nor the reverse.
#[test]
fn only_nodes_the_sync_did_not_see_are_removed() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    sync_hosts(&mut graph, 1, &[("a", "up"), ("b", "up"), ("c", "up")]);
    sync_hosts(&mut graph, 2, &[("a", "up"), ("b", "up"), ("c", "up")]);

    let fewer = sync_hosts(&mut graph, 3, &[("a", "up")]);
    assert_eq!((fewer.unchanged_nodes, fewer.stale_nodes_removed), (1, 2), "{fewer:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Host) RETURN count(n) AS c"), 1);

    // Another source's node with the same record ID is not this source's to remove.
    let mut other = Merger::new(&mut graph, "other", "test@0", 4);
    other.add_node(NodeRecord { id: "a".into(), label: "Host".into(), properties: Map::new() }).unwrap();
    other.finish(true, &[]).unwrap();
    let none = sync_hosts(&mut graph, 5, &[]);
    assert_eq!(none.stale_nodes_removed, 1, "{none:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Host {khonsu_source: 'other'}) RETURN count(n) AS c"), 1);
}

/// Compared with what the graph holds by the time it's reached, not when the sync began.
#[test]
fn a_repeat_returning_to_the_stored_value_is_still_written() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    sync_hosts(&mut graph, 1, &[("a", "up")]);
    let stats = sync_hosts(&mut graph, 2, &[("a", "down"), ("a", "up")]);
    assert_eq!(stats.nodes_written, 2, "{stats:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Host) WHERE n.state = 'up' RETURN count(n) AS c"), 1);
}

/// A graph from before nodes had a hash: every node is written once more, then left alone.
#[test]
fn a_node_with_no_hash_is_written_once() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    graph
        .run(
            Language::Cypher,
            "CREATE (:Host {khonsu_id: 'lab/a', khonsu_source: 'lab', khonsu_synced_at: 1, state: 'up'})",
            &Map::new(),
            QueryOptions::default(),
        )
        .unwrap();
    let upgraded = sync_hosts(&mut graph, 2, &[("a", "up")]);
    assert_eq!((upgraded.nodes_written, upgraded.stale_nodes_removed), (1, 0), "{upgraded:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Host) RETURN count(n) AS c"), 1);
    let again = sync_hosts(&mut graph, 3, &[("a", "up")]);
    assert_eq!((again.nodes_written, again.unchanged_nodes), (0, 1), "{again:?}");
}

/// A source added again has no patterns on record. Its relationships still have to
/// be found, or every one is written a second time.
#[test]
fn a_source_added_again_finds_its_relationships() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let (first, _) = sync(&mut graph, "nfl", 1, 4, |i| i as i64, &[]);
    assert_eq!(first.edges_written, 4, "{first:?}");

    let (again, patterns) = sync(&mut graph, "nfl", 2, 4, |i| i as i64, &[]);
    assert_eq!((again.edges_written, again.unchanged_edges), (0, 4), "{again:?}");
    assert_eq!(patterns.len(), 1, "{patterns:?}");
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() RETURN count(r) AS c"), 4);

    // And what it no longer reports can still be removed.
    let (fewer, _) = sync(&mut graph, "nfl", 3, 3, |i| if i == 0 { 70 } else { i as i64 }, &[]);
    assert_eq!((fewer.edges_written, fewer.stale_edges_removed, fewer.stale_nodes_removed), (1, 1, 1), "{fewer:?}");
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() RETURN count(r) AS c"), 3);
    assert_eq!(count(&mut graph, "MATCH ()-[r:ON_TEAM]->() WHERE r.jersey = 70 RETURN count(r) AS c"), 1);
}

/// An update reaches the node already there, across reopens of the graph. A MERGE
/// was seen adding a second node here on a graph of 100,000.
#[test]
fn updating_a_node_across_reopens_keeps_one_node() {
    let dir = TestDir::new();
    let path = dir.graph();
    let graph = Graph::create(&path, "test");
    let mut graph = graph.unwrap();
    sync_hosts(&mut graph, 1, &[("a", "s0"), ("b", "up")]);
    drop(graph);
    for round in 1..4 {
        let mut graph = Graph::open(&path, khonsu_core::AccessMode::ReadWrite, "test").unwrap();
        let state = format!("s{round}");
        let stats = sync_hosts(&mut graph, 1 + round, &[("a", &state), ("b", "up")]);
        assert_eq!((stats.nodes_written, stats.unchanged_nodes), (1, 1), "{stats:?}");
        assert_eq!(count(&mut graph, "MATCH (n:Host) RETURN count(n) AS c"), 2, "round {round}");
        assert_eq!(count(&mut graph, "MATCH (n:Host {khonsu_id: 'lab/a'}) RETURN count(n) AS c"), 1, "round {round}");
    }
}

/// A node that comes back under another label is a new node in the graph, with the
/// same `khonsu_id` as the old one until the old one is removed. Its relationships
/// have to land on the new one, whichever label sorts first: on the old one they
/// would go with it, and the sync would still read as complete.
#[test]
fn a_node_that_changes_label_keeps_its_relationships() {
    for (before, after) in [("Alpha", "Zulu"), ("Zulu", "Alpha")] {
        let dir = TestDir::new();
        let mut graph = Graph::create(dir.graph(), "test").unwrap();
        let write = |graph: &mut Graph, label: &str, synced_at: i64, known: &[EdgePattern]| {
            let mut merger = Merger::new(graph, "lab", "test@0", synced_at);
            merger.add_node(NodeRecord { id: "team".into(), label: "Team".into(), properties: Map::new() }).unwrap();
            merger.add_node(NodeRecord { id: "p".into(), label: label.into(), properties: Map::new() }).unwrap();
            merger.add_edge(EdgeRecord {
                id: "p-team".into(),
                source: "p".into(),
                target: "team".into(),
                rel_type: "ON_TEAM".into(),
                properties: Map::new(),
            });
            merger.finish(true, known).unwrap()
        };

        let (_, patterns) = write(&mut graph, before, 1, &[]);
        let (stats, _) = write(&mut graph, after, 2, &patterns);

        assert_eq!(stats.stale_nodes_removed, 1, "{before} -> {after}: {stats:?}");
        assert_eq!(count(&mut graph, &format!("MATCH (n:{before}) RETURN count(n) AS c")), 0, "{before} -> {after}");
        assert_eq!(count(&mut graph, &format!("MATCH (n:{after}) RETURN count(n) AS c")), 1, "{before} -> {after}");
        assert_eq!(
            count(&mut graph, &format!("MATCH (:{after})-[r:ON_TEAM]->(:Team) RETURN count(r) AS c")),
            1,
            "{before} -> {after}: {stats:?}"
        );
    }
}

/// An incremental sync keeps what it did not see, but a relabelled node it did see
/// has only one current version, and an edge to it must not land on the old one.
#[test]
fn an_edge_written_after_a_relabel_lands_on_the_current_node() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let mut first = Merger::new(&mut graph, "lab", "test@0", 1);
    first.add_node(NodeRecord { id: "p".into(), label: "Zulu".into(), properties: Map::new() }).unwrap();
    first.finish(true, &[]).unwrap();

    let mut second = Merger::new(&mut graph, "lab", "test@0", 2);
    second.add_node(NodeRecord { id: "team".into(), label: "Team".into(), properties: Map::new() }).unwrap();
    second.add_node(NodeRecord { id: "p".into(), label: "Alpha".into(), properties: Map::new() }).unwrap();
    second.add_edge(EdgeRecord {
        id: "p-team".into(),
        source: "p".into(),
        target: "team".into(),
        rel_type: "ON_TEAM".into(),
        properties: Map::new(),
    });
    let (stats, _) = second.finish(false, &[]).unwrap();
    assert_eq!(stats.edges_written, 1, "{stats:?}");
    assert_eq!(count(&mut graph, "MATCH (:Alpha)-[r:ON_TEAM]->(:Team) RETURN count(r) AS c"), 1);
}

/// Stale nodes are found by comparison, so a shared or earlier stamp can't keep one.
#[test]
fn a_sync_in_the_same_second_as_the_last_still_removes_stale_nodes() {
    let dir = TestDir::new();
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let now = 1000;
    assert_eq!(next_sync_stamp(&mut graph, "nfl", now).unwrap(), now, "nothing synced yet");

    let (_, patterns) = sync(&mut graph, "nfl", now, 3, |i| i as i64, &[]);
    let stamp = next_sync_stamp(&mut graph, "nfl", now).unwrap();
    assert_eq!(stamp, now + 1);
    let (second, _) = sync(&mut graph, "nfl", stamp, 2, |i| i as i64, &patterns);
    assert_eq!(second.stale_nodes_removed, 1, "{second:?}");
    assert_eq!(count(&mut graph, "MATCH (n:Player) RETURN count(n) AS c"), 2);

    // Later time wins, and another source's stamps do not count.
    assert_eq!(next_sync_stamp(&mut graph, "nfl", 5000).unwrap(), 5000);
    assert_eq!(next_sync_stamp(&mut graph, "other", now).unwrap(), now);
}

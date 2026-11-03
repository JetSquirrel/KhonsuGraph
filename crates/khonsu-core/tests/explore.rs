mod common;

use common::TestDir;
use khonsu_core::explore::{Direction, Exploration, ExploreRequest, Limit};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{json, Map};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

/// `a -NEXT-> b -NEXT-> c -NEXT-> a` (a cycle), `b -SIDE-> d`, and `e`, alone.
struct Fixture {
    _dir: TestDir,
    graph: Graph,
    ids: BTreeMap<String, i64>,
}

impl Fixture {
    fn new() -> Self {
        let dir = TestDir::new();
        let mut graph = Graph::create(dir.graph(), "test").unwrap();
        let mut run = |text: String| {
            graph.run(Language::Cypher, &text, &Map::new(), QueryOptions::default()).unwrap();
        };
        for name in ["a", "b", "c", "d", "e"] {
            run(format!("CREATE (:N {{name: '{name}'}})"));
        }
        for (from, rel, to) in [("a", "NEXT", "b"), ("b", "NEXT", "c"), ("c", "NEXT", "a"), ("b", "SIDE", "d")] {
            run(format!("MATCH (x:N {{name: '{from}'}}), (y:N {{name: '{to}'}}) CREATE (x)-[:{rel}]->(y)"));
        }
        let out = graph
            .run(Language::Cypher, "MATCH (n:N) RETURN n.name AS name, id(n) AS id", &Map::new(), QueryOptions::default())
            .unwrap();
        let ids = out.rows.iter().map(|row| (row[0].as_str().unwrap().to_string(), row[1].as_i64().unwrap())).collect();
        Self { _dir: dir, graph, ids }
    }

    fn request(&self, starts: &[&str]) -> ExploreRequest {
        ExploreRequest::new(starts.iter().map(|name| self.ids[*name]).collect())
    }

    fn walk(&mut self, request: &ExploreRequest) -> Exploration {
        self.graph.explore(request, &AtomicBool::new(false)).unwrap()
    }
}

fn names(result: &Exploration) -> Vec<String> {
    let mut names: Vec<String> = result.graph.nodes.iter().map(|node| node.properties["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    names
}

fn explained(result: &Exploration) -> Vec<String> {
    result.hops.iter().map(|hop| result.explain(hop)).collect()
}

#[test]
fn out_follows_the_arrows_and_in_goes_against_them() {
    let mut f = Fixture::new();
    let mut request = f.request(&["b"]);
    request.max_hops = 1;

    request.direction = Direction::Out;
    let out = f.walk(&request);
    assert_eq!(names(&out), ["b", "c", "d"]);
    assert_eq!(explained(&out), ["b -[NEXT]-> c", "b -[SIDE]-> d"]);
    assert!(out.hops.iter().all(|hop| hop.along));

    request.direction = Direction::In;
    let inbound = f.walk(&request);
    assert_eq!(names(&inbound), ["a", "b"]);
    assert_eq!(explained(&inbound), ["b <-[NEXT]- a"]);
    assert!(!inbound.hops[0].along);

    request.direction = Direction::Both;
    let both = f.walk(&request);
    assert_eq!(names(&both), ["a", "b", "c", "d"]);
    assert_eq!(both.hops.len(), 3);
}

#[test]
fn a_cycle_ends_and_uses_each_relationship_once() {
    let mut f = Fixture::new();
    let mut request = f.request(&["a"]);
    request.rel_types = vec!["NEXT".into()];
    request.max_hops = 16;
    let result = f.walk(&request);

    assert_eq!(names(&result), ["a", "b", "c"]);
    assert_eq!(result.hops.len(), 3, "the closing c → a is shown once");
    assert_eq!(result.depth_reached, 3);
    assert!(result.is_complete(), "{:?}", result.limits);
}

#[test]
fn types_filter_what_is_followed() {
    let mut f = Fixture::new();
    let mut request = f.request(&["b"]);
    request.rel_types = vec!["SIDE".into()];
    assert_eq!(names(&f.walk(&request)), ["b", "d"]);

    request.rel_types = vec!["SIDE".into(), "NEXT".into()];
    request.max_hops = 1;
    assert_eq!(names(&f.walk(&request)), ["b", "c", "d"]);
}

#[test]
fn zero_matches_is_a_complete_empty_answer() {
    let mut f = Fixture::new();
    let alone = f.walk(&f.request(&["e"]));
    assert_eq!(names(&alone), ["e"]);
    assert!(alone.hops.is_empty());
    assert_eq!(alone.depth_reached, 0);
    assert!(alone.is_complete());

    let mut request = f.request(&["a"]);
    request.rel_types = vec!["NO_SUCH_TYPE".into()];
    let unknown = f.walk(&request);
    assert!(unknown.hops.is_empty());
    assert!(unknown.is_complete());
}

#[test]
fn a_start_missing_from_the_graph_is_reported() {
    let mut f = Fixture::new();
    let a = f.ids["a"];
    let mut request = ExploreRequest::new(vec![a, 987_654]);
    request.max_hops = 1;
    let result = f.walk(&request);
    assert_eq!(result.missing_starts, [987_654]);
    assert_eq!(names(&result), ["a", "b"]);
}

#[test]
fn several_starts_walk_together() {
    let mut f = Fixture::new();
    let mut request = f.request(&["a", "c"]);
    request.max_hops = 1;
    request.direction = Direction::Out;
    let result = f.walk(&request);
    assert_eq!(names(&result), ["a", "b", "c"]);
    assert_eq!(explained(&result), ["a -[NEXT]-> b", "c -[NEXT]-> a"]);
}

#[test]
fn each_cap_stops_the_walk_and_says_so() {
    let mut f = Fixture::new();

    let mut hops = f.request(&["a"]);
    hops.max_hops = 1;
    let result = f.walk(&hops);
    assert_eq!(names(&result), ["a", "b"]);
    assert_eq!(result.limits, [Limit::Hops]);
    assert!(!result.is_complete());

    let mut nodes = f.request(&["b"]);
    nodes.max_nodes = 2;
    let result = f.walk(&nodes);
    assert_eq!(result.graph.nodes.len(), 2);
    assert_eq!(result.limits, [Limit::Nodes]);

    let mut edges = f.request(&["b"]);
    edges.max_edges = 1;
    let result = f.walk(&edges);
    assert_eq!(result.graph.edges.len(), 1);
    assert_eq!(result.limits, [Limit::Edges]);

    // Exactly at the caps is not over them.
    let mut exact = f.request(&["b"]);
    exact.rel_types = vec!["SIDE".into()];
    exact.max_nodes = 2;
    exact.max_edges = 1;
    exact.max_hops = 1;
    let result = f.walk(&exact);
    assert_eq!(names(&result), ["b", "d"]);
    assert!(result.is_complete(), "{:?}", result.limits);
}

#[test]
fn requests_beyond_the_ceilings_are_refused_not_clamped() {
    let mut f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let base = f.request(&["a"]);
    for bad in [
        ExploreRequest { max_hops: 0, ..base.clone() },
        ExploreRequest { max_hops: 17, ..base.clone() },
        ExploreRequest { max_nodes: 0, ..base.clone() },
        ExploreRequest { max_nodes: 2001, ..base.clone() },
        ExploreRequest { max_edges: 0, ..base.clone() },
        ExploreRequest { max_edges: 5001, ..base.clone() },
        ExploreRequest { starts: vec![], ..base.clone() },
    ] {
        let error = f.graph.explore(&bad, &cancel).unwrap_err();
        assert_eq!(error.kind, "semantic", "{bad:?}");
    }
}

#[test]
fn a_type_name_cannot_change_the_query() {
    let mut f = Fixture::new();
    let mut request = f.request(&["a"]);
    request.rel_types = vec!["NEXT]->(m) DETACH DELETE m //".into()];
    let error = f.graph.explore(&request, &AtomicBool::new(false)).unwrap_err();
    assert!(error.message.contains("not a relationship type"), "{}", error.message);
    let count = f.graph.run(Language::Cypher, "MATCH (n:N) RETURN count(n) AS c", &Map::new(), QueryOptions::default()).unwrap();
    assert_eq!(count.rows[0][0], json!(5));
}

#[test]
fn a_cancelled_walk_says_so_and_is_not_complete() {
    let mut f = Fixture::new();
    let result = f.graph.explore(&f.request(&["a"]), &AtomicBool::new(true)).unwrap();
    assert!(result.cancelled);
    assert!(!result.is_complete());
    assert_eq!(names(&result), ["a"], "the start is known before the first query");
}

#[test]
fn the_same_walk_gives_the_same_result() {
    let mut f = Fixture::new();
    let mut request = f.request(&["b"]);
    request.direction = Direction::Both;
    assert_eq!(f.walk(&request), f.walk(&request));
}

#[test]
fn a_path_goes_from_a_start_to_its_target_in_fewest_hops() {
    let mut f = Fixture::new();
    let (a, d) = (f.ids["a"], f.ids["d"]);
    let result = f.walk(&f.request(&["a"]));

    let path = result.path_to(d).unwrap();
    let words: Vec<String> = path.iter().map(|hop| result.explain(hop)).collect();
    assert_eq!(words, ["a -[NEXT]-> b", "b -[SIDE]-> d"]);

    assert!(result.path_to(a).unwrap().is_empty(), "a start is already there, even on a cycle");
    assert!(result.path_to(f.ids["e"]).is_none(), "not reached");
}

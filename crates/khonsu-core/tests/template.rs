mod common;

use common::TestDir;
use khonsu_core::protocol::SyncMode;
use khonsu_core::report::{NoteKind, Report, ReviewState};
use khonsu_core::settings::{RunStatus, Source, SyncRun};
use khonsu_core::template::{StepStatus, Template};
use khonsu_core::{Graph, Language, QueryOptions};
use serde_json::{json, Map, Value};
use std::sync::atomic::AtomicBool;

/// `web -CALLS-> api -CALLS-> db`, written as a source named `shop` would.
/// `api` holds a password and an access key.
fn graph(dir: &TestDir) -> Graph {
    let mut graph = Graph::create(dir.graph(), "test").unwrap();
    let mut run = |text: &str| {
        graph.run(Language::Cypher, text, &Map::new(), QueryOptions::default()).unwrap();
    };
    for (name, extra) in [("web", ""), ("api", ", password: 'hunter2', deploy_key: 'AKIAIOSFODNN7EXAMPLE'"), ("db", "")] {
        run(&format!("CREATE (:Service {{name: '{name}', khonsu_id: 'shop/{name}', khonsu_source: 'shop'{extra}}})"));
    }
    for (from, to) in [("web", "api"), ("api", "db")] {
        run(&format!(
            "MATCH (x:Service {{name: '{from}'}}), (y:Service {{name: '{to}'}}) CREATE (x)-[:CALLS {{khonsu_id: 'shop/{from}>{to}', khonsu_source: 'shop'}}]->(y)"
        ));
    }
    graph
}

fn template() -> Template {
    let text = json!({
        "format": "khonsu-template",
        "version": 1,
        "id": "callers",
        "revision": "1",
        "name": "Callers",
        "question": "What calls this?",
        "claim": "What the data says calls it.",
        "requires": { "node_labels": ["Service"], "relationship_types": ["CALLS"] },
        "parameters": [{ "name": "service", "kind": "node" }],
        "steps": [
            { "id": "callers", "kind": "walk", "from": ["$service"], "direction": "in", "types": ["CALLS"] },
            { "id": "everything", "kind": "query", "cypher": "MATCH (s:Service) RETURN s AS service, s.password AS password, s.deploy_key AS deploy_key ORDER BY s.khonsu_id", "resource_column": "service" },
        ],
    });
    Template::parse(&text.to_string()).unwrap()
}

fn source(dir: &TestDir, status: RunStatus) -> Source {
    Source {
        id: "shop".into(),
        name: "shop".into(),
        plugin: "csv".into(),
        graph: dir.graph(),
        config: Map::new(),
        secret_keys: Vec::new(),
        cursor: None,
        patterns: Vec::new(),
        last_sync: None,
        runs: vec![SyncRun {
            run_id: "shop#1".into(),
            number: 1,
            mode: SyncMode::Full,
            started_at: 10,
            ended_at: 11,
            status,
            plugin: "csv@0".into(),
            received_nodes: 3,
            received_edges: 2,
            removals_applied: status == RunStatus::Complete,
            cursor_advanced: false,
            gaps: if status == RunStatus::Complete { vec![] } else { vec!["the orders table".into()] },
            message: String::new(),
        }],
    }
}

fn service(name: &str) -> Map<String, Value> {
    Map::from_iter([("service".to_string(), json!(format!("shop/{name}")))])
}

#[test]
fn a_cancelled_run_says_so_and_claims_nothing() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let run = graph.run_template(&template(), &service("db"), 200, &[], &AtomicBool::new(true)).unwrap();
    assert!(run.cancelled && !run.complete);
    assert!(run.steps.iter().all(|step| step.status == StepStatus::Cancelled), "{:?}", run.steps);
    assert!(run.findings.is_empty());
    assert!(run.gaps.iter().any(|gap| gap.contains("cancelled")), "{:?}", run.gaps);
}

#[test]
fn a_source_not_read_completely_makes_the_run_incomplete() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let whole = graph.run_template(&template(), &service("db"), 200, &[source(&dir, RunStatus::Complete)], &AtomicBool::new(false)).unwrap();
    assert!(whole.complete, "{:?}", whole.gaps);

    let partial = graph.run_template(&template(), &service("db"), 200, &[source(&dir, RunStatus::Partial)], &AtomicBool::new(false)).unwrap();
    assert!(!partial.complete);
    assert!(partial.gaps.iter().any(|gap| gap.contains("shop#1") && gap.contains("partial")), "{:?}", partial.gaps);
    // The findings themselves are the same: what differs is how far to trust an absence.
    assert_eq!(whole.finding_ids(), partial.finding_ids());

    // A source nobody configured here has unknown coverage, which is not a gap.
    let unknown = graph.run_template(&template(), &service("db"), 200, &[], &AtomicBool::new(false)).unwrap();
    assert!(unknown.complete);
}

#[test]
fn a_run_is_the_same_twice_and_its_findings_point_at_records() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let cancel = AtomicBool::new(false);
    let first = graph.run_template(&template(), &service("db"), 200, &[], &cancel).unwrap();
    let again = graph.run_template(&template(), &service("db"), 200, &[], &cancel).unwrap();
    assert_eq!(first.findings, again.findings);
    assert_eq!(first.evidence, again.evidence);

    let callers: Vec<&str> = first.findings.iter().filter(|finding| finding.step == "callers").map(|finding| finding.resource.known().unwrap().as_str()).collect();
    assert_eq!(callers, ["node:shop/api", "node:shop/web"], "nearest first");
    let web = &first.findings[1];
    let path = web.path.as_ref().unwrap();
    assert_eq!(path.hops, ["db <-[CALLS]- api", "api <-[CALLS]- web"]);
    assert_eq!(path.refs.iter().map(|hop| (hop.edge.as_str(), hop.along)).collect::<Vec<_>>(), [("rel:shop/api>db", false), ("rel:shop/web>api", false)]);
    let held: Vec<&str> = first.evidence.iter().map(|record| record.evidence_id.as_str()).collect();
    for finding in &first.findings {
        assert!(finding.evidence_ids.iter().all(|id| held.contains(&id.as_str())), "{finding:?}");
    }
}

#[test]
fn secrets_stay_out_of_a_run_a_report_and_its_markdown() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let run = graph.run_template(&template(), &service("db"), 200, &[], &AtomicBool::new(false)).unwrap();
    let api = run.findings.iter().find(|finding| finding.step == "everything" && finding.resource.known().is_some_and(|id| id == "node:shop/api")).unwrap().clone();
    assert_eq!(api.values["password"], "[redacted]");
    assert_eq!(api.values["deploy_key"], "[redacted]");
    assert_eq!(api.values["service"], "node:shop/api", "a record in a cell is named, not copied");

    let mut report = Report::new(&template(), run, "test", None);
    report.note(NoteKind::Hypothesis, "the key AKIAIOSFODNN7EXAMPLE was reused", "agent", None).unwrap();
    report.review(&api.finding_id, ReviewState::NeedsMore, "jet", Some("see -----BEGIN RSA PRIVATE KEY----- abc")).unwrap();
    for text in [serde_json::to_string(&report).unwrap(), report.to_markdown()] {
        assert!(!text.contains("hunter2"), "a password got out");
        assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "an access key got out");
        assert!(!text.contains("PRIVATE KEY"), "key material got out");
        assert!(text.contains("[redacted]"));
    }
    let record = report.run.evidence.iter().find(|record| record.evidence_id == "node:shop/api").unwrap();
    assert_eq!(record.redacted, ["deploy_key", "password"]);
}

#[test]
fn a_report_keeps_what_was_suspected_apart_from_what_was_decided() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let run = graph.run_template(&template(), &service("db"), 200, &[], &AtomicBool::new(false)).unwrap();
    let finding = run.findings[0].finding_id.clone();
    let mut report = Report::new(&template(), run, "test", None);
    assert!(report.verify().ok);
    let steps = report.actions.len();
    assert_eq!(steps, 3, "the run, and its two steps");

    report.note(NoteKind::Hypothesis, "web may have a cache", "agent", Some(&finding)).unwrap();
    report.review(&finding, ReviewState::Rejected, "jet", None).unwrap();
    report.review(&finding, ReviewState::Confirmed, "jet", Some("checked the deploy")).unwrap();
    report.note(NoteKind::Conclusion, "db is a single point of failure", "jet", None).unwrap();
    assert_eq!(report.reviews[&finding].state, ReviewState::Confirmed, "the later review stands");
    let seqs: Vec<u32> = report.actions.iter().map(|action| action.seq).collect();
    assert_eq!(seqs, (1..=7).collect::<Vec<u32>>(), "and both reviews stay in the log");
    assert!(report.actions.windows(2).all(|pair| pair[0].at <= pair[1].at));

    assert_eq!(report.review("finding:none", ReviewState::Confirmed, "jet", None).unwrap_err().kind, "not_found");
    assert_eq!(report.note(NoteKind::Conclusion, "  ", "jet", None).unwrap_err().kind, "invalid");
    assert_eq!(report.note(NoteKind::Conclusion, "text", " ", None).unwrap_err().kind, "invalid");

    let page = report.to_markdown();
    let at = |heading: &str| page.find(heading).unwrap_or_else(|| panic!("no {heading}"));
    let (conclusions, hypotheses, derived, observations) = (at("## Human conclusions"), at("## Hypotheses"), at("## Derived findings"), at("## Observations"));
    assert!(page[conclusions..hypotheses].contains("single point of failure"));
    assert!(!page[conclusions..hypotheses].contains("cache"));
    assert!(page[hypotheses..derived].contains("web may have a cache"));
    assert!(page[derived..observations].contains("**confirmed** by jet"));
    assert!(page.contains("## Action log") && page.contains("## Evidence") && page.contains("## Data gaps"));
}

#[test]
fn a_replay_on_the_same_data_agrees_and_on_changed_data_says_what_moved() {
    let dir = TestDir::new();
    let mut graph = graph(&dir);
    let cancel = AtomicBool::new(false);
    let run = graph.run_template(&template(), &service("db"), 200, &[], &cancel).unwrap();
    let report = Report::new(&template(), run, "test", None);

    let same = report.replay(&mut graph, &Map::new(), &[], &cancel).unwrap();
    assert!(same.same, "{same:?}");
    assert_eq!((same.added.len(), same.removed.len()), (0, 0));

    for text in [
        "CREATE (:Service {name: 'batch', khonsu_id: 'shop/batch', khonsu_source: 'shop'})",
        "MATCH (x:Service {name: 'batch'}), (y:Service {name: 'db'}) CREATE (x)-[:CALLS {khonsu_id: 'shop/batch>db', khonsu_source: 'shop'}]->(y)",
    ] {
        graph.run(Language::Cypher, text, &Map::new(), QueryOptions::default()).unwrap();
    }
    let moved = report.replay(&mut graph, &Map::new(), &[], &cancel).unwrap();
    assert!(!moved.same);
    assert_eq!(serde_json::to_value(moved.graph_state).unwrap(), "changed");
    assert_eq!(moved.removed.len(), 0);
    assert_eq!(moved.added.len(), 2, "a new caller, and a new row: {:?}", moved.added);
    assert_eq!(moved.unchanged, report.run.findings.len());
}

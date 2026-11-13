//! The template and report gate: the two shipped templates run offline on
//! the example data, give the same findings each time, say what stopped an
//! answer from being whole, and leave a report a reviewer can check and run
//! again. See `docs/templates.md` and `docs/reports.md`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        build_plugins();
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).unwrap();
        Self { dir: tempfile::tempdir_in(base).unwrap() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Runs `khonsu` and returns (exit code, stdout JSON).
    fn khonsu(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_khonsugraph"))
            .args(args)
            .env("KHONSU_CONFIG_DIR", self.path("config"))
            .env_remove("KHONSU_DB")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.code().unwrap_or(-1), json)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, out) = self.khonsu(args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }

    /// A graph of the CSV demo in `examples/csv`, from the source `infra`.
    fn infra(&self, name: &str, assets: &Path) -> PathBuf {
        let db = self.path(name);
        self.ok(&["init", text(&db)]);
        let table = |path: &Path, rest: Value| {
            let mut table = json!({ "path": path });
            table.as_object_mut().unwrap().extend(rest.as_object().unwrap().clone());
            json!([table]).to_string()
        };
        let nodes = format!("nodes={}", table(assets, json!({ "label_column": "kind", "id_column": "id" })));
        let links = repo().join("examples/csv/links.csv");
        let edges = format!("edges={}", table(&links, json!({ "type_column": "type", "source_column": "source", "target_column": "target", "id_column": "id" })));
        self.ok(&["source", "add", "-d", text(&db), "--plugin", "csv", "--name", "infra", "-c", &nodes, "-c", &edges]);
        self.ok(&["plugin", "sync", "--source", "infra"]);
        db
    }

    /// A graph of the AWS plugin's saved responses, from the source `shop`.
    fn aws(&self, name: &str) -> PathBuf {
        let db = self.path(name);
        self.ok(&["init", text(&db)]);
        let fixture = repo().join("plugins/aws/tests/fixture");
        self.ok(&[
            "source", "add", "-d", text(&db), "--plugin", "aws", "--name", "shop",
            "-c", &format!("fixture_dir={}", serde_json::to_string(text(&fixture)).unwrap()),
            "-c", "regions=us-east-1",
        ]);
        self.ok(&["plugin", "sync", "--source", "shop"]);
        db
    }

    /// Writes a template of one's own and returns its path.
    fn template(&self, name: &str, steps: Value) -> PathBuf {
        let path = self.path(name);
        let template = json!({
            "format": "khonsu-template",
            "version": 1,
            "id": "probe",
            "revision": "1",
            "name": "Probe",
            "question": "What happens at the edges?",
            "claim": "Nothing: it is a test.",
            "parameters": [{ "name": "service", "kind": "node", "default": "infra/svc-db" }],
            "steps": steps,
        });
        std::fs::write(&path, template.to_string()).unwrap();
        path
    }
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn assets() -> PathBuf {
    repo().join("examples/csv/assets.csv")
}

fn blast_radius() -> PathBuf {
    repo().join("examples/sre/blast-radius.template.json")
}

fn aws_exposure() -> PathBuf {
    repo().join("plugins/aws/templates/exposure.template.json")
}

/// `cargo test` builds a plugin only as a test harness; the CLI looks for the
/// real executable beside itself in target/<profile>.
fn build_plugins() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO")).args(["build", "-q", "--locked", "-p", "khonsu-plugin-aws", "-p", "khonsu-plugin-csv"]).status().unwrap();
        assert!(status.success());
    });
}

fn ids(run: &Value) -> Vec<String> {
    run["findings"].as_array().unwrap().iter().map(|finding| finding["finding_id"].as_str().unwrap().to_string()).collect()
}

fn step<'a>(run: &'a Value, id: &str) -> &'a Value {
    run["steps"].as_array().unwrap().iter().find(|step| step["id"] == id).unwrap_or_else(|| panic!("no step {id}: {run}"))
}

/// Every record a finding cites is in the run, so a reader never follows an
/// ID to nothing.
fn assert_evidence_resolves(run: &Value) {
    let held: Vec<&str> = run["evidence"].as_array().unwrap().iter().map(|record| record["evidence_id"].as_str().unwrap()).collect();
    for finding in run["findings"].as_array().unwrap() {
        let cited = finding["evidence_ids"].as_array().unwrap();
        assert!(!cited.is_empty(), "a finding with nothing behind it: {finding}");
        assert!(cited.iter().all(|id| held.contains(&id.as_str().unwrap())), "{finding}");
    }
}

#[test]
fn the_blast_radius_template_gives_the_same_findings_on_two_graphs_of_the_same_data() {
    let env = Env::new();
    // Each with settings of its own, so both have a source called `infra`.
    let other = Env::new();
    let (first, second) = (env.infra("one", &assets()), other.infra("two", &assets()));
    let check = env.ok(&["template", "check", text(&blast_radius()), "-d", text(&first)]);
    assert_eq!(check["ready"], true, "{check}");
    assert_eq!(check["graph"]["supported"], true);

    let run = |env: &Env, db: &Path| env.ok(&["template", "run", text(&blast_radius()), "-d", text(db), "-p", "service=infra/svc-db"]);
    let (a, b) = (run(&env, &first), run(&other, &second));
    assert_eq!(ids(&a), ids(&b), "finding IDs follow the data, not the graph they were read from");
    assert_eq!(a["fingerprint"]["hash"], b["fingerprint"]["hash"]);
    assert_eq!(a["complete"], true, "{}", a["gaps"]);
    assert_eq!(a["summary"], json!({ "dependents": 2, "runs_on": 2, "alerts": 1 }));
    assert_evidence_resolves(&a);

    // The walk: payments-api calls postgres, and storefront calls payments-api.
    let dependents: Vec<&Value> = a["findings"].as_array().unwrap().iter().filter(|finding| finding["step"] == "dependents").collect();
    assert_eq!(dependents[0]["resource"]["value"], "node:infra/svc-api");
    assert_eq!(dependents[1]["resource"]["value"], "node:infra/svc-web");
    assert_eq!(dependents[1]["path"]["hops"], json!(["postgres <-[DEPENDS_ON]- payments-api", "payments-api <-[DEPENDS_ON]- storefront"]));
    assert_eq!(dependents[1]["path"]["supporting"].as_array().unwrap().len(), 5);
    assert_eq!(dependents[1]["class"], "derived");
    assert_eq!(dependents[1]["verdict"]["value"], "depends_on_it");
    assert_eq!(step(&a, "dependents")["caps"], json!({ "max_hops": 4, "max_nodes": 500, "max_edges": 2000 }));

    // The queries: each row is about a record, and names the records in it.
    let alert = a["findings"].as_array().unwrap().iter().find(|finding| finding["step"] == "alerts").unwrap();
    assert_eq!(alert["class"], "observation");
    assert_eq!(alert["resource_name"]["value"], "disk 91% on db-primary");
    assert_eq!(alert["values"]["fired_on"], "rel:infra/l13");

    // A parameter narrows the walk, and its cap is reported when it cuts it.
    let near = env.ok(&["template", "run", text(&blast_radius()), "-d", text(&first), "-p", "service=infra/svc-db", "-p", "hops=1"]);
    assert_eq!(near["summary"]["dependents"], 1);
    assert_eq!(step(&near, "dependents")["limits"], json!(["hops"]));
    assert_eq!(near["complete"], false);
    assert!(near["gaps"][0].as_str().unwrap().contains("cap on hops"), "{}", near["gaps"]);
}

#[test]
fn the_aws_template_passes_on_the_offline_fixture() {
    let env = Env::new();
    let db = env.aws("aws");
    let check = env.ok(&["template", "check", text(&aws_exposure()), "-d", text(&db)]);
    assert_eq!(check["ready"], true, "{check}");
    assert_eq!(check["plugins"], json!([{ "plugin": "aws", "ready": true }]));

    let run = env.ok(&["template", "run", text(&aws_exposure()), "-d", text(&db)]);
    let again = env.ok(&["template", "run", text(&aws_exposure()), "-d", text(&db)]);
    assert_eq!(ids(&run), ids(&again));
    assert_eq!(run["complete"], true, "{}", run["gaps"]);
    assert_eq!(run["summary"], json!({ "exposure": 4, "open_rules": 1 }));
    assert_evidence_resolves(&run);

    // The plugin's findings keep its rules, verdicts, conditions and unknowns.
    let direct = run["findings"].as_array().unwrap().iter().find(|finding| finding["rule_id"] == "aws.exposure.direct-instance" && finding["resource_name"]["value"] == "web-1").unwrap();
    assert_eq!(direct["verdict"]["value"], "insufficient_data");
    assert_eq!(direct["conditions"].as_array().unwrap().len(), 4);
    assert!(!direct["unknowns"].as_array().unwrap().is_empty());
    assert_eq!(direct["path"]["hops"], json!(["Internet -[ALLOWS]-> web", "web <-[PROTECTED_BY]- web-1"]));
    assert_eq!(direct["path"]["refs"].as_array().unwrap().len(), 2);

    let open = run["findings"].as_array().unwrap().iter().find(|finding| finding["step"] == "open_rules").unwrap();
    assert_eq!(open["values"]["ports"], "443");
    assert_eq!(open["verdict"]["value"], "open_to_internet");

    // One verdict, and one resource.
    let steps = json!([{ "id": "undecided", "kind": "analysis", "plugin": "aws", "analysis": "exposure", "only_verdict": "insufficient_data" }]);
    let narrowed = env.ok(&["template", "run", text(&env.template("narrow.template.json", steps)), "-d", text(&db), "-p", "service=shop/cidr:internet"]);
    let verdicts: Vec<&str> = narrowed["findings"].as_array().unwrap().iter().map(|finding| finding["verdict"]["value"].as_str().unwrap()).collect();
    assert_eq!(verdicts, ["insufficient_data", "insufficient_data"]);

    let steps = json!([{ "id": "bad", "kind": "analysis", "plugin": "aws", "analysis": "exposure", "only_verdict": "reachable" }]);
    let (code, out) = env.khonsu(&["template", "run", text(&env.template("bad.template.json", steps)), "-d", text(&db), "-p", "service=shop/cidr:internet"]);
    assert_eq!(code, 1, "{out}");
    assert!(step(&out, "bad")["error"]["message"].as_str().unwrap().contains("no verdict"), "{out}");
}

#[test]
fn a_graph_without_what_a_template_needs_is_refused_and_a_bad_request_says_why() {
    let env = Env::new();
    let db = env.infra("infra", &assets());
    let check = env.ok(&["template", "check", text(&aws_exposure()), "-d", text(&db)]);
    assert_eq!(check["ready"], false);
    assert_eq!(check["graph"]["missing"]["node_labels"], json!(["Instance", "SecurityGroup"]));

    let (code, out) = env.khonsu(&["template", "run", text(&aws_exposure()), "-d", text(&db)]);
    assert_eq!(code, 1);
    assert_eq!(out["error"]["kind"], "unsupported_source", "an empty answer from the wrong graph would read as nothing found: {out}");
    assert_eq!(out["error"]["detail"]["missing"]["relationship_types"], json!(["PROTECTED_BY"]));

    let run = |args: &[&str]| env.khonsu(&[&["template", "run", text(&blast_radius()), "-d", text(&db)], args].concat());
    let (code, out) = run(&[]);
    assert_eq!((code, out["error"]["kind"].as_str()), (2, Some("usage")), "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("-p service="));
    let (code, out) = run(&["-p", "service=infra/nobody"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
    let (code, out) = run(&["-p", "service=infra/svc-db", "-p", "depth=2"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (2, Some("usage")), "{out}");
    assert!(out["error"]["message"].as_str().unwrap().contains("it takes service, hops"));
    let (code, out) = run(&["-p", "service=infra/svc-db", "-p", "hops=many"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (2, Some("usage")), "{out}");

    let (code, out) = env.khonsu(&["template", "run", "no-such-template", "-d", text(&db)]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
    std::fs::write(env.path("broken.template.json"), "{\"format\": \"khonsu-template\", \"version\": 1}").unwrap();
    let (code, out) = env.khonsu(&["template", "check", text(&env.path("broken.template.json"))]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("invalid")), "{out}");

    // Templates are found by ID in a folder, and a broken one is listed, not hidden.
    std::fs::copy(blast_radius(), env.path("blast-radius.template.json")).unwrap();
    let listed = env.ok(&["template", "list", "--dir", text(env.dir.path())]);
    let found: Vec<(Option<&str>, bool)> = listed["templates"].as_array().unwrap().iter().map(|listed| (listed["template"]["id"].as_str(), listed["error"].is_string())).collect();
    assert_eq!(found, [(Some("blast-radius"), false), (None, true)]);
    let by_id = env.ok(&["template", "run", "blast-radius", "--dir", text(env.dir.path()), "-d", text(&db), "-p", "service=infra/svc-db"]);
    assert_eq!(by_id["summary"]["dependents"], 2);
}

#[test]
fn a_step_that_cannot_run_is_reported_and_the_rest_still_run_and_nothing_is_written() {
    let env = Env::new();
    let db = env.infra("infra", &assets());
    let count = || env.ok(&["query", "-d", text(&db), "--cypher", "MATCH (n) RETURN count(n) AS c"])["rows"][0][0].as_i64().unwrap();
    let before = count();
    let steps = json!([
        { "id": "writes", "kind": "query", "cypher": "MATCH (s {khonsu_id: $service}) SET s.pwned = true RETURN s" },
        { "id": "unsupported", "kind": "query", "cypher": "MATCH p = (a:Service)-[:DEPENDS_ON|RUNS*..6]->(b) RETURN p" },
        { "id": "no_column", "kind": "query", "cypher": "MATCH (s:Service) RETURN s.name AS name", "resource_column": "service" },
        { "id": "one_row", "kind": "query", "cypher": "MATCH (s:Service) RETURN s AS service ORDER BY s.khonsu_id", "max_rows": 1, "resource_column": "service" },
        { "id": "works", "kind": "walk", "from": ["$service"], "direction": "in", "types": ["DEPENDS_ON"] },
    ]);
    let (code, run) = env.khonsu(&["template", "run", text(&env.template("probe.template.json", steps)), "-d", text(&db)]);
    assert_eq!(code, 1, "a failed step must not look like success to a script: {run}");
    assert_eq!(run["failed"], json!(["writes", "unsupported", "no_column"]));
    assert_eq!(run["complete"], false);

    assert_eq!(step(&run, "writes")["status"], "failed");
    assert_eq!(step(&run, "writes")["error"]["kind"], "read_only", "the engine refuses the write: {run}");
    assert_eq!(step(&run, "unsupported")["status"], "failed");
    assert!(step(&run, "no_column")["error"]["message"].as_str().unwrap().contains("\"service\""));
    assert_eq!(step(&run, "one_row")["limits"], json!(["rows"]));
    assert_eq!(step(&run, "one_row")["findings"], 1);
    assert_eq!(step(&run, "works")["status"], "ok");
    assert_eq!(step(&run, "works")["findings"], 2);
    let gaps: Vec<&str> = run["gaps"].as_array().unwrap().iter().map(|gap| gap.as_str().unwrap()).collect();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    assert!(gaps[0].contains("missing, not none"));

    assert_eq!(count(), before);
    let pwned = env.ok(&["query", "-d", text(&db), "--cypher", "MATCH (s) WHERE s.pwned = true RETURN count(s) AS c"]);
    assert_eq!(pwned["rows"][0][0], 0);

    // The cap on findings counts what it cut.
    let capped = env.ok(&["template", "run", text(&blast_radius()), "-d", text(&db), "-p", "service=infra/svc-db", "--max-findings", "2"]);
    assert_eq!((capped["findings"].as_array().unwrap().len(), capped["truncated"].as_u64()), (2, Some(3)));
    assert_eq!(capped["complete"], false);
    assert_evidence_resolves(&capped);
}

#[test]
fn a_report_verifies_reviews_exports_and_replays() {
    let env = Env::new();
    let db = env.infra("infra", &assets());
    let (report, snapshot, page, handoff) = (env.path("db.report.json"), env.path("db.snapshot.json"), env.path("db.md"), env.path("web.investigation"));
    let run = env.ok(&[
        "template", "run", text(&blast_radius()), "-d", text(&db), "-p", "service=infra/svc-db",
        "--report", text(&report), "--snapshot", text(&snapshot),
    ]);
    let result_id = run["report"]["result_id"].as_str().unwrap().to_string();
    assert!(run["report"]["snapshot"]["snapshot_id"].as_str().unwrap().starts_with("snapshot:"));
    let (code, out) = env.khonsu(&["template", "run", text(&blast_radius()), "-d", text(&db), "-p", "service=infra/svc-db", "--report", text(&report)]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("exists")), "{out}");

    let checked = env.ok(&["report", "verify", text(&report)]);
    assert_eq!((checked["ok"].as_bool(), checked["findings"].as_u64()), (Some(true), Some(5)), "{checked}");
    assert_eq!(checked["result_id"], result_id.as_str());

    // People and agents add to it, and the file still verifies.
    let storefront = run["findings"][1]["finding_id"].as_str().unwrap();
    env.ok(&["report", "note", text(&report), "--hypothesis", "redis may hide this", "--by", "agent", "--finding", storefront]);
    env.ok(&["report", "review", text(&report), "--finding", storefront, "--state", "confirmed", "--by", "jet", "--note", "no fallback"]);
    env.ok(&["report", "note", text(&report), "--conclusion", "postgres failing takes the storefront down", "--by", "jet"]);
    let (code, out) = env.khonsu(&["report", "review", text(&report), "--finding", "finding:none", "--state", "rejected", "--by", "jet"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
    let checked = env.ok(&["report", "verify", text(&report)]);
    assert_eq!((checked["ok"].as_bool(), checked["reviewed"].as_u64()), (Some(true), Some(1)), "{checked}");
    assert_eq!(checked["result_id"], result_id.as_str(), "a review changes the file, not the result");

    // On unchanged data the run comes back the same.
    let replay = env.ok(&["report", "replay", text(&report)]);
    assert_eq!((replay["same"].as_bool(), replay["graph_state"].as_str()), (Some(true), Some("unchanged")), "{replay}");
    assert_eq!(replay["result_id"], result_id.as_str());
    assert_eq!(env.ok(&["snapshot", "check", text(&snapshot), "-d", text(&db)])["graph_state"], "unchanged");

    // For a person: Markdown. For the window: the finding's path, without an LLM anywhere.
    let exported = env.ok(&["report", "export", text(&report), "--markdown", text(&page), "--investigation", text(&handoff), "--finding", storefront]);
    assert_eq!(exported["verified"], true);
    let markdown = std::fs::read_to_string(&page).unwrap();
    for part in ["# Investigation report: Blast radius of a service", "## Data gaps", "## Human conclusions", "## Hypotheses", "## Derived findings", "## Observations", "## Evidence", "## Action log", &result_id, "**confirmed** by jet", "redis may hide this"] {
        assert!(markdown.contains(part), "the page lacks {part:?}");
    }
    let reopened = env.ok(&["investigation", "reopen", text(&handoff)]);
    assert_eq!((reopened["graph_state"].as_str(), reopened["path_same"].as_bool()), (Some("unchanged"), Some(true)), "{reopened}");
    assert_eq!(reopened["walk"]["hops"].as_array().unwrap().len(), 2);
    let (code, out) = env.khonsu(&["report", "export", text(&report), "--investigation", text(&env.path("row.investigation")), "--finding", run["findings"][2]["finding_id"].as_str().unwrap()]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("invalid")), "a row has no path: {out}");

    // The source changes: storefront no longer runs anywhere, and gains nothing.
    let edited = env.path("assets.csv");
    let rows = std::fs::read_to_string(assets()).unwrap().replace("svc-web,Service,storefront,prod,web,\n", "");
    std::fs::write(&edited, rows).unwrap();
    let later = Env::new();
    let changed = later.infra("changed", &edited);
    let replay = later.ok(&["report", "replay", text(&report), "-d", text(&changed)]);
    assert_eq!((replay["same"].as_bool(), replay["graph_state"].as_str()), (Some(false), Some("changed")), "{replay}");
    assert_eq!(replay["removed"], json!([storefront]));
    assert_eq!(replay["added"], json!([]));
    assert_eq!(replay["unchanged"], 4);

    // A file changed by hand is caught, and is not built on.
    let tampered = std::fs::read_to_string(&report).unwrap().replace("\"confirmed\"", "\"rejected\"");
    std::fs::write(&report, tampered).unwrap();
    let (code, checked) = env.khonsu(&["report", "verify", text(&report)]);
    assert_eq!((code, checked["ok"].as_bool()), (1, Some(false)), "{checked}");
    let (code, out) = env.khonsu(&["report", "note", text(&report), "--conclusion", "fine", "--by", "jet"]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("invalid")), "{out}");
    let (code, out) = env.khonsu(&["report", "verify", text(&env.path("none.report.json"))]);
    assert_eq!((code, out["error"]["kind"].as_str()), (1, Some("not_found")), "{out}");
}

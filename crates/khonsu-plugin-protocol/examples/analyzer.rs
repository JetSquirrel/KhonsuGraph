//! A small analyzer with nothing to do with any cloud provider: it shows the
//! whole of what a plugin needs to offer an analysis, and its other analyses
//! misbehave in the ways a host must survive. Tests install it as a plugin.
//!
//! `dead-ends` says which `Thing` nodes have no way onward along `LINKS`.

use khonsu_plugin_protocol::{
    fnv1a, read_analyze_request, AnalysisInfo, AnalyzeRequest, ConditionRecord, ConditionStatus, Describe, Emitter, FieldRef, FindingRecord, HopRef, Message, PathRef, RuleInfo, VerdictInfo, VerdictTone,
};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::process::ExitCode;

const RULE: &str = "demo.dead-end";

fn analysis(id: &str) -> AnalysisInfo {
    AnalysisInfo {
        id: id.into(),
        title: "Dead ends".into(),
        description: "Things with no link onward.".into(),
        claim: "Only says what the links in the graph say.".into(),
        subject_labels: vec!["Thing".into()],
        node_labels: vec!["Thing".into()],
        relationship_types: vec!["LINKS".into()],
        verdicts: vec![
            VerdictInfo { id: "dead_end".into(), title: "Dead end".into(), tone: VerdictTone::Attention },
            VerdictInfo { id: "continues".into(), title: "Continues".into(), tone: VerdictTone::Quiet },
        ],
        rules: vec![RuleInfo { rule_id: RULE.into(), title: "No link onward".into(), statement: "A thing with nothing linked after it.".into(), assumptions: vec!["Only LINKS is read.".into()] }],
        translations: BTreeMap::from([("zh".to_string(), BTreeMap::from([("verdict.dead_end".to_string(), "死路".to_string())]))]),
    }
}

fn dead_ends(request: &AnalyzeRequest) -> Vec<FindingRecord> {
    let mut findings = Vec::new();
    for node in request.nodes.iter().filter(|node| node.labels.iter().any(|label| label == "Thing")) {
        let onward = request.edges.iter().any(|edge| edge.from == node.reference);
        let arriving = request.edges.iter().find(|edge| edge.to == node.reference);
        findings.push(FindingRecord {
            finding_id: format!("finding:{:016x}", fnv1a(&node.reference)),
            rule_id: RULE.into(),
            verdict: if onward { "continues" } else { "dead_end" }.into(),
            resource: node.reference.clone(),
            conditions: vec![ConditionRecord {
                id: "no-link-onward".into(),
                statement: "Nothing is linked after it.".into(),
                status: if onward { ConditionStatus::NotMet } else { ConditionStatus::Met },
                detail: format!("{} {} onward.", node.name, if onward { "links" } else { "does not link" }),
                fields: vec![FieldRef { record: node.reference.clone(), field: "name".into(), source_field: "name".into() }],
            }],
            unknowns: vec![],
            assumptions: vec!["Only LINKS is read.".into()],
            path: arriving.map(|edge| PathRef { derivation: "the link that arrives here".into(), hops: vec![HopRef { edge: edge.reference.clone(), along: true }] }),
            depends_on: None,
            records: vec![],
        });
    }
    findings
}

fn analyze(out: &mut Emitter<io::BufWriter<io::Stdout>>) -> Result<(), String> {
    let request = read_analyze_request().map_err(|e| e.to_string())?;
    let io_error = |e: io::Error| e.to_string();
    let findings = dead_ends(&request);
    let subjects = findings.len() as u64;
    match request.analysis.as_str() {
        "dead-ends" => {}
        // Findings about a record it was not sent.
        "forged" => {
            let mut finding = findings.into_iter().next().ok_or("no records")?;
            finding.resource = "node:forged".into();
            out.emit(&Message::Finding(Box::new(finding))).map_err(io_error)?;
            return out.emit(&Message::AnalysisDone { subjects }).map_err(io_error);
        }
        // Everything but saying that it finished.
        "unfinished" => {
            for finding in findings {
                out.emit(&Message::Finding(Box::new(finding))).map_err(io_error)?;
            }
            return Ok(());
        }
        "fails" => return Err("the analysis could not run".into()),
        // Never finishes, and goes on writing nothing.
        "hangs" => loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
        // One line past what a host holds.
        "endless-line" => {
            let line = "x".repeat(17 * 1024 * 1024);
            io::stdout().write_all(line.as_bytes()).map_err(io_error)?;
            return io::stdout().flush().map_err(io_error);
        }
        // Output that is not the protocol, after saying it finished.
        "not-json" => {
            out.emit(&Message::AnalysisDone { subjects }).map_err(io_error)?;
            io::stdout().write_all(b"not json\n").map_err(io_error)?;
            return io::stdout().flush().map_err(io_error);
        }
        other => return Err(format!("no analysis {other:?}")),
    }
    for finding in findings {
        out.emit(&Message::Finding(Box::new(finding))).map_err(io_error)?;
    }
    out.emit(&Message::AnalysisDone { subjects }).map_err(io_error)
}

fn main() -> ExitCode {
    let mut out = Emitter::stdout();
    let result = match std::env::args().nth(1).as_deref() {
        Some("describe") => out
            .emit(&Message::Describe(Describe {
                name: "Analyzer".into(),
                version: "1.0.0".into(),
                description: "An example analyzer".into(),
                config: vec![],
                permissions: Default::default(),
                labels: vec![],
                relationship_types: vec![],
                supports_incremental: false,
                analyses: ["dead-ends", "forged", "unfinished", "fails", "hangs", "endless-line", "not-json"].map(analysis).to_vec(),
            }))
            .map_err(|e| e.to_string()),
        Some("analyze") => analyze(&mut out),
        other => Err(format!("unknown command {other:?}")),
    };
    let _ = io::stdout().flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = out.emit(&Message::Error { message });
            ExitCode::FAILURE
        }
    }
}

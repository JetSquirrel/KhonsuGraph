//! An investigation report: one template run, kept in a file with what a
//! reviewer needs to check it and run it again.
//!
//! It holds the template as it ran, the parameters, what the graph held (its
//! fingerprint and each source's last sync), every step with its caps and
//! errors, the findings, the records they rest on, and what was missing. People
//! and agents then add to it: a hypothesis, a conclusion, a review of a finding.
//! Each addition is an entry in `actions`, in order.
//!
//! Four things are kept apart, because a report that blurs them cannot be
//! checked: what a source reported (`run.evidence`, and findings of class
//! `observation`), what was worked out from that (findings of class `derived`),
//! what someone suspects (`notes` of kind `hypothesis`), and what a person
//! decided (`reviews`, and notes of kind `conclusion`).
//!
//! The file holds no source configuration or credentials. Records have their
//! secrets masked as in evidence, and free text has key material masked.
//! `digest` is a SHA-256 of everything else in the file, so a change made
//! outside KhonsuGraph shows. It is a check against accidents and careless
//! edits, not a signature: whoever can write the file can write a new digest.
//! See `docs/reports.md`.

use crate::engine::{Graph, QueryError};
use crate::evidence::{self, Evidence, Field};
use crate::investigation::{self, check_header, Capture, FileError, GraphState};
use crate::settings::Source;
use crate::store;
use crate::template::{Class, Finding, Run, RunError, StepStatus, Template};
use crate::util::{format_rfc3339, sha256_hex, unix_now};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const FORMAT: &str = "khonsu-report";
pub const VERSION: u32 = 1;

/// One thing that was done, in the order it was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub seq: u32,
    /// Unix seconds.
    pub at: u64,
    /// Who did it: the program for a run, a name for a note or review.
    pub actor: String,
    /// `run`, `step`, `note` or `review`.
    pub action: String,
    /// The template, step, note or finding it was about.
    pub id: String,
    /// `ok`, `failed`, `cancelled`, or what a note or review said it was.
    pub outcome: String,
    /// The caps a step ran under.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub caps: Map<String, Json>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// Something suspected and not shown.
    Hypothesis,
    /// What a person decided.
    Conclusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub note_id: String,
    pub kind: NoteKind,
    pub text: String,
    pub by: String,
    pub at: u64,
    /// The finding it is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Confirmed,
    Rejected,
    /// Not enough to decide either way.
    NeedsMore,
}

impl ReviewState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::NeedsMore => "needs more",
        }
    }
}

/// What a person decided about one finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub state: ReviewState,
    pub by: String,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A snapshot of the graph taken with the run, for saying later what changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPin {
    pub snapshot_id: String,
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub format: String,
    pub version: u32,
    /// The same template, parameters and graph contents give the same ID,
    /// whenever and wherever the run was made.
    pub result_id: String,
    pub created_at: u64,
    /// The program that ran it.
    pub created_by: String,
    /// The template as it ran, so the run can be made again without its file.
    pub template: Template,
    pub run: Run,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotPin>,
    pub actions: Vec<Action>,
    #[serde(default)]
    pub notes: Vec<Note>,
    /// Reviews by finding ID.
    #[serde(default)]
    pub reviews: BTreeMap<String, Review>,
    /// SHA-256 of the file's contents with this field empty.
    pub digest: String,
}

/// What identifies a run's answer: what was asked, of what, and what came back.
/// Times, paths and the program's version are left out.
pub fn result_id(run: &Run) -> String {
    let said = json!({
        "template": run.template.digest,
        "parameters": run.parameters,
        "fingerprint": run.fingerprint,
        "max_findings": run.max_findings,
        "findings": run.findings.iter().map(|finding| &finding.finding_id).collect::<Vec<_>>(),
        "truncated": run.truncated,
        "failed": run.failed,
        "cancelled": run.cancelled,
    });
    format!("result:{}", &sha256_hex(said.to_string().as_bytes())[..32])
}

/// Whether a report is what was written, and holds together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub ok: bool,
    pub digest: String,
    pub result_id: String,
    pub problems: Vec<String>,
}

/// What differs between a report and the same run made again.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Replay {
    pub graph_state: GraphState,
    /// What changed in the graph, in words.
    pub differences: Vec<String>,
    pub saved_result_id: String,
    pub result_id: String,
    /// True when every finding is the same and nothing else about the answer
    /// differs.
    pub same: bool,
    pub unchanged: usize,
    /// Findings there now and not in the report, and the other way round.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub run: Run,
}

impl Report {
    /// A report of `run`, which `template` produced.
    pub fn new(template: &Template, run: Run, created_by: &str, snapshot: Option<SnapshotPin>) -> Self {
        let mut actions = vec![Action {
            seq: 1,
            at: run.steps.first().map_or_else(unix_now, |step| step.started_at),
            actor: created_by.to_string(),
            action: "run".into(),
            id: template.id.clone(),
            outcome: if run.cancelled {
                "cancelled"
            } else if run.failed.is_empty() {
                "ok"
            } else {
                "failed"
            }
            .into(),
            caps: Map::from_iter([("max_findings".to_string(), json!(run.max_findings))]),
            error: None,
        }];
        for step in &run.steps {
            actions.push(Action {
                seq: actions.len() as u32 + 1,
                at: step.started_at,
                actor: created_by.to_string(),
                action: "step".into(),
                id: step.id.clone(),
                outcome: match step.status {
                    StepStatus::Ok => "ok",
                    StepStatus::Failed => "failed",
                    StepStatus::Cancelled => "cancelled",
                }
                .into(),
                caps: step.caps.clone(),
                error: step.error.as_ref().map(|error| format!("{}: {}", error.kind, error.message)),
            });
        }
        let mut report = Report {
            format: FORMAT.into(),
            version: VERSION,
            result_id: result_id(&run),
            created_at: unix_now(),
            created_by: created_by.to_string(),
            template: template.clone(),
            run,
            snapshot,
            actions,
            notes: Vec::new(),
            reviews: BTreeMap::new(),
            digest: String::new(),
        };
        report.digest = report.digest_now();
        report
    }

    fn digest_now(&self) -> String {
        let mut value = serde_json::to_value(self).expect("a report serializes");
        value["digest"] = json!("");
        sha256_hex(value.to_string().as_bytes())
    }

    pub fn load(path: &Path) -> Result<Self, FileError> {
        let shown = path.display();
        let text = std::fs::read_to_string(path).map_err(|error| FileError {
            kind: if error.kind() == std::io::ErrorKind::NotFound { "not_found" } else { "io" },
            message: format!("cannot read {shown}: {error}"),
        })?;
        Self::parse(&text).map_err(|error| FileError { kind: error.kind, message: format!("{shown}: {}", error.message) })
    }

    pub fn parse(text: &str) -> Result<Self, FileError> {
        let value = check_header(text, FORMAT, VERSION)?;
        serde_json::from_value(value).map_err(|error| FileError { kind: "invalid", message: format!("its contents are not usable: {error}") })
    }

    /// Writes the file whole or not at all. An existing file is kept unless
    /// `overwrite` says otherwise.
    pub fn save(&self, path: &Path, overwrite: bool) -> Result<(), FileError> {
        if !overwrite && path.exists() {
            return Err(FileError { kind: "exists", message: format!("{} already exists", path.display()) });
        }
        let text = serde_json::to_string_pretty(self).expect("a report serializes") + "\n";
        store::write_bytes(path, text.as_bytes()).map_err(|error| FileError { kind: "io", message: format!("cannot write {}: {error}", path.display()) })
    }

    /// Checks the digest, and that the file holds together: the template is
    /// the one the run names, the result ID is the run's, and every record a
    /// finding cites is there.
    pub fn verify(&self) -> Verification {
        let mut problems = Vec::new();
        let digest = self.digest_now();
        if digest != self.digest {
            problems.push("The file is not as KhonsuGraph wrote it: its digest does not match its contents.".to_string());
        }
        if self.template.digest() != self.run.template.digest {
            problems.push("The template in the file is not the one the run names.".into());
        }
        let result = result_id(&self.run);
        if result != self.result_id {
            problems.push("The result ID does not match the run's findings and inputs.".into());
        }
        let held: BTreeSet<&str> = self.run.evidence.iter().map(|record| record.evidence_id.as_str()).collect();
        for finding in &self.run.findings {
            let lost = finding.evidence_ids.iter().filter(|id| !held.contains(id.as_str())).count();
            if lost > 0 {
                problems.push(format!("Finding {} cites {lost} records that are not in the file.", finding.finding_id));
            }
        }
        let known = self.run.finding_ids();
        for id in self.reviews.keys().filter(|id| !known.contains(id.as_str())) {
            problems.push(format!("A review is about {id}, which is not a finding in the file."));
        }
        Verification { ok: problems.is_empty(), digest, result_id: result, problems }
    }

    fn finding(&self, id: &str) -> Result<&Finding, FileError> {
        self.run.findings.iter().find(|finding| finding.finding_id == id).ok_or_else(|| FileError {
            kind: "not_found",
            message: format!("the report has no finding {id:?}"),
        })
    }

    fn act(&mut self, actor: &str, action: &str, id: &str, outcome: &str) -> u64 {
        // Never before the entry it follows, whatever the clock says.
        let at = unix_now().max(self.actions.last().map_or(0, |last| last.at));
        self.actions.push(Action {
            seq: self.actions.last().map_or(0, |last| last.seq) + 1,
            at,
            actor: actor.to_string(),
            action: action.to_string(),
            id: id.to_string(),
            outcome: outcome.to_string(),
            caps: Map::new(),
            error: None,
        });
        at
    }

    /// Records what a person decided about a finding. A later review of the
    /// same finding replaces it, and both stay in `actions`.
    pub fn review(&mut self, finding: &str, state: ReviewState, by: &str, note: Option<&str>) -> Result<(), FileError> {
        self.finding(finding)?;
        let by = named(by)?;
        let at = self.act(&by, "review", finding, state.word());
        self.reviews.insert(finding.to_string(), Review { state, by, at, note: note.map(evidence::scrub).filter(|note| !note.trim().is_empty()) });
        Ok(())
    }

    /// Adds a hypothesis or a conclusion, about one finding or the whole report.
    pub fn note(&mut self, kind: NoteKind, text: &str, by: &str, finding: Option<&str>) -> Result<String, FileError> {
        if let Some(finding) = finding {
            self.finding(finding)?;
        }
        let text = evidence::scrub(text.trim());
        if text.is_empty() {
            return Err(FileError { kind: "invalid", message: "a note needs some text".into() });
        }
        let by = named(by)?;
        let note_id = format!("note:{}", self.notes.len() + 1);
        let word = match kind {
            NoteKind::Hypothesis => "hypothesis",
            NoteKind::Conclusion => "conclusion",
        };
        let at = self.act(&by, "note", &note_id, word);
        self.notes.push(Note { note_id: note_id.clone(), kind, text, by, at, finding: finding.map(String::from) });
        Ok(note_id)
    }

    /// What to save so the window can show a finding's path: `khonsu open` takes
    /// the investigation file this makes.
    pub fn capture_of(&self, finding: &str) -> Result<Capture, FileError> {
        let found = self.finding(finding)?;
        let path = found.path.as_ref().filter(|path| !path.refs.is_empty()).ok_or_else(|| FileError {
            kind: "invalid",
            message: format!("finding {finding} has no path to show: only a walk's or a rule's finding has one"),
        })?;
        Ok(Capture {
            name: format!("{}: {}", self.template.name, found.resource_name.known().cloned().unwrap_or_else(|| found.summary.clone())),
            path_refs: path.refs.clone(),
            ..Capture::default()
        })
    }

    /// The parameters to run it again with: the saved ones, with `given` over
    /// them. A value masked in the file has to be given again.
    pub fn parameters_for_replay(&self, given: &Map<String, Json>) -> Result<Map<String, Json>, RunError> {
        let mut parameters = self.run.parameters.clone();
        parameters.extend(given.clone());
        match parameters.iter().find(|(_, value)| value.as_str() == Some("[redacted]")) {
            Some((name, _)) => Err(RunError {
                kind: "usage".into(),
                message: format!("parameter {name} was masked in the report: pass it again with -p {name}=..."),
                detail: Json::Null,
            }),
            None => Ok(parameters),
        }
    }

    /// Makes the run again on `graph` and says what differs. The report is not
    /// changed.
    pub fn replay(&self, graph: &mut Graph, given: &Map<String, Json>, sources: &[Source], cancel: &AtomicBool) -> Result<Replay, RunError> {
        let parameters = self.parameters_for_replay(given)?;
        let run = graph.run_template(&self.template, &parameters, self.run.max_findings, sources, cancel)?;
        let (graph_state, differences) = investigation::compare(&self.run.fingerprint, &run.fingerprint);
        let (was, now) = (self.run.finding_ids(), run.finding_ids());
        let result = result_id(&run);
        Ok(Replay {
            graph_state,
            differences,
            same: result == self.result_id,
            saved_result_id: self.result_id.clone(),
            unchanged: was.intersection(&now).count(),
            added: now.difference(&was).map(|id| id.to_string()).collect(),
            removed: was.difference(&now).map(|id| id.to_string()).collect(),
            result_id: result,
            run,
        })
    }
}

fn named(by: &str) -> Result<String, FileError> {
    let by = by.trim();
    if by.is_empty() {
        return Err(FileError { kind: "invalid", message: "say who this is from".into() });
    }
    Ok(evidence::scrub(by))
}

/// Changes the report at `path` and writes it back, with the file locked so an
/// agent and a person cannot lose each other's addition. A file whose digest
/// does not match is not built on.
pub fn update<R>(path: &Path, change: impl FnOnce(&mut Report) -> Result<R, FileError>) -> Result<(R, Report), FileError> {
    let io = |error: std::io::Error| FileError { kind: "io", message: format!("{}: {error}", path.display()) };
    let _lock = store::lock(path).map_err(io)?;
    let mut report = Report::load(path)?;
    let checked = report.verify();
    if !checked.ok {
        return Err(FileError { kind: "invalid", message: format!("{} was not changed. {}", path.display(), checked.problems.join(" ")) });
    }
    let out = change(&mut report)?;
    report.digest = report.digest_now();
    report.save(path, true)?;
    Ok((out, report))
}

impl Graph {
    /// The investigation that shows a finding's path, as the graph stands now.
    pub fn investigation_of(&mut self, report: &Report, finding: &str, sources: &[Source], cancel: &AtomicBool) -> Result<Result<investigation::Investigation, FileError>, QueryError> {
        let capture = match report.capture_of(finding) {
            Ok(capture) => capture,
            Err(error) => return Ok(Err(error)),
        };
        let path = self.path().to_path_buf();
        self.capture_investigation(&path, capture, sources, cancel).map(Ok)
    }
}

/// Text for a table cell or a list item: one line, with `|` kept from ending
/// the cell.
fn cell(text: &str) -> String {
    text.replace('\\', "\\\\").replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn code(text: &str) -> String {
    // A backtick inside would end the span early.
    format!("`{}`", text.replace('`', "'"))
}

fn known<T: ToString>(field: &Field<T>) -> String {
    field.known().map_or_else(|| "unknown".to_string(), ToString::to_string)
}

fn time(field: &Field<u64>) -> String {
    field.known().map_or_else(|| "unknown".to_string(), |at| format_rfc3339(*at))
}

impl Report {
    /// The report for a person to read. It says the same as the file, and the
    /// file is what is checked.
    pub fn to_markdown(&self) -> String {
        let run = &self.run;
        let mut out = String::new();
        let w = &mut out;
        let _ = writeln!(w, "# Investigation report: {}\n", cell(&self.template.name));
        let _ = writeln!(w, "**Question.** {}\n", cell(&self.template.question));
        let _ = writeln!(w, "**What the findings show.** {}\n", cell(&self.template.claim));
        let _ = writeln!(w, "| | |\n|---|---|");
        let _ = writeln!(w, "| Result ID | {} |", code(&self.result_id));
        let _ = writeln!(w, "| Made | {} by {} |", format_rfc3339(self.created_at), cell(&self.created_by));
        let _ = writeln!(w, "| Complete | {} |", if run.complete { "yes" } else { "**no**: see Data gaps" });
        let _ = writeln!(w, "| Findings | {}{} |", run.findings.len(), if run.truncated > 0 { format!(" (and {} left out by the cap)", run.truncated) } else { String::new() });
        let _ = writeln!(w, "| Report schema | {FORMAT} version {} |", self.version);
        let _ = writeln!(w, "| Digest (SHA-256) | {} |", code(&self.digest));

        let _ = writeln!(w, "\n## Inputs\n");
        let _ = writeln!(w, "- Template {} revision {}, digest {}", code(&self.template.id), code(&self.template.revision), code(&run.template.digest));
        if run.parameters.is_empty() {
            let _ = writeln!(w, "- Parameters: none");
        }
        for (name, value) in &run.parameters {
            let _ = writeln!(w, "- Parameter {}: {}", code(name), code(&plain(value)));
        }
        let _ = writeln!(w, "- Graph: {}", code(&run.graph.display().to_string()));
        let print = &run.fingerprint;
        let _ = writeln!(
            w,
            "- Graph contents: {} nodes, {} relationships, fingerprint {}",
            print.nodes,
            print.relationships,
            print.hash.as_deref().map_or_else(|| "not taken (too large)".to_string(), code)
        );
        if let Some(snapshot) = &self.snapshot {
            let _ = writeln!(w, "- Snapshot: {} in {}", code(&snapshot.snapshot_id), code(&snapshot.file.display().to_string()));
        }
        if !run.sources.is_empty() {
            let _ = writeln!(w, "\n| Source | Last sync | Status | Gaps |\n|---|---|---|---|");
            for (source, coverage) in &run.sources {
                match coverage.known() {
                    Some(coverage) => {
                        let _ = writeln!(w, "| {} | {} | {} | {} |", cell(source), code(&coverage.run_id), coverage.status.word(), cell(&coverage.gaps.join("; ")));
                    }
                    None => {
                        let _ = writeln!(w, "| {} | unknown | unknown | |", cell(source));
                    }
                }
            }
        }

        let _ = writeln!(w, "\n## Data gaps\n");
        if run.gaps.is_empty() {
            let _ = writeln!(w, "None known. Every step ran within its caps, and every source's last sync was complete or its coverage is unknown.");
        } else {
            let _ = writeln!(w, "A finding that is not in this report means nothing where these apply.\n");
            for gap in &run.gaps {
                let _ = writeln!(w, "- {}", cell(gap));
            }
        }

        let _ = writeln!(w, "\n## Steps\n\n| Step | Kind | Status | Caps | Caps reached | Findings | Error |\n|---|---|---|---|---|---|---|");
        for step in &run.steps {
            let caps: Vec<String> = step.caps.iter().map(|(name, value)| format!("{name}={value}")).collect();
            let status = match step.status {
                StepStatus::Ok => "ok",
                StepStatus::Failed => "**failed**",
                StepStatus::Cancelled => "**cancelled**",
            };
            let error = step.error.as_ref().map_or(String::new(), |error| format!("{}: {}", error.kind, error.message));
            let _ = writeln!(w, "| {} | {} | {status} | {} | {} | {} | {} |", code(&step.id), step.kind, cell(&caps.join(", ")), cell(&step.limits.join(", ")), step.findings, cell(&error));
        }

        let _ = writeln!(w, "\n## Human conclusions\n");
        let conclusions: Vec<&Note> = self.notes.iter().filter(|note| note.kind == NoteKind::Conclusion).collect();
        if conclusions.is_empty() && self.reviews.is_empty() {
            let _ = writeln!(w, "None yet: nobody has reviewed this report.");
        }
        for note in conclusions {
            let about = note.finding.as_ref().map_or(String::new(), |finding| format!(" (about {})", code(finding)));
            let _ = writeln!(w, "- {} — {}, {}{about}", cell(&note.text), cell(&note.by), format_rfc3339(note.at));
        }
        if !self.reviews.is_empty() {
            let count = |state: ReviewState| self.reviews.values().filter(|review| review.state == state).count();
            let unreviewed = run.findings.len().saturating_sub(self.reviews.len());
            let _ = writeln!(
                w,
                "\nFindings reviewed: {} confirmed, {} rejected, {} need more, {unreviewed} not reviewed.",
                count(ReviewState::Confirmed),
                count(ReviewState::Rejected),
                count(ReviewState::NeedsMore)
            );
        }

        let _ = writeln!(w, "\n## Hypotheses\n");
        let hypotheses: Vec<&Note> = self.notes.iter().filter(|note| note.kind == NoteKind::Hypothesis).collect();
        if hypotheses.is_empty() {
            let _ = writeln!(w, "None recorded.");
        } else {
            let _ = writeln!(w, "Suspected, and not shown by anything in this report.\n");
        }
        for note in hypotheses {
            let about = note.finding.as_ref().map_or(String::new(), |finding| format!(" (about {})", code(finding)));
            let _ = writeln!(w, "- {} — {}, {}{about}", cell(&note.text), cell(&note.by), format_rfc3339(note.at));
        }

        for (class, title, lead) in [
            (Class::Derived, "Derived findings", "Worked out from the records below by a walk or a rule. A path is a connection in the data, not proof that anything can reach anything."),
            (Class::Observation, "Observations", "Rows of what the graph holds, as a query returned them."),
        ] {
            let findings: Vec<&Finding> = run.findings.iter().filter(|finding| finding.class == class).collect();
            let _ = writeln!(w, "\n## {title}\n");
            if findings.is_empty() {
                let _ = writeln!(w, "None.");
                continue;
            }
            let _ = writeln!(w, "{lead}");
            for finding in findings {
                self.finding_markdown(w, finding);
            }
        }

        let _ = writeln!(w, "\n## Evidence\n\nWhat the sources reported: every record a finding rests on. Secrets are masked.");
        if run.evidence.is_empty() {
            let _ = writeln!(w, "No records.");
        }
        for record in &run.evidence {
            evidence_markdown(w, record);
        }

        let _ = writeln!(w, "\n## Action log\n\n| # | When | Who | Action | About | Outcome | Caps | Error |\n|---|---|---|---|---|---|---|---|");
        for action in &self.actions {
            let caps: Vec<String> = action.caps.iter().map(|(name, value)| format!("{name}={value}")).collect();
            let _ = writeln!(
                w,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                action.seq,
                format_rfc3339(action.at),
                cell(&action.actor),
                action.action,
                code(&action.id),
                cell(&action.outcome),
                cell(&caps.join(", ")),
                cell(action.error.as_deref().unwrap_or_default())
            );
        }

        let _ = writeln!(w, "\n## Check and reproduce\n");
        let _ = writeln!(w, "This page is a rendering of the report file, which is what is checked:\n");
        let _ = writeln!(w, "```sh\nkhonsu report verify REPORT.json\nkhonsu report replay REPORT.json --db {}\n```\n", shell(&run.graph.display().to_string()));
        let _ = writeln!(w, "`verify` checks the digest above. `replay` makes the run again and compares finding IDs: on an unchanged graph the result ID is {} again.", code(&self.result_id));
        out
    }

    fn finding_markdown(&self, w: &mut String, finding: &Finding) {
        let name = finding.resource_name.known().cloned().unwrap_or_else(|| finding.summary.clone());
        let _ = writeln!(w, "\n### {}\n", cell(&name));
        let _ = writeln!(w, "- Finding {}, step {}, rule {}", code(&finding.finding_id), code(&finding.step), code(&finding.rule_id));
        if let Some(verdict) = finding.verdict.known() {
            let _ = writeln!(w, "- Verdict: {}", cell(verdict));
        }
        let review = match self.reviews.get(&finding.finding_id) {
            Some(review) => format!(
                "**{}** by {}, {}{}",
                review.state.word(),
                cell(&review.by),
                format_rfc3339(review.at),
                review.note.as_ref().map_or(String::new(), |note| format!(": {}", cell(note)))
            ),
            None => "not reviewed".into(),
        };
        let _ = writeln!(w, "- Review: {review}");
        if let Some(resource) = finding.resource.known() {
            let _ = writeln!(w, "- About: {}", code(resource));
        }
        if !finding.values.is_empty() {
            let values: Vec<String> = finding.values.iter().map(|(column, value)| format!("{column} = {}", code(&plain(value)))).collect();
            let _ = writeln!(w, "- Values: {}", values.join(", "));
        }
        if let Some(path) = &finding.path {
            let _ = writeln!(w, "- Path {} ({}; confidence {}):", code(&path.evidence_id), cell(&path.derivation), path.confidence.known().map_or("unknown", |confidence| confidence.as_str()));
            for (at, hop) in path.hops.iter().enumerate() {
                let _ = writeln!(w, "  {}. {}", at + 1, cell(hop));
            }
        }
        for condition in &finding.conditions {
            let status = serde_json::to_value(condition.status).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
            let _ = writeln!(w, "- Condition {} — {}: **{}**. {}", code(&condition.id), cell(&condition.statement), status.replace('_', " "), cell(&condition.detail));
            for field in &condition.fields {
                let value = field.value.known().map_or_else(|| "unknown".to_string(), |value| code(&plain(value)));
                let _ = writeln!(w, "  - {} of {} = {value}", code(&field.field), code(&field.record));
            }
        }
        for unknown in &finding.unknowns {
            let _ = writeln!(w, "- Unknown: {}", cell(unknown));
        }
        for assumption in &finding.assumptions {
            let _ = writeln!(w, "- Assumes: {}", cell(assumption));
        }
        if !finding.evidence_ids.is_empty() {
            let ids: Vec<String> = finding.evidence_ids.iter().map(|id| code(id)).collect();
            let _ = writeln!(w, "- Evidence: {}", ids.join(", "));
        }
    }
}

fn evidence_markdown(w: &mut String, record: &Evidence) {
    let _ = writeln!(w, "\n### {}\n", code(&record.evidence_id));
    let kind = match record.kind {
        evidence::Kind::Node => "Node",
        evidence::Kind::Relationship => "Relationship",
        evidence::Kind::Path => "Path",
    };
    let _ = writeln!(w, "- {kind}: {}{}", cell(&record.types.join(", ")), if record.stable { "" } else { " (no stable ID: may not survive a rebuild)" });
    if let (Some(from), Some(to)) = (&record.from, &record.to) {
        let _ = writeln!(w, "- From {} to {}", code(&known(from)), code(&known(to)));
    }
    let said = match record.assertion {
        evidence::Assertion::Explicit => "reported by a data source",
        evidence::Assertion::Derived => "worked out by KhonsuGraph",
        evidence::Assertion::Unknown => "not written by a data source",
    };
    let _ = writeln!(w, "- Source: {} via {} ({said})", cell(&known(&record.source)), cell(&known(&record.plugin)));
    let coverage = record.source_coverage.known().map_or_else(|| "unknown".to_string(), |coverage| format!("{} ({})", coverage.status.word(), coverage.run_id));
    let _ = writeln!(w, "- Record written {}; source last synced completely {}; last sync {}", time(&record.record_written_at), time(&record.source_last_successful_sync), cell(&coverage));
    if let Some(confidence) = record.confidence.known() {
        let _ = writeln!(w, "- Confidence: {}{}", confidence.as_str(), record.evidence.known().map_or(String::new(), |shown| format!(" — {}", cell(shown))));
    }
    if !record.properties.is_empty() {
        let _ = writeln!(w, "- Properties: {}", code(&Json::Object(record.properties.clone()).to_string()));
    }
    if !record.redacted.is_empty() {
        let _ = writeln!(w, "- Masked: {}", cell(&record.redacted.join(", ")));
    }
}

fn plain(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A path as one word of a shell command.
fn shell(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '\\')) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_in_a_table_stays_in_its_cell() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(code("it`s"), "`it's`");
        assert_eq!(shell("graphs/shop"), "graphs/shop");
        assert_eq!(shell("my graphs/it's"), "'my graphs/it'\\''s'");
    }
}

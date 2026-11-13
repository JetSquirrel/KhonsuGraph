//! `khonsu report`: check a kept template run, read it, review it, and make
//! it again. See `khonsu_core::report` and `docs/reports.md`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use clap::{Args, Subcommand, ValueEnum};
use khonsu_core::report::{self, NoteKind, Report, ReviewState};
use khonsu_core::{normalize_path, settings, AccessMode, Graph};
use serde_json::{json, Value};

use crate::{key_values, CliError, PROGRAM};

#[derive(Subcommand)]
pub enum ReportCommand {
    /// Whether the file is as it was written: its digest, its result ID and the records its findings cite.
    Verify(FileArg),
    /// Write the report for a person to read (Markdown), or a finding's path for the window to show.
    Export(ExportArgs),
    /// Make the run again and say which findings are the same, new or gone. The report is not changed.
    Replay(ReplayArgs),
    /// Record what a person decided about a finding.
    Review(ReviewArgs),
    /// Add a hypothesis (suspected, not shown) or a conclusion (what a person decided).
    Note(NoteArgs),
}

#[derive(Args)]
pub struct FileArg {
    /// The report file.
    file: PathBuf,
}

#[derive(Args)]
pub struct ExportArgs {
    /// The report file.
    file: PathBuf,
    /// Write the whole report as Markdown to this file.
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// Write a finding's path to this `.investigation` file, which `khonsu open` shows in the window. Needs --finding.
    #[arg(long, requires = "finding")]
    investigation: Option<PathBuf>,
    /// The finding whose path to write.
    #[arg(long)]
    finding: Option<String>,
    /// Use this graph for --investigation instead of the one the report names.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
    /// Replace the files if they exist.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct ReplayArgs {
    /// The report file.
    file: PathBuf,
    /// Use this graph instead of the one the report names.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
    /// A parameter to change, or to give again when the report masked it: `name=value`.
    #[arg(long = "param", short = 'p')]
    params: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum StateArg {
    Confirmed,
    Rejected,
    NeedsMore,
}

#[derive(Args)]
pub struct ReviewArgs {
    /// The report file.
    file: PathBuf,
    /// The finding's ID.
    #[arg(long)]
    finding: String,
    #[arg(long, value_enum)]
    state: StateArg,
    /// Who decided.
    #[arg(long)]
    by: String,
    /// Why, in a line. Leave out anything secret.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Args)]
pub struct NoteArgs {
    /// The report file.
    file: PathBuf,
    /// Something suspected that the report does not show.
    #[arg(long, conflicts_with = "conclusion", required_unless_present = "conclusion")]
    hypothesis: Option<String>,
    /// What a person decided.
    #[arg(long)]
    conclusion: Option<String>,
    /// Who it is from.
    #[arg(long)]
    by: String,
    /// The finding it is about, when it is about one.
    #[arg(long)]
    finding: Option<String>,
}

pub fn report(command: ReportCommand) -> Result<Value, CliError> {
    match command {
        ReportCommand::Verify(args) => {
            let report = Report::load(&args.file)?;
            let checked = report.verify();
            let mut out = json!(checked);
            out["template"] = json!(report.template.id);
            out["created_at"] = json!(report.created_at);
            out["complete"] = json!(report.run.complete);
            out["findings"] = json!(report.run.findings.len());
            out["reviewed"] = json!(report.reviews.len());
            // A script must not take a changed file for a good one.
            out["failed"] = json!(checked.problems);
            Ok(out)
        }
        ReportCommand::Export(args) => export(args),
        ReportCommand::Replay(args) => replay(args),
        ReportCommand::Review(args) => {
            let state = match args.state {
                StateArg::Confirmed => ReviewState::Confirmed,
                StateArg::Rejected => ReviewState::Rejected,
                StateArg::NeedsMore => ReviewState::NeedsMore,
            };
            let (_, saved) = report::update(&args.file, |report| report.review(&args.finding, state, &args.by, args.note.as_deref()))?;
            Ok(json!({ "reviewed": args.finding, "review": saved.reviews.get(&args.finding), "digest": saved.digest }))
        }
        ReportCommand::Note(args) => {
            let (kind, text) = match (&args.hypothesis, &args.conclusion) {
                (Some(text), _) => (NoteKind::Hypothesis, text),
                (_, Some(text)) => (NoteKind::Conclusion, text),
                _ => return Err(CliError::usage("give --hypothesis or --conclusion")),
            };
            let (note_id, saved) = report::update(&args.file, |report| report.note(kind, text, &args.by, args.finding.as_deref()))?;
            Ok(json!({ "noted": saved.notes.iter().find(|note| note.note_id == note_id), "digest": saved.digest }))
        }
    }
}

fn graph_of(report: &Report, db: &Option<PathBuf>) -> Result<Graph, CliError> {
    let path = db.clone().unwrap_or_else(|| report.run.graph.clone());
    if !path.is_dir() {
        return Err(CliError::new("graph_missing", format!("the graph {} is not there; pass --db to use another", path.display())));
    }
    Ok(Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?)
}

fn export(args: ExportArgs) -> Result<Value, CliError> {
    if args.markdown.is_none() && args.investigation.is_none() {
        return Err(CliError::usage("say what to write: --markdown FILE, or --investigation FILE --finding ID"));
    }
    let report = Report::load(&args.file)?;
    let checked = report.verify();
    let mut out = json!({ "verified": checked.ok, "problems": checked.problems });
    if let Some(path) = &args.markdown {
        if !args.force && path.exists() {
            return Err(CliError::new("exists", format!("{} already exists; pass --force to replace it", path.display())));
        }
        let mut text = report.to_markdown();
        if !checked.ok {
            text = format!("> **This report did not verify.** {}\n\n{text}", checked.problems.join(" "));
        }
        khonsu_core::store::write_bytes(path, text.as_bytes())?;
        out["markdown"] = json!(normalize_path(path));
    }
    if let (Some(path), Some(finding)) = (&args.investigation, &args.finding) {
        let mut graph = graph_of(&report, &args.db)?;
        let saved = graph.investigation_of(&report, finding, &settings::load_sources(), &AtomicBool::new(false))??;
        saved.save(path, args.force)?;
        out["investigation"] = json!(normalize_path(path));
        out["open_with"] = json!(format!("khonsu open {}", path.display()));
    }
    Ok(out)
}

fn replay(args: ReplayArgs) -> Result<Value, CliError> {
    let report = Report::load(&args.file)?;
    let checked = report.verify();
    let mut graph = graph_of(&report, &args.db)?;
    let replayed = report.replay(&mut graph, &key_values(&args.params)?, &settings::load_sources(), &AtomicBool::new(false))?;
    let mut out = json!(replayed);
    out["verified"] = json!(checked.ok);
    out["problems"] = json!(checked.problems);
    Ok(out)
}

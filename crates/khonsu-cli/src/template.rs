//! `khonsu template`: list, check and run investigation templates. See
//! `khonsu_core::template` and `docs/templates.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use khonsu_core::report::{Report, SnapshotPin};
use khonsu_core::template::{self, RunError, Template, DEFAULT_MAX_FINDINGS};
use khonsu_core::{normalize_path, plugin, settings, AccessMode, Graph};
use serde_json::{json, Value};

use crate::{key_values, resolve_db, CliError, DbArg, PROGRAM};

#[derive(Subcommand)]
pub enum TemplateCommand {
    /// Templates in your templates folder and in each plugin's.
    List(ListArgs),
    /// Whether a template is usable, and whether a graph holds what it needs. Nothing is run.
    Check(CheckArgs),
    /// Run a template's steps, read-only and within their caps, and print its findings with the records behind them.
    Run(Box<RunArgs>),
}

#[derive(Args)]
pub struct ListArgs {
    /// Also look in this folder. Repeat for several.
    #[arg(long)]
    dir: Vec<PathBuf>,
}

#[derive(Args)]
pub struct CheckArgs {
    /// A template's ID (see `khonsu template list`), or a file.
    template: String,
    /// The graph to check against. Without one, only the file is checked.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
    /// Also look for the ID in this folder. Repeat for several.
    #[arg(long)]
    dir: Vec<PathBuf>,
}

#[derive(Args)]
pub struct RunArgs {
    /// A template's ID (see `khonsu template list`), or a file.
    template: String,
    #[command(flatten)]
    db: DbArg,
    /// Parameter `name=value`; value is parsed as JSON when it can be.
    #[arg(long = "param", short = 'p')]
    params: Vec<String>,
    /// Findings to print; `truncated` counts the rest.
    #[arg(long, default_value_t = DEFAULT_MAX_FINDINGS)]
    max_findings: usize,
    /// Stop starting steps after this many seconds. A step already running finishes first.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    /// Also keep the run in a report file, e.g. `web-tier.report.json` (see `khonsu report`).
    #[arg(long)]
    report: Option<PathBuf>,
    /// With --report: also keep the graph's records in this snapshot file, to say later what changed.
    #[arg(long, requires = "report")]
    snapshot: Option<PathBuf>,
    /// Replace the report and snapshot files if they exist.
    #[arg(long)]
    force: bool,
    /// Also look for the ID in this folder. Repeat for several.
    #[arg(long)]
    dir: Vec<PathBuf>,
}

impl From<RunError> for CliError {
    fn from(error: RunError) -> Self {
        let mut out = if error.kind == "usage" { CliError::usage(error.message) } else { CliError::new(&error.kind, error.message) };
        out.detail = Box::new(error.detail);
        out
    }
}

fn folders(extra: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut folders = template::folders();
    folders.extend(extra.iter().map(|dir| ("folder".to_string(), dir.clone())));
    folders
}

/// The template `name` means: a file when it is one, or the template with
/// that ID in the folders templates are kept in.
fn find(name: &str, extra: &[PathBuf]) -> Result<(Template, PathBuf), CliError> {
    let file = Path::new(name);
    if file.is_file() {
        return Ok((Template::load(file)?, normalize_path(file)));
    }
    let listed = template::list_in(&folders(extra));
    let mut matches = listed.iter().filter(|listed| listed.template.as_ref().is_some_and(|template| template.id == name));
    let Some(found) = matches.next() else {
        let ids: Vec<&str> = listed.iter().filter_map(|listed| listed.template.as_ref().map(|template| template.id.as_str())).collect();
        let known = if ids.is_empty() { "none are installed".to_string() } else { format!("there are {}", ids.join(", ")) };
        return Err(CliError::new("not_found", format!("no template or file {name:?}: {known}")));
    };
    // The first folder wins, and the others are named rather than hidden.
    if let Some(other) = matches.next() {
        eprintln!("{}", json!({ "warning": format!("template {name} is also in {}; using {}", other.file.display(), found.file.display()) }));
    }
    Ok((Template::load(&found.file)?, found.file.clone()))
}

pub fn template(command: TemplateCommand) -> Result<Value, CliError> {
    match command {
        TemplateCommand::List(args) => Ok(json!({ "templates": template::list_in(&folders(&args.dir)) })),
        TemplateCommand::Check(args) => check(args),
        TemplateCommand::Run(args) => run(*args),
    }
}

/// Which of a template's plugins may run now, and why not for the rest.
fn plugins_of(template: &Template) -> Vec<Value> {
    template
        .plugins()
        .into_iter()
        .map(|id| match plugin::find(&id).and_then(|info| plugin::require_approved(&info)) {
            Ok(()) => json!({ "plugin": id, "ready": true }),
            Err(error) => json!({ "plugin": id, "ready": false, "reason": error.to_string() }),
        })
        .collect()
}

fn check(args: CheckArgs) -> Result<Value, CliError> {
    let (template, file) = find(&args.template, &args.dir)?;
    let plugins = plugins_of(&template);
    let mut ready = plugins.iter().all(|plugin| plugin["ready"] == true);
    let mut out = json!({
        "file": file,
        "valid": true,
        "template": template.summary(),
        "plugins": plugins,
    });
    if let Some(db) = &args.db {
        let mut graph = Graph::open(db, AccessMode::ReadOnly, PROGRAM)?;
        let missing = graph.missing_for(&template)?;
        ready &= missing.is_empty();
        out["graph"] = json!({ "path": normalize_path(db), "supported": missing.is_empty(), "missing": missing });
    }
    out["ready"] = json!(ready);
    Ok(out)
}

fn run(args: RunArgs) -> Result<Value, CliError> {
    let (template, file) = find(&args.template, &args.dir)?;
    let given = key_values(&args.params)?;
    for path in args.report.iter().chain(&args.snapshot) {
        if !args.force && path.exists() {
            return Err(CliError::new("exists", format!("{} already exists; pass --force to replace it", path.display())));
        }
    }
    let path = resolve_db(&args.db)?;
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let sources = settings::load_sources();

    let cancel = Arc::new(AtomicBool::new(false));
    if args.timeout > 0 {
        let (flag, wait) = (cancel.clone(), Duration::from_secs(args.timeout));
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            flag.store(true, Ordering::Relaxed);
        });
    }
    let run = graph.run_template(&template, &given, args.max_findings, &sources, &cancel)?;

    let mut out = json!(run);
    out["template_file"] = json!(file);
    if let Some(report_path) = &args.report {
        let snapshot = match &args.snapshot {
            Some(snapshot_path) => {
                let taken = graph.snapshot(&sources)?;
                taken.save(snapshot_path, args.force)?;
                Some(SnapshotPin { snapshot_id: taken.snapshot_id, file: normalize_path(snapshot_path) })
            }
            None => None,
        };
        let report = Report::new(&template, run, &created_by(), snapshot);
        report.save(report_path, args.force)?;
        out["report"] = json!({
            "saved": normalize_path(report_path),
            "result_id": report.result_id,
            "digest": report.digest,
            "snapshot": report.snapshot,
        });
    }
    Ok(out)
}

pub(crate) fn created_by() -> String {
    format!("{PROGRAM} {}", env!("CARGO_PKG_VERSION"))
}

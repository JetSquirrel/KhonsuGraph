//! `khonsu plugin analyze`: findings from an analysis a plugin offers. See
//! `khonsu_core::plugin_analysis` and `docs/plugins.md`.

use clap::Args;
use khonsu_core::plugin_analysis::{self, DEFAULT_MAX_FINDINGS};
use khonsu_core::{settings, AccessMode, Graph};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Args)]
pub struct AnalyzeArgs {
    /// The plugin, e.g. `aws` (see `khonsu plugin list`).
    plugin: String,
    /// The analysis, e.g. `exposure` (see `khonsu plugin describe ID`).
    analysis: String,
    #[command(flatten)]
    db: DbArg,
    /// Only findings about this resource: its `khonsu_id`.
    #[arg(long)]
    resource: Option<String>,
    /// Only findings with this verdict, as the analysis names it.
    #[arg(long)]
    verdict: Option<String>,
    /// Findings to print; `truncated` counts the rest.
    #[arg(long, default_value_t = DEFAULT_MAX_FINDINGS)]
    max_findings: usize,
}

pub fn analyze(args: AnalyzeArgs) -> Result<Value, CliError> {
    let offered = plugin_analysis::find(&args.plugin, &args.analysis)?;
    // `potential-exposure` reads as well as `potential_exposure` on a command line.
    let verdict = args.verdict.map(|verdict| verdict.replace('-', "_"));
    if let Some(wanted) = &verdict {
        if !offered.info.verdicts.iter().any(|verdict| verdict.id == *wanted) {
            let names: Vec<&str> = offered.info.verdicts.iter().map(|verdict| verdict.id.as_str()).collect();
            return Err(CliError::usage(format!("no verdict {wanted:?}: this analysis has {}", names.join(", "))));
        }
    }
    let path = resolve_db(&args.db)?;
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let input = graph.read_analysis_input(&offered.info)?;
    let report = plugin_analysis::run(&offered, &input, &settings::load_sources(), &AtomicBool::new(false), |_| {})?;
    let resource = args.resource.map(|id| format!("node:{id}"));
    // A mistyped ID must not read as "no findings".
    if let Some(id) = &resource {
        if !report.findings.iter().any(|finding| finding.resource == *id) {
            return Err(CliError::new("not_found", format!("no finding about {:?}", id.trim_start_matches("node:"))));
        }
    }
    let (report, truncated) = report.narrowed(
        |finding| resource.as_ref().is_none_or(|id| finding.resource == *id) && verdict.as_ref().is_none_or(|wanted| finding.verdict == *wanted),
        args.max_findings,
    );
    let mut out = json!(report);
    out["truncated"] = json!(truncated);
    Ok(out)
}

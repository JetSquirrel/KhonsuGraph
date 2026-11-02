//! `khonsu exposure`: explainable AWS exposure findings. See
//! `khonsu_core::exposure` and `docs/exposure-rules.md`.

use clap::{Args, ValueEnum};
use khonsu_core::exposure::{self, Verdict};
use khonsu_core::{settings, AccessMode, Graph};
use serde_json::{json, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Clone, Copy, ValueEnum)]
enum VerdictArg {
    PotentialExposure,
    InsufficientData,
    NotIndicated,
}

impl From<VerdictArg> for Verdict {
    fn from(verdict: VerdictArg) -> Self {
        match verdict {
            VerdictArg::PotentialExposure => Verdict::PotentialExposure,
            VerdictArg::InsufficientData => Verdict::InsufficientData,
            VerdictArg::NotIndicated => Verdict::NotIndicated,
        }
    }
}

#[derive(Args)]
pub struct ExposureArgs {
    #[command(flatten)]
    db: DbArg,
    /// Only findings about this resource: its `khonsu_id`.
    #[arg(long)]
    resource: Option<String>,
    /// Only findings with this verdict.
    #[arg(long, value_enum)]
    verdict: Option<VerdictArg>,
    /// Findings to print; `truncated` counts the rest.
    #[arg(long, default_value_t = exposure::DEFAULT_MAX_FINDINGS)]
    max_findings: usize,
}

pub fn exposure(args: ExposureArgs) -> Result<Value, CliError> {
    let path = resolve_db(&args.db)?;
    let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
    let report = graph.exposure(&settings::load_sources())?;
    let resource = args.resource.map(|id| format!("node:{id}"));
    // Every instance gets a finding, so a mistyped ID must not read as "none".
    if let Some(id) = &resource {
        if !report.findings.iter().any(|finding| finding.resource == *id) {
            return Err(CliError::new("not_found", format!("no instance {:?}", id.trim_start_matches("node:"))));
        }
    }
    let verdict = args.verdict.map(Verdict::from);
    let (report, truncated) = report.narrowed(
        |finding| resource.as_ref().is_none_or(|id| finding.resource == *id) && verdict.is_none_or(|wanted| finding.verdict == wanted),
        args.max_findings,
    );
    let mut out = json!(report);
    out["truncated"] = json!(truncated);
    Ok(out)
}

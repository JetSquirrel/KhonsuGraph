//! `khonsu graph ontology`: what the labels and relationship types of a graph mean,
//! from the ontology library. See `khonsu_core::ontology`.
//!
//! Every answer names the ontology and mapping files it used
//! (`ontology_versions`) and the draft definitions it rests on (`drafts`),
//! which an agent tells the person have not been reviewed.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use khonsu_core::ontology::{self, Library, Status};
use khonsu_core::{AccessMode, Graph, GraphSchema, OpenError};
use serde_json::{json, Value};

use crate::{dash, resolve_db, CliError, DbArg, PROGRAM};

/// Exit status when the graph cannot answer for what was asked.
const NOT_INVESTIGABLE: u8 = 3;
const MAX_HOPS: usize = 8;

#[derive(Subcommand)]
pub enum OntologyCommand {
    /// The concepts and relationships the library declares, and with --db which labels of the graph they cover.
    Describe(DescribeArgs),
    /// What words mean: a concept or relationship by name or synonym, or a label or type a mapping gives a meaning.
    Resolve(ResolveArgs),
    /// Whether a graph holds records of each thing a question is about. Exits 3 when it does not.
    Check(CheckArgs),
    /// How one concept connects to another: through declared relationships, and with --db through the graph's labels, as Cypher patterns.
    Path(PathArgs),
}

#[derive(Args)]
pub struct LibraryArgs {
    /// Also read ontology and mapping files (*.ontology.json, *.mapping.json) from this file or folder. Repeatable.
    #[arg(long = "ontology")]
    ontology: Vec<PathBuf>,
}

#[derive(Args)]
pub struct DescribeArgs {
    #[command(flatten)]
    library: LibraryArgs,
    /// One concept in full: what it extends, what extends it, its relationships and the labels that hold it.
    #[arg(long)]
    concept: Option<String>,
    /// Names and statuses only.
    #[arg(long)]
    brief: bool,
    /// Compare with this graph: which labels are mapped, which are not, and what is mapped but absent.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
}

#[derive(Args)]
pub struct ResolveArgs {
    #[command(flatten)]
    library: LibraryArgs,
    /// Words to look up, such as `host` or `logged on`.
    #[arg(required = true)]
    words: Vec<String>,
}

#[derive(Args)]
pub struct CheckArgs {
    #[command(flatten)]
    library: LibraryArgs,
    #[command(flatten)]
    db: DbArg,
    /// What the question is about, comma separated: `account,device,ip`.
    #[arg(long, value_delimiter = ',', required = true)]
    entities: Vec<String>,
}

#[derive(Args)]
pub struct PathArgs {
    #[command(flatten)]
    library: LibraryArgs,
    /// The concept to start from.
    #[arg(long)]
    from: String,
    /// The concept to reach.
    #[arg(long)]
    to: String,
    /// Most relationships on a path (at most 8).
    #[arg(long, default_value_t = 4)]
    max_hops: usize,
    /// Also find the ways through this graph's labels and relationship types.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
}

pub fn ontology(command: OntologyCommand) -> Result<Value, CliError> {
    match command {
        OntologyCommand::Describe(args) => describe(args),
        OntologyCommand::Resolve(args) => resolve(args),
        OntologyCommand::Check(args) => check(args),
        OntologyCommand::Path(args) => path(args),
    }
}

/// The graph's schema, through the window when it has the graph open.
fn schema(path: &Path) -> Result<GraphSchema, CliError> {
    let value = match Graph::open(path, AccessMode::ReadOnly, PROGRAM) {
        Ok(mut graph) => return Ok(graph.schema()?),
        Err(OpenError::Locked(owner)) => dash::schema_via_window(path).unwrap_or_else(|| Err(OpenError::Locked(owner).into()))?,
        Err(e) => return Err(e.into()),
    };
    serde_json::from_value(value).map_err(|e| CliError::new("schema", format!("the window's schema is not usable: {e}")))
}

/// The statement to run, and what was found about it: rewritten into the
/// graph's labels and types when `semantic`, and checked against the graph
/// either way. A concept nothing in the graph holds exits 3; a statement that
/// cannot match (an unknown label, a pattern the graph doesn't hold) exits 2,
/// with the issues under `error.detail`. Warnings don't stop it.
pub(crate) fn prepare(path: &Path, text: &str, semantic: bool, extra: &[PathBuf]) -> Result<(String, Value), CliError> {
    let schema = schema(path)?;
    let mut report = json!({});
    let text = if semantic {
        let library = Library::load(extra);
        report = header(&library);
        match khonsu_core::semantic::rewrite(&library, &schema, text) {
            Ok(rewritten) => {
                report["replacements"] = json!(rewritten.replacements);
                report["drafts"] = json!(rewritten.drafts);
                rewritten.text
            }
            Err(unanswerable) => {
                report["missing"] = json!(unanswerable.missing);
                return Err(CliError {
                    kind: "not_investigable".into(),
                    message: format!("the graph holds nothing of {}", unanswerable.missing.join(", ")),
                    detail: Box::new(report),
                    code: NOT_INVESTIGABLE,
                });
            }
        }
    } else {
        text.to_string()
    };
    let issues = khonsu_core::semantic::validate(&schema, &text);
    let valid = !issues.iter().any(|issue| issue.severity == "error");
    report["statement"] = json!(text);
    report["valid"] = json!(valid);
    report["issues"] = json!(issues);
    if !valid {
        let first = issues.iter().find(|issue| issue.severity == "error").map(|issue| issue.message.clone()).unwrap_or_default();
        return Err(CliError {
            kind: "invalid_query".into(),
            message: format!("the statement cannot match this graph: {first}"),
            detail: Box::new(report),
            code: 2,
        });
    }
    Ok((text, report))
}

/// What every answer carries about the library it came from.
fn header(library: &Library) -> Value {
    let mut out = json!({ "ontology_versions": library.versions });
    if !library.problems.is_empty() {
        out["problems"] = json!(library.problems);
    }
    out
}

fn merge(mut base: Value, more: Value) -> Value {
    if let (Some(base), Value::Object(more)) = (base.as_object_mut(), more) {
        base.extend(more);
    }
    base
}

fn status(status: Status) -> Value {
    json!(status)
}

fn describe(args: DescribeArgs) -> Result<Value, CliError> {
    let library = Library::load(&args.library.ontology);
    let counts = match &args.db {
        Some(db) => Some(schema(db)?),
        None => None,
    };
    let count_of = |label: &str| counts.as_ref().map(|s| s.labels.iter().find(|l| l.name == label).map_or(0, |l| l.count));

    if let Some(name) = &args.concept {
        let declared = library.concept(name).ok_or_else(|| {
            let mut err = CliError::new("not_found", format!("no concept {name:?}"));
            err.detail = Box::new(json!({ "suggestions": ontology::suggestions(&library, name) }));
            err
        })?;
        let concept = &declared.value;
        let ancestors = library.ancestors(&concept.name);
        let relationships = |outgoing: bool| -> Vec<Value> {
            library
                .relationships()
                .filter(|r| ancestors.iter().any(|a| a.eq_ignore_ascii_case(if outgoing { &r.value.from } else { &r.value.to })))
                .map(|r| json!({ "name": r.value.name, "from": r.value.from, "to": r.value.to, "description": r.value.description, "status": r.value.status, "types": library.types_for(&r.value.name).into_iter().map(|(t, _)| t).collect::<Vec<_>>() }))
                .collect()
        };
        let labels: Vec<Value> = library
            .labels_for(&concept.name)
            .into_iter()
            .map(|(label, mapped)| {
                let mut out = json!({ "label": label, "concept": mapped.to, "mapping": mapped.mapping, "status": mapped.status });
                if let Some(count) = count_of(&label) {
                    out["count"] = json!(count);
                }
                out
            })
            .collect();
        return Ok(merge(
            header(&library),
            json!({
                "concept": {
                    "name": concept.name,
                    "ontology": declared.ontology,
                    "description": concept.description,
                    "synonyms": concept.synonyms,
                    "status": status(concept.status),
                    "extends": concept.extends,
                    "ancestors": &ancestors[1..],
                    "extended_by": library.descendants(&concept.name).into_iter().filter(|d| !d.eq_ignore_ascii_case(&concept.name)).collect::<Vec<_>>(),
                    "relationships_out": relationships(true),
                    "relationships_in": relationships(false),
                    "labels": labels,
                },
                "drafts": library.drafts(std::slice::from_ref(&concept.name), &[]),
            }),
        ));
    }

    let concepts: Vec<Value> = library
        .concepts()
        .map(|c| {
            let concept = &c.value;
            if args.brief {
                return json!({ "name": concept.name, "extends": concept.extends, "status": concept.status });
            }
            json!({
                "name": concept.name,
                "ontology": c.ontology,
                "extends": concept.extends,
                "description": concept.description,
                "synonyms": concept.synonyms,
                "status": concept.status,
                "labels": library.labels_for(&concept.name).into_iter().filter(|(_, m)| m.to.eq_ignore_ascii_case(&concept.name)).map(|(label, _)| label).collect::<Vec<_>>(),
            })
        })
        .collect();
    let relationships: Vec<Value> = library
        .relationships()
        .map(|r| {
            let relationship = &r.value;
            if args.brief {
                return json!({ "name": relationship.name, "from": relationship.from, "to": relationship.to, "status": relationship.status });
            }
            json!({
                "name": relationship.name,
                "ontology": r.ontology,
                "from": relationship.from,
                "to": relationship.to,
                "description": relationship.description,
                "synonyms": relationship.synonyms,
                "status": relationship.status,
                "types": library.types_for(&relationship.name).into_iter().map(|(t, _)| t).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut out = merge(header(&library), json!({ "concepts": concepts, "relationships": relationships }));
    if let Some(schema) = &counts {
        let mapped: Vec<Value> = schema
            .labels
            .iter()
            .filter(|l| !library.label(&l.name).is_empty())
            .map(|l| json!({ "label": l.name, "count": l.count, "concepts": library.label(&l.name).iter().map(|m| &m.to).collect::<Vec<_>>() }))
            .collect();
        let unmapped: Vec<Value> = schema.labels.iter().filter(|l| library.label(&l.name).is_empty()).map(|l| json!({ "label": l.name, "count": l.count })).collect();
        let absent: Vec<&String> = library.mapped_labels().filter(|label| !schema.labels.iter().any(|l| &&l.name == label && l.count > 0)).collect();
        let unmapped_types: Vec<&String> = schema.relationship_types.iter().filter(|t| library.rel_type(&t.name).is_empty()).map(|t| &t.name).collect();
        out["graph"] = json!({ "mapped": mapped, "unmapped": unmapped, "unmapped_types": unmapped_types, "mapped_but_absent": absent });
    }
    Ok(out)
}

fn resolve(args: ResolveArgs) -> Result<Value, CliError> {
    let library = Library::load(&args.library.ontology);
    let words: Vec<Value> = args
        .words
        .iter()
        .map(|word| {
            let matches = ontology::resolve(&library, word);
            let mut out = json!({ "word": word, "matches": matches });
            if matches.is_empty() {
                out["suggestions"] = json!(ontology::suggestions(&library, word));
            } else {
                let labels: Vec<String> = matches
                    .iter()
                    .filter(|m| m.kind == "concept")
                    .flat_map(|m| library.labels_for(&m.name).into_iter().map(|(label, _)| label))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let types: Vec<String> = matches
                    .iter()
                    .filter(|m| m.kind == "relationship")
                    .flat_map(|m| library.types_for(&m.name).into_iter().map(|(t, _)| t))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                out["labels"] = json!(labels);
                out["types"] = json!(types);
            }
            out
        })
        .collect();
    Ok(merge(header(&library), json!({ "words": words })))
}

fn check(args: CheckArgs) -> Result<Value, CliError> {
    let library = Library::load(&args.library.ontology);
    let path = resolve_db(&args.db)?;
    let schema = schema(&path)?;
    let terms: Vec<String> = args.entities.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Err(CliError::usage("--entities names nothing"));
    }
    let coverage = ontology::check(&library, &schema, &terms);
    let out = merge(header(&library), json!({ "graph": path, "coverage": coverage, "investigable": coverage.complete() }));
    if coverage.complete() {
        return Ok(out);
    }
    let missing: Vec<String> = coverage.terms.iter().filter_map(|t| t.missing.as_ref().map(|why| format!("{} ({why})", t.term))).collect();
    Err(CliError {
        kind: "not_investigable".into(),
        message: format!("the graph holds nothing for {}", missing.join(", ")),
        detail: Box::new(out),
        code: NOT_INVESTIGABLE,
    })
}

fn path(args: PathArgs) -> Result<Value, CliError> {
    if args.max_hops == 0 || args.max_hops > MAX_HOPS {
        return Err(CliError::usage(format!("--max-hops must be 1 to {MAX_HOPS}")));
    }
    let library = Library::load(&args.library.ontology);
    for name in [&args.from, &args.to] {
        if library.concept(name).is_none() {
            let mut err = CliError::new("not_found", format!("no concept {name:?}; `khonsu graph ontology resolve {name}` says what it may mean"));
            err.detail = Box::new(json!({ "suggestions": ontology::suggestions(&library, name) }));
            return Err(err);
        }
    }
    let declared = ontology::declared_paths(&library, &args.from, &args.to, args.max_hops);
    let relationships: Vec<String> = declared.iter().flat_map(|p| p.steps.iter().map(|s| s.via.clone())).collect();
    let mut out = merge(
        header(&library),
        json!({
            "from": args.from,
            "to": args.to,
            "max_hops": args.max_hops,
            "declared": declared,
            "drafts": library.drafts(&[args.from.clone(), args.to.clone()], &relationships),
        }),
    );
    if let Some(db) = &args.db {
        let schema = schema(db)?;
        let found = ontology::graph_paths(&library, &schema, &args.from, &args.to, args.max_hops);
        out["graph"] = json!({
            "db": db,
            "from_labels": library.labels_for(&args.from).into_iter().map(|(l, _)| l).collect::<Vec<_>>(),
            "to_labels": library.labels_for(&args.to).into_iter().map(|(l, _)| l).collect::<Vec<_>>(),
            "paths": found,
        });
    }
    Ok(out)
}

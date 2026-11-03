//! `khonsu snapshot`: keep a graph's records in a file, and compare two. See
//! `khonsu_core::snapshot`.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use khonsu_core::snapshot::{self, Snapshot};
use khonsu_core::{normalize_path, settings, AccessMode, Graph};
use serde_json::{json, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Subcommand)]
pub enum SnapshotCommand {
    /// Write the graph's records to a file. The graph is not changed.
    Save(SaveArgs),
    /// What differs between two snapshots, by stable IDs.
    Diff(DiffArgs),
    /// Whether the graph still holds what a snapshot does.
    Check(CheckArgs),
}

#[derive(Args)]
pub struct SaveArgs {
    /// The file to write, e.g. `monday.snapshot.json`.
    file: PathBuf,
    #[command(flatten)]
    db: DbArg,
    /// Replace the file if it exists.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct DiffArgs {
    /// The earlier snapshot.
    before: PathBuf,
    /// The later snapshot.
    after: PathBuf,
    /// Changes to list; `truncated` counts the rest.
    #[arg(long, default_value_t = snapshot::DEFAULT_MAX_CHANGES)]
    max_changes: usize,
}

#[derive(Args)]
pub struct CheckArgs {
    /// The snapshot.
    file: PathBuf,
    #[command(flatten)]
    db: DbArg,
}

pub fn snapshot(command: SnapshotCommand) -> Result<Value, CliError> {
    match command {
        SnapshotCommand::Save(args) => {
            let path = resolve_db(&args.db)?;
            let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
            let taken = graph.snapshot(&settings::load_sources())?;
            taken.save(&args.file, args.force)?;
            Ok(json!({
                "saved": normalize_path(&args.file),
                "snapshot_id": taken.snapshot_id,
                "captured_at": taken.captured_at,
                "nodes": taken.nodes.len(),
                "relationships": taken.relationships.len(),
                "unstable_nodes": taken.unstable_nodes,
                "unstable_relationships": taken.unstable_relationships,
                "sources": taken.sources,
            }))
        }
        SnapshotCommand::Diff(args) => {
            let (before, after) = (Snapshot::load(&args.before)?, Snapshot::load(&args.after)?);
            Ok(json!(snapshot::diff(&before, &after, args.max_changes)))
        }
        SnapshotCommand::Check(args) => {
            let saved = Snapshot::load(&args.file)?;
            let path = resolve_db(&args.db)?;
            let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
            let (state, differences) = saved.check(&mut graph)?;
            Ok(json!({
                "snapshot_id": saved.snapshot_id,
                "captured_at": saved.captured_at,
                "graph_state": state,
                "differences": differences,
                "sources": saved.sources,
            }))
        }
    }
}

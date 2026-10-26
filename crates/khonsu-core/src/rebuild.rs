//! Making an old graph again from its data sources.
//!
//! HawDB 0.12 rejects the project format 0.8–0.11 wrote and offers no
//! migration (its storage spec keeps no compatibility for earlier formats).
//! Everything KhonsuGraph puts in a graph comes from a data source, so the
//! graph can be made again: the old directory is moved aside, kept whole, a
//! new graph is created at the same path, and every source configured for
//! that path syncs into it in full. Sources name their graph by path, so
//! they go on working. What was written by hand (`query --write`) is not in
//! any source and is not carried over.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::engine::{is_old_format, normalize_path, Graph, OpenError};
use crate::plugin::{self, SyncEvent, SyncReport};
use crate::protocol::SyncMode;
use crate::settings::{self, Source};

#[derive(Debug, Serialize)]
pub struct RebuildReport {
    pub graph: PathBuf,
    /// Where the old graph now is, untouched.
    pub backup: PathBuf,
    pub synced: Vec<SyncReport>,
    /// Sources whose sync failed, with why; the graph holds the others.
    pub failed: Vec<FailedSource>,
}

#[derive(Debug, Serialize)]
pub struct FailedSource {
    pub source: String,
    pub error: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("{} is not a graph in the old HawDB format; nothing to rebuild", .0.display())]
    NotOld(PathBuf),
    #[error("{} has no data sources to rebuild from (see `khonsu source list`)", .0.display())]
    NoSources(PathBuf),
    /// Checked before anything moves: a graph is rebuilt once, so a source
    /// that cannot sync would leave its data out for good.
    #[error("plugin {} is not available (see `khonsu plugin list`); {} is rebuilt only once, so its sources need their plugins first", .plugins.join(", "), .graph.display())]
    MissingPlugins { graph: PathBuf, plugins: Vec<String> },
    #[error("cannot move the old graph aside to {}: {error}", .backup.display())]
    Backup { backup: PathBuf, error: std::io::Error },
    #[error(transparent)]
    Open(#[from] OpenError),
}

/// The data sources that write to the graph at `path`.
pub fn sources_of(path: &Path) -> Vec<Source> {
    let path = normalize_path(path);
    settings::load_sources()
        .into_iter()
        .filter(|s| normalize_path(&s.graph) == path)
        .collect()
}

/// The plugins `sources` use that are not installed, each named once.
pub fn missing_plugins(sources: &[Source]) -> Vec<String> {
    let installed: Vec<String> = plugin::list().iter().map(|p| p.id().to_string()).collect();
    let mut missing: Vec<String> = Vec::new();
    for source in sources {
        if !installed.contains(&source.plugin) && !missing.contains(&source.plugin) {
            missing.push(source.plugin.clone());
        }
    }
    missing
}

/// `graph` beside itself as `<name>.hawdb-old`, numbered if that is taken.
fn backup_path(graph: &Path) -> PathBuf {
    let name = graph.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "graph".into());
    let mut candidate = graph.with_file_name(format!("{name}.hawdb-old"));
    let mut n = 2;
    while candidate.exists() {
        candidate = graph.with_file_name(format!("{name}.hawdb-old-{n}"));
        n += 1;
    }
    candidate
}

/// Rebuild the old-format graph at `path` from its data sources (see the
/// module). A source that fails is reported and the rest still sync.
pub fn rebuild(
    path: &Path,
    program: &str,
    cancel: &AtomicBool,
    mut on_event: impl FnMut(&Source, SyncEvent),
) -> Result<RebuildReport, RebuildError> {
    let graph_path = normalize_path(path);
    if !is_old_format(&graph_path) {
        return Err(RebuildError::NotOld(graph_path));
    }
    let sources = sources_of(&graph_path);
    if sources.is_empty() {
        return Err(RebuildError::NoSources(graph_path));
    }
    let plugins = missing_plugins(&sources);
    if !plugins.is_empty() {
        return Err(RebuildError::MissingPlugins { graph: graph_path, plugins });
    }
    let backup = backup_path(&graph_path);
    std::fs::rename(&graph_path, &backup).map_err(|error| RebuildError::Backup { backup: backup.clone(), error })?;
    let mut graph = Graph::create(&graph_path, program)?;
    let (mut synced, mut failed) = (Vec::new(), Vec::new());
    for source in sources {
        // From scratch: no cursor, and no relationship shapes from the old
        // graph to look for stale records in.
        let fresh = Source {
            cursor: None,
            patterns: Vec::new(),
            ..source.clone()
        };
        match plugin::sync_source(&mut graph, &fresh, SyncMode::Full, cancel, |event| on_event(&source, event)) {
            Ok(report) => synced.push(report),
            Err(error) => failed.push(FailedSource {
                source: source.id.clone(),
                error: error.to_string(),
            }),
        }
    }
    Ok(RebuildReport {
        graph: graph_path,
        backup,
        synced,
        failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backup_never_overwrites_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph");
        assert_eq!(backup_path(&graph), dir.path().join("graph.hawdb-old"));
        std::fs::create_dir(dir.path().join("graph.hawdb-old")).unwrap();
        assert_eq!(backup_path(&graph), dir.path().join("graph.hawdb-old-2"));
    }

    #[test]
    fn a_missing_plugin_is_named_once() {
        let source = |id: &str, plugin: &str| Source {
            id: id.into(),
            name: id.into(),
            plugin: plugin.into(),
            graph: PathBuf::from("graph"),
            config: Default::default(),
            secret_keys: Vec::new(),
            cursor: None,
            patterns: Vec::new(),
            last_sync: None,
        };
        let sources = [source("a", "no-such-plugin"), source("b", "no-such-plugin")];
        assert_eq!(missing_plugins(&sources), ["no-such-plugin"]);
        assert!(missing_plugins(&[]).is_empty());
    }

    #[test]
    fn only_the_old_format_is_old() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.hawdb"), "HAWDB_BRANCH_PROJECT_V1\nproject_id\tx\n").unwrap();
        assert!(is_old_format(dir.path()));
        std::fs::write(dir.path().join("manifest.hawdb"), "HAWDB_BRANCH_PROJECT_V2\nproject_id\tx\n").unwrap();
        assert!(!is_old_format(dir.path()));
        assert!(!is_old_format(&dir.path().join("missing")));
    }
}

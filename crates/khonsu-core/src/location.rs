//! What a folder is, before anyone tries to open it, and where a new graph can
//! go. A graph is a folder, which is not obvious, so the window checks a choice
//! here and can say what was picked instead of showing the engine's error.

use crate::engine::{is_graph_dir, Graph, OpenError};
use std::fs;
use std::path::{Path, PathBuf};

/// Graphs named when a folder holds some. More would only make the message long.
const LISTED_GRAPHS: usize = 5;

/// What a path chosen as a graph turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderCheck {
    /// A graph. It may still fail to open, if it is in use, say.
    Graph,
    Missing,
    /// A file, not a folder. `csv` says it looks like data to import.
    File { csv: bool },
    /// A folder inside the graph at this path.
    InsideGraph(PathBuf),
    /// A folder holding these graphs, and not one itself.
    HoldsGraphs(Vec<PathBuf>),
    NotAGraph,
}

/// Looks at `path` without opening it or changing anything.
pub fn check_graph_folder(path: &Path) -> FolderCheck {
    if !path.exists() {
        return FolderCheck::Missing;
    }
    if path.is_file() {
        let csv = path.extension().is_some_and(|ext| {
            let ext = ext.to_string_lossy();
            ext.eq_ignore_ascii_case("csv") || ext.eq_ignore_ascii_case("tsv")
        });
        return FolderCheck::File { csv };
    }
    if is_graph_dir(path) {
        return FolderCheck::Graph;
    }
    if let Some(graph) = path.ancestors().skip(1).find(|ancestor| is_graph_dir(ancestor)) {
        return FolderCheck::InsideGraph(graph.to_path_buf());
    }
    let mut graphs: Vec<PathBuf> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|child| is_graph_dir(child))
        .collect();
    if graphs.is_empty() {
        return FolderCheck::NotAGraph;
    }
    graphs.sort();
    graphs.truncate(LISTED_GRAPHS);
    FolderCheck::HoldsGraphs(graphs)
}

/// Why a new graph can't take the name and place asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewGraphProblem {
    NoName,
    /// The name can't be a folder name on every system we run on.
    BadName,
    NoLocation,
    LocationMissing,
    /// Something is already there, and a graph needs a new or empty folder.
    Taken(PathBuf),
}

/// The folder a graph called `name` gets under `location`.
pub fn new_graph_path(location: &Path, name: &str) -> Result<PathBuf, NewGraphProblem> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NewGraphProblem::NoName);
    }
    if !valid_folder_name(name) {
        return Err(NewGraphProblem::BadName);
    }
    if location.as_os_str().is_empty() {
        return Err(NewGraphProblem::NoLocation);
    }
    if !location.is_dir() {
        return Err(NewGraphProblem::LocationMissing);
    }
    let path = location.join(name);
    let free = match fs::read_dir(&path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !path.exists(),
    };
    if free {
        Ok(path)
    } else {
        Err(NewGraphProblem::Taken(path))
    }
}

fn valid_folder_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
        "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or(name);
    name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        && !RESERVED.iter().any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// Whether a graph can really be made under `location`, by making one and
/// removing it. A writable folder isn't enough: on Windows HawDB also needs
/// write access to every parent folder up to the drive root.
pub fn probe_location(location: &Path, program: &str) -> Result<(), OpenError> {
    let probe = location.join(format!(".khonsu-probe-{}", std::process::id()));
    let graph = Graph::create(&probe, program)?;
    drop(graph);
    fs::remove_dir_all(&probe)?;
    Ok(())
}

/// The first place a graph can be made: beside a recent graph, then the usual
/// folders under `home`. `None` when none of them work, as on a Windows machine
/// with only drive C, so the person has to choose.
pub fn suggest_location(recent: &[PathBuf], home: Option<&Path>, program: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = recent.iter().filter_map(|path| path.parent().map(Path::to_path_buf)).collect();
    if let Some(home) = home {
        candidates.push(home.join("Documents"));
        candidates.push(home.to_path_buf());
    }
    let mut tried: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if tried.contains(&candidate) {
            continue;
        }
        if candidate.is_dir() && probe_location(&candidate, program).is_ok() {
            return Some(candidate);
        }
        tried.push(candidate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let base = std::env::var_os("KHONSU_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    fn fake_graph(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("manifest.hawdb"), "x").unwrap();
    }

    #[test]
    fn a_choice_is_named_for_what_it_is() {
        let dir = scratch();
        let root = dir.path();
        assert_eq!(check_graph_folder(&root.join("nope")), FolderCheck::Missing);
        assert_eq!(check_graph_folder(root), FolderCheck::NotAGraph);

        fs::write(root.join("people.CSV"), "a,b").unwrap();
        fs::write(root.join("notes.txt"), "a").unwrap();
        assert_eq!(check_graph_folder(&root.join("people.CSV")), FolderCheck::File { csv: true });
        assert_eq!(check_graph_folder(&root.join("notes.txt")), FolderCheck::File { csv: false });

        let graph = root.join("shop");
        fake_graph(&graph);
        fs::create_dir_all(graph.join("internal")).unwrap();
        assert_eq!(check_graph_folder(&graph), FolderCheck::Graph);
        assert_eq!(check_graph_folder(&graph.join("internal")), FolderCheck::InsideGraph(graph.clone()));
        assert_eq!(check_graph_folder(root), FolderCheck::HoldsGraphs(vec![graph]));
    }

    #[test]
    fn a_folder_of_many_graphs_lists_only_a_few() {
        let dir = scratch();
        for ix in 0..LISTED_GRAPHS + 2 {
            fake_graph(&dir.path().join(format!("g{ix}")));
        }
        let FolderCheck::HoldsGraphs(graphs) = check_graph_folder(dir.path()) else {
            panic!("expected the graphs to be found");
        };
        assert_eq!(graphs.len(), LISTED_GRAPHS);
    }

    #[test]
    fn a_new_graph_needs_a_usable_name_and_place() {
        let dir = scratch();
        let root = dir.path();
        assert_eq!(new_graph_path(root, "  "), Err(NewGraphProblem::NoName));
        for bad in ["a/b", "a\\b", "..", "a:b", "CON", "con.txt", "trailing.", "a?"] {
            assert_eq!(new_graph_path(root, bad), Err(NewGraphProblem::BadName), "{bad}");
        }
        assert_eq!(new_graph_path(Path::new(""), "shop"), Err(NewGraphProblem::NoLocation));
        assert_eq!(new_graph_path(&root.join("nope"), "shop"), Err(NewGraphProblem::LocationMissing));
        assert_eq!(new_graph_path(root, " shop "), Ok(root.join("shop")));

        // An empty folder is free; one with anything in it is not.
        fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(new_graph_path(root, "empty"), Ok(root.join("empty")));
        fs::create_dir_all(root.join("full")).unwrap();
        fs::write(root.join("full").join("x"), "x").unwrap();
        assert_eq!(new_graph_path(root, "full"), Err(NewGraphProblem::Taken(root.join("full"))));
        fs::write(root.join("file"), "x").unwrap();
        assert_eq!(new_graph_path(root, "file"), Err(NewGraphProblem::Taken(root.join("file"))));
    }

    #[test]
    fn a_probe_makes_a_real_graph_and_leaves_nothing() {
        let dir = scratch();
        probe_location(dir.path(), "test").unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_suggestion_skips_what_cannot_be_used() {
        let dir = scratch();
        let root = dir.path();
        let gone = root.join("gone").join("graph");
        let beside = root.join("work").join("graph");
        fs::create_dir_all(beside.parent().unwrap()).unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();

        assert_eq!(suggest_location(&[gone.clone(), beside], Some(&home), "test"), Some(root.join("work")));
        // No Documents folder under this home, so the home itself.
        assert_eq!(suggest_location(&[gone], Some(&home), "test"), Some(home));
        assert_eq!(suggest_location(&[], None, "test"), None);
    }
}

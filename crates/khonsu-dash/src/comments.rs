//! Review comments on a dashboard: threads pinned to its plots, for the
//! person looking at the dashboard and the agent that wrote it to talk over.
//!
//! The threads live beside the spec, in `NAME.dash.comments.json`, rather than
//! in the window or the history store: an agent reads and answers them with
//! `khonsu comments` (or just reads the file) without a window running,
//! and they travel with the spec the way its `source` files do. The window
//! writes a comment the moment it is posted and watches the file, so a reply
//! the agent writes shows up on the plot it is about.
//!
//! Both sides write the whole file, so every change is a read-modify-write
//! from disk — never from a copy held since — and two writers only race over
//! the few milliseconds between reading and writing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::model;

/// Who wrote a comment: the person in the window, or an agent over the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    User,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub author: Author,
    /// Which agent wrote it — Claude, Codex, DeepSeek… — as `presence` names
    /// it, for its avatar. Absent on the person's comments and on agents'
    /// comments written before agents were named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
    /// When it was written, RFC 3339 in local time.
    pub at: String,
}

/// One conversation about one plot. Resolved threads stay in the file, out of
/// the way, so what was asked and what was done remains on record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// The plot's name, as in `plot "name"`.
    pub plot: String,
    #[serde(default)]
    pub resolved: bool,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Comments {
    #[serde(default)]
    pub threads: Vec<Thread>,
}

impl Comments {
    /// The threads on one plot, open ones first, each group oldest first.
    pub fn on_plot<'a>(&'a self, plot: &str) -> Vec<&'a Thread> {
        let mut threads: Vec<&Thread> = self.threads.iter().filter(|t| t.plot == plot).collect();
        threads.sort_by_key(|t| t.resolved);
        threads
    }

    /// How many unresolved threads a plot has.
    pub fn open_on(&self, plot: &str) -> usize {
        self.threads
            .iter()
            .filter(|t| t.plot == plot && !t.resolved)
            .count()
    }

    /// Start a thread on `plot`; returns its id.
    pub fn add(&mut self, plot: &str, author: Author, text: &str) -> String {
        let next = self
            .threads
            .iter()
            .filter_map(|t| t.id.strip_prefix('c')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("c{next}");
        self.threads.push(Thread {
            id: id.clone(),
            plot: plot.to_string(),
            resolved: false,
            comments: vec![comment(author, text)],
        });
        id
    }

    /// Answer a thread. A reply to a resolved thread reopens it: it is being
    /// talked about again.
    pub fn reply(&mut self, id: &str, author: Author, text: &str) -> Result<(), String> {
        let thread = self.thread_mut(id)?;
        thread.comments.push(comment(author, text));
        thread.resolved = false;
        Ok(())
    }

    pub fn set_resolved(&mut self, id: &str, resolved: bool) -> Result<(), String> {
        self.thread_mut(id)?.resolved = resolved;
        Ok(())
    }

    fn thread_mut(&mut self, id: &str) -> Result<&mut Thread, String> {
        self.threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("No comment thread {id:?}"))
    }
}

fn comment(author: Author, text: &str) -> Comment {
    Comment {
        author,
        // An agent's comment is written by its own command, whose
        // environment says which agent it is.
        name: (author == Author::Agent).then(crate::presence::agent_name),
        text: text.trim().to_string(),
        at: khonsu_core::util::format_rfc3339(khonsu_core::util::unix_now()),
    }
}

/// Where a spec's comments live: `orders.dash` → `orders.dash.comments.json`.
pub fn path_for(spec: &Path) -> PathBuf {
    let mut name = spec.file_name().map(OsString::from).unwrap_or_default();
    name.push(".comments.json");
    spec.with_file_name(name)
}

/// The comments on a spec. No file yet is no comments, not an error.
pub fn load(spec: &Path) -> Result<Comments, String> {
    let path = path_for(spec);
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(Comments::default()),
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Comments::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn save(spec: &Path, comments: &Comments) -> Result<(), String> {
    let path = path_for(spec);
    let text = serde_json::to_string_pretty(comments).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

/// Load, change, save: every write starts from what is on disk now.
pub fn update<T>(
    spec: &Path,
    change: impl FnOnce(&mut Comments) -> Result<T, String>,
) -> Result<(T, Comments), String> {
    let mut comments = load(spec)?;
    let out = change(&mut comments)?;
    save(spec, &comments)?;
    Ok((out, comments))
}

/// A thread as an agent reads it: with the plot it is on — its title, query
/// and the lines of its `plot` block — so the change asked for can be made.
pub fn describe(source: &str, thread: &Thread) -> Json {
    let plots = plot_sites(source);
    let site = plots.iter().find(|p| p.name == thread.plot);
    json!({
        "id": thread.id,
        "plot": thread.plot,
        "plot_title": site.and_then(|s| s.title.clone()),
        "plot_query": site.and_then(|s| s.query.clone()),
        "plot_lines": site.map(|s| [s.line, s.end_line]),
        "plot_missing": site.is_none(),
        "resolved": thread.resolved,
        "comments": thread.comments,
    })
}

/// Where a `plot` block sits, for an agent to find what a thread is about.
struct PlotSite {
    name: String,
    title: Option<String>,
    query: Option<String>,
    line: usize,
    end_line: usize,
}

/// The plot blocks of a spec, read leniently: a spec that does not parse
/// lists no plots, and the threads are still listed by name.
/// The names of a spec's plots, read leniently like [`plot_sites`].
/// The names of a spec's plots, read leniently like [`plot_sites`].
pub fn plot_names(source: &str) -> Vec<String> {
    plot_sites(source)
        .into_iter()
        .map(|site| site.name)
        .collect()
}

fn plot_sites(source: &str) -> Vec<PlotSite> {
    let Ok(file) = model::parse(source) else {
        return Vec::new();
    };
    let spec = model::validate(&file).ok();
    file.blocks
        .iter()
        .filter(|b| b.kind == "plot")
        .map(|b| {
            let plot = spec
                .as_ref()
                .and_then(|s| s.plots.iter().find(|p| p.name == b.name));
            PlotSite {
                name: b.name.clone(),
                title: plot.and_then(|p| p.title.clone()),
                query: plot.map(|p| p.query.clone()),
                line: b.line,
                end_line: b.end_line,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comments_file_sits_beside_the_spec() {
        assert_eq!(
            path_for(Path::new("dir/orders.dash")),
            PathBuf::from("dir/orders.dash.comments.json")
        );
    }

    #[test]
    fn threads_number_on_and_a_reply_reopens() {
        let mut comments = Comments::default();
        let a = comments.add("revenue", Author::User, "  Sort the bars  ");
        let b = comments.add("orders", Author::User, "Weekly, not daily?");
        assert_eq!((a.as_str(), b.as_str()), ("c1", "c2"));
        assert_eq!(comments.threads[0].comments[0].text, "Sort the bars");

        comments.set_resolved(&a, true).unwrap();
        assert_eq!(comments.open_on("revenue"), 0);
        comments
            .reply(&a, Author::Agent, "Sorted descending")
            .unwrap();
        assert_eq!(comments.open_on("revenue"), 1);
        assert!(comments.reply("c9", Author::Agent, "?").is_err());

        // Resolved threads sort after open ones.
        comments.add("revenue", Author::User, "Another");
        comments.set_resolved("c1", true).unwrap();
        let ids: Vec<_> = comments
            .on_plot("revenue")
            .iter()
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(ids, ["c3", "c1"]);
    }

    #[test]
    fn the_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("khonsu_comments_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = dir.join("d.dash");
        assert_eq!(load(&spec).unwrap(), Comments::default());
        let (id, _) = update(&spec, |c| Ok(c.add("p", Author::User, "hi"))).unwrap();
        let loaded = load(&spec).unwrap();
        assert_eq!(loaded.threads[0].id, id);
        assert_eq!(loaded.threads[0].comments[0].author, Author::User);
        std::fs::remove_dir_all(&dir).ok();
    }
}

//! `.dash` commands and the window link: check, comments, presence, open.
//! After DuckLocal's commands of the same names.

use std::path::{Path, PathBuf};

use clap::Args;
use khonsu_core::{normalize_path, AccessMode, Graph, Language, OpenError};
use khonsu_dash::comments::{self, Author};
use khonsu_dash::presence::{self, Activity};
use khonsu_dash::remote::{self, CheckRequest, Message, OpenRequest, QueryRequest, StateRequest};
use serde_json::{json, Value};

use crate::{resolve_db, CliError, DbArg, PROGRAM};

#[derive(Args)]
pub struct CheckArgs {
    /// The .dash file to check.
    pub file: PathBuf,
    #[command(flatten)]
    pub db: DbArg,
    /// One line on success instead of the full report.
    #[arg(long)]
    pub summary: bool,
}

#[derive(Args)]
pub struct CommentsArgs {
    /// The .dash file the comments are on.
    pub file: PathBuf,
    /// Include resolved threads.
    #[arg(long)]
    pub all: bool,
    /// Answer a thread.
    #[arg(long, value_name = "ID")]
    pub reply: Option<String>,
    /// What the comment says (with --reply or --add).
    #[arg(long)]
    pub text: Option<String>,
    /// Resolve a thread; with --reply, resolve the one replied to.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub resolve: Option<String>,
    /// Reopen a resolved thread.
    #[arg(long, value_name = "ID")]
    pub reopen: Option<String>,
    /// Start a thread on a plot.
    #[arg(long, value_name = "PLOT")]
    pub add: Option<String>,
}

#[derive(Args)]
pub struct PresenceArgs {
    /// The .dash file being worked on.
    pub file: PathBuf,
    /// editing, reading, or idle (done).
    #[arg(long)]
    pub doing: String,
    /// Plots, comma-separated; with editing, plots about to be added may be named.
    #[arg(long, value_delimiter = ',')]
    pub plot: Vec<String>,
    /// The name shown; default KHONSU_AGENT, else the agent the environment names.
    #[arg(long)]
    pub agent: Option<String>,
}

#[derive(Args)]
pub struct OpenArgs {
    /// Graph directories and .dash files to open in the window.
    pub paths: Vec<PathBuf>,
    /// A Cypher query to show in a tab of its own.
    #[arg(long, conflicts_with = "sql")]
    pub cypher: Option<String>,
    /// A SQL query to show in a tab of its own.
    #[arg(long)]
    pub sql: Option<String>,
    /// Run the query in the window once shown.
    #[arg(long)]
    pub run: bool,
    /// Read what the window shows instead of opening anything; with a .dash
    /// path, only the tabs showing it.
    #[arg(long)]
    pub state: bool,
    /// With --state, only the tab the person is looking at.
    #[arg(long)]
    pub active: bool,
    /// Do not start a window when none is running.
    #[arg(long)]
    pub no_launch: bool,
}

fn absolute(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string()
}

fn window_answer(answer: Option<Result<Value, String>>) -> Result<Value, CliError> {
    match answer {
        None => Err(CliError::new("not_running", "no KhonsuGraph window is running")),
        Some(Err(message)) => Err(CliError::new("window", message)),
        Some(Ok(value)) => match value.get("error") {
            Some(error) => Err(CliError {
                kind: error["kind"].as_str().unwrap_or("window").to_string(),
                message: error["message"].as_str().unwrap_or_default().to_string(),
                detail: error.get("detail").cloned().unwrap_or(Value::Null),
                code: error["code"].as_u64().unwrap_or(1) as u8,
            }),
            None => Ok(value),
        },
    }
}

/// `khonsu check`: on the graph directly, or through the window when the
/// window has the graph open.
pub fn check(args: CheckArgs) -> Result<Value, CliError> {
    let db = resolve_db(&args.db)?;
    if !args.file.is_file() {
        return Err(CliError::new("io", format!("{}: no such file", args.file.display())));
    }
    let _checking = presence::Checking::start(&args.file);
    match Graph::open(&db, AccessMode::ReadOnly, PROGRAM) {
        Ok(mut graph) => khonsu_dash::check::check_file(&args.file, &mut graph, args.summary).map_err(|e| CliError {
            kind: e.kind.to_string(),
            code: e.exit_code(),
            message: e.message,
            detail: Value::Null,
        }),
        Err(OpenError::Locked(owner)) => {
            let answer = remote::request(&Message::Check(CheckRequest {
                graph: normalize_path(&db).display().to_string(),
                path: absolute(&args.file),
                summary: args.summary,
            }));
            match answer {
                None => Err(OpenError::Locked(owner).into()),
                other => window_answer(other),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// A query on the window's graph, for when the window holds the lease.
pub fn query_via_window(db: &Path, language: Language, text: &str, max_rows: usize) -> Option<Result<Value, CliError>> {
    let answer = remote::request(&Message::Query(QueryRequest {
        graph: normalize_path(db).display().to_string(),
        language,
        text: text.to_string(),
        max_rows,
    }))?;
    Some(window_answer(Some(answer)).map(|mut value| {
        value["via_window"] = json!(true);
        value
    }))
}

pub fn comments(args: CommentsArgs) -> Result<Value, CliError> {
    let file = &args.file;
    if !file.is_file() {
        return Err(CliError::new("io", format!("{}: no such file", file.display())));
    }
    let need_text = || {
        args.text
            .clone()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| CliError::usage("Missing --text: what the comment says"))
    };
    let failed = |message: String| {
        if message.starts_with("No comment thread") {
            CliError::usage(format!("{message}; list the threads with `khonsu comments {} --all`", file.display()))
        } else {
            CliError::new("io", message)
        }
    };
    let thread_plot = |id: &str| {
        comments::load(file)
            .ok()
            .and_then(|c| c.threads.into_iter().find(|t| t.id == id))
            .map(|t| t.plot)
    };
    let resolve_id = args.resolve.clone().filter(|id| !id.is_empty());
    let resolve = args.resolve.is_some();

    // The window marks the plot a reply or a new thread is on, and the plots
    // whose open threads a listing reads.
    let (activity, marked): (Activity, Vec<String>) = match (&args.reply, &args.add) {
        (Some(id), _) => (Activity::Replying, thread_plot(id).into_iter().collect()),
        (None, Some(plot)) => (Activity::Replying, vec![plot.clone()]),
        (None, None) if !resolve && args.reopen.is_none() => {
            let mut plots: Vec<String> = comments::load(file)
                .map(|c| c.threads.into_iter().filter(|t| args.all || !t.resolved).map(|t| t.plot).collect())
                .unwrap_or_default();
            plots.sort();
            plots.dedup();
            (Activity::Reading, plots)
        }
        _ => (Activity::Reading, Vec::new()),
    };
    if !marked.is_empty() {
        presence::announce(file, activity, marked);
    }

    let changed = if let Some(id) = &args.reply {
        let text = need_text()?;
        let (_, all) = comments::update(file, |c| {
            c.reply(id, Author::Agent, &text)?;
            if resolve {
                c.set_resolved(id, true)?;
            }
            Ok(())
        })
        .map_err(failed)?;
        Some((id.clone(), all))
    } else if resolve {
        let id = resolve_id.ok_or_else(|| CliError::usage("Name the thread to resolve: --resolve ID"))?;
        let (_, all) = comments::update(file, |c| c.set_resolved(&id, true)).map_err(failed)?;
        Some((id, all))
    } else if let Some(id) = &args.reopen {
        let (_, all) = comments::update(file, |c| c.set_resolved(id, false)).map_err(failed)?;
        Some((id.clone(), all))
    } else if let Some(plot) = &args.add {
        let text = need_text()?;
        let (id, all) = comments::update(file, |c| Ok(c.add(plot, Author::Agent, &text))).map_err(failed)?;
        Some((id, all))
    } else {
        if args.text.is_some() {
            return Err(CliError::usage("--text goes with --reply or --add"));
        }
        None
    };

    let source = std::fs::read_to_string(file).unwrap_or_default();
    Ok(match changed {
        Some((id, all)) => {
            let thread = all.threads.iter().find(|t| t.id == id);
            json!({ "thread": thread.map(|t| comments::describe(&source, t)) })
        }
        None => {
            let all = comments::load(file).map_err(|e| CliError::new("io", e))?;
            json!({
                "file": file.display().to_string(),
                "comments_file": comments::path_for(file).display().to_string(),
                "open": all.threads.iter().filter(|t| !t.resolved).count(),
                "threads": all.threads.iter().filter(|t| args.all || !t.resolved).map(|t| comments::describe(&source, t)).collect::<Vec<_>>(),
            })
        }
    })
}

pub fn presence(args: PresenceArgs) -> Result<Value, CliError> {
    let activity = Activity::parse(&args.doing)
        .ok_or_else(|| CliError::usage(format!("--doing {:?}: say reading, editing or idle", args.doing)))?;
    let source = std::fs::read_to_string(&args.file).map_err(|e| CliError::new("io", e.to_string()))?;
    // A plot the spec does not have would mark nothing; editing may name a
    // plot about to be added.
    if !args.plot.is_empty() && activity != Activity::Editing {
        let known = comments::plot_names(&source);
        if let Some(missing) = args.plot.iter().find(|plot| !known.contains(plot)) {
            return Err(CliError::usage(format!(
                "{}: no plot named {missing:?}; plots: {}",
                args.file.display(),
                known.join(", ")
            )));
        }
    }
    let delivered = presence::announce_as(&args.file, activity, args.plot, args.agent.unwrap_or_else(presence::agent_name));
    Ok(json!({ "delivered": delivered }))
}

/// The desktop app beside this executable, to start when no window runs.
fn app_executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = format!("khonsugraph{}", std::env::consts::EXE_SUFFIX);
    [dir.join(&name), dir.join("../MacOS/KhonsuGraph")].into_iter().find(|p| p.is_file())
}

pub fn open(args: OpenArgs) -> Result<Value, CliError> {
    if args.state {
        let path = args.paths.first().map(|p| absolute(p));
        return window_answer(remote::request(&Message::State(StateRequest { path, active: args.active })));
    }
    let (language, text) = match (args.cypher, args.sql) {
        (Some(text), _) => (Some(Language::Cypher), Some(text)),
        (_, Some(text)) => (Some(Language::Sql), Some(text)),
        _ => (None, None),
    };
    if args.paths.is_empty() && text.is_none() {
        return Err(CliError::usage("Name a graph or .dash file to open, or a query with --cypher or --sql"));
    }
    for path in &args.paths {
        if !path.exists() {
            return Err(CliError::new("io", format!("{}: no such file or directory", path.display())));
        }
    }
    let request = Message::Open(OpenRequest {
        paths: args.paths.iter().map(|p| absolute(p)).collect(),
        text,
        language,
        title: None,
        run: args.run,
    });
    if !remote::window_running() {
        if args.no_launch {
            return Err(CliError::new("not_running", "no KhonsuGraph window is running"));
        }
        let app = app_executable().ok_or_else(|| CliError::new("not_running", "no KhonsuGraph window is running, and the app was not found beside khonsu"))?;
        std::process::Command::new(&app)
            .spawn()
            .map_err(|e| CliError::new("launch", format!("{}: {e}", app.display())))?;
        // Wait for the window to start listening.
        let started = std::time::Instant::now();
        while !remote::window_running() {
            if started.elapsed() > std::time::Duration::from_secs(20) {
                return Err(CliError::new("launch", "the window did not start within 20 s"));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    window_answer(remote::request(&request))
}

#[cfg(test)]
mod tests {
    #[test]
    fn absolute_paths_stay_absolute() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(super::absolute(&here), here.display().to_string());
    }
}

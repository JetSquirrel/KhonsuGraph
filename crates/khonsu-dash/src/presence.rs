//! An agent's presence on a dashboard, after DuckLocal's: one name in the
//! corner while it works, and its edits marked on the plots until the person
//! has seen them.
//!
//! Every command that names a spec (`check`, `comments`, `presence`) tells
//! the running window, as one fire-and-forget line over [`crate::remote`].
//! The window works the rest out itself: it compares plots before and after
//! a change on disk and credits them to the agent heard from last.
//! `presence --doing editing --plot NAMES` is the one thing said ahead of
//! time: where an edit is about to land.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What an agent is doing to a plot, or to the whole dashboard when no plot
/// is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    Reading,
    Editing,
    Checking,
    Replying,
    /// Done: its marks go.
    Idle,
}

impl Activity {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "reading" => Some(Self::Reading),
            "editing" => Some(Self::Editing),
            "idle" | "done" => Some(Self::Idle),
            _ => None,
        }
    }
}

/// One line from an agent's command to the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    /// The spec, absolute: the window's working directory is not the
    /// command's.
    pub path: String,
    /// Who is at work, as the window labels it.
    pub agent: String,
    pub activity: Activity,
    /// The plots it is about; none means the dashboard as a whole.
    #[serde(default)]
    pub plots: Vec<String>,
}

/// An agent's work ends this long after its last command.
pub const QUIET_ENDS: Duration = Duration::from_secs(30);

/// The name the window shows for whoever runs this command: `KHONSU_AGENT`
/// when set, else the agent the environment gives away, else "Agent".
pub fn agent_name() -> String {
    if let Some(name) = std::env::var("KHONSU_AGENT").ok().filter(|name| !name.trim().is_empty()) {
        return name.trim().to_string();
    }
    let set = |name: &str| std::env::var_os(name).is_some();
    if set("CLAUDECODE") || set("CLAUDE_CODE_ENTRYPOINT") {
        "Claude".into()
    } else if set("CODEX_SANDBOX") || set("CODEX_HOME") {
        "Codex".into()
    } else if set("CURSOR_AGENT") || set("CURSOR_TRACE_ID") {
        "Cursor".into()
    } else if set("GEMINI_CLI") {
        "Gemini".into()
    } else {
        "Agent".into()
    }
}

fn absolute(spec: &Path) -> String {
    std::path::absolute(spec)
        .unwrap_or_else(|_| spec.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Tell the running window, if there is one, what this command is doing to
/// `spec`. Never waits on a window that is not there, and never fails the
/// command: presence is a courtesy to the person watching. Returns whether
/// a window received it.
pub fn announce(spec: &Path, activity: Activity, plots: Vec<String>) -> bool {
    announce_as(spec, activity, plots, agent_name())
}

pub fn announce_as(spec: &Path, activity: Activity, plots: Vec<String>, agent: String) -> bool {
    crate::remote::notify(&crate::remote::Message::Presence(Update {
        path: absolute(spec),
        agent,
        activity,
        plots,
    }))
}

/// Marks the whole dashboard as being checked while it lives.
pub struct Checking<'a> {
    spec: &'a Path,
}

impl<'a> Checking<'a> {
    pub fn start(spec: &'a Path) -> Self {
        announce(spec, Activity::Checking, Vec::new());
        Self { spec }
    }
}

impl Drop for Checking<'_> {
    fn drop(&mut self) {
        announce(self.spec, Activity::Idle, Vec::new());
    }
}

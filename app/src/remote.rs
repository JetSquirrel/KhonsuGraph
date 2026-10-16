//! The window's side of the line to `khonsu` commands (see
//! `khonsu_dash::remote`). Queries and checks run here, on the listener's
//! thread, against the window's own graph — HawDB leases a graph to one
//! process, so while the window has it this is the only way in. Presence,
//! open and state touch the views: they queue for the UI thread, which
//! drains them on a slow poll, as DuckLocal's window does.

use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use khonsu_core::QueryOptions;
use khonsu_dash::remote::{self, Message, ServeGuard};
use serde_json::{json, Map, Value};

/// Messages for the UI thread, each with where its answer goes.
static PENDING: Mutex<Vec<(Message, Sender<Value>)>> = Mutex::new(Vec::new());

/// How long a command waits for the UI thread before giving up.
const UI_ANSWER: Duration = Duration::from_secs(10);

fn error(kind: &str, message: impl Into<String>) -> Value {
    json!({"error": {"kind": kind, "message": message.into()}})
}

/// Whether `graph` names the graph the window has open.
fn is_open_graph(graph: &str) -> Result<(), Value> {
    let open = crate::db::with_graph(|g| Ok(g.path().to_path_buf()))
        .map_err(|_| error("no_graph", "the window has no graph open"))?;
    if khonsu_core::normalize_path(Path::new(graph)) == open {
        Ok(())
    } else {
        Err(error(
            "other_graph",
            format!("the window has {} open, not {graph}", open.display()),
        ))
    }
}

fn handle(message: Message) -> Value {
    match message {
        Message::Query(request) => {
            if let Err(e) = is_open_graph(&request.graph) {
                return e;
            }
            // Read-only through the window too: a write waits for --write on
            // a graph no window holds.
            if khonsu_dash::model::writing_keyword(request.language, &request.text).is_some() {
                return error("read_only", "queries through the window only read; close the graph in the window to write with --write");
            }
            match crate::db::with_graph(|g| g.run(request.language, &request.text, &Map::new(), QueryOptions { max_rows: request.max_rows })) {
                Ok(result) => serde_json::to_value(result).unwrap_or(Value::Null),
                Err(e) => json!({"error": {"kind": e.kind, "message": e.message, "detail": {"line": e.line, "column": e.column}}}),
            }
        }
        Message::Check(request) => {
            if let Err(e) = is_open_graph(&request.graph) {
                return e;
            }
            let path = Path::new(&request.path);
            let outcome = crate::db::with_graph(|g| {
                Ok(khonsu_dash::check::check_file(path, g, request.summary))
            });
            match outcome {
                Ok(Ok(report)) => report,
                Ok(Err(e)) => json!({"error": {"kind": e.kind, "message": e.message, "code": e.exit_code()}}),
                Err(e) => error(&e.kind, e.message),
            }
        }
        // The views answer these; the listener waits for them.
        other => {
            let (tx, rx) = channel();
            PENDING.lock().unwrap_or_else(PoisonError::into_inner).push((other, tx));
            rx.recv_timeout(UI_ANSWER)
                .unwrap_or_else(|_| error("busy", "the window did not answer in time"))
        }
    }
}

/// Start listening. Keep the guard for the life of the window.
pub fn serve() -> Option<ServeGuard> {
    match remote::serve(handle) {
        Ok(guard) => Some(guard),
        Err(e) => {
            tracing::warn!("khonsu commands cannot reach this window: {e}");
            None
        }
    }
}

/// The messages waiting for the UI thread.
pub fn take_pending() -> Vec<(Message, Sender<Value>)> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner))
}

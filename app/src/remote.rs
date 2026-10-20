//! The window's side of the line to `khonsu` commands (see
//! `khonsu_dash::remote`). Queries, writes, syncs, schema and checks run
//! here, on the listener's thread, against the window's own graph — HawDB
//! leases a graph to one process, so while the window has it this is the
//! only way in. A write or a sync leaves a note for the UI thread, which
//! rereads the schema and the sources and says what changed. Presence,
//! open and state touch the views: they queue for the UI thread, which
//! drains them on a slow poll, as DuckLocal's window does.

use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use khonsu_core::plugin;
use khonsu_core::protocol::SyncMode;
use khonsu_core::{settings, AccessMode, QueryOptions};
use khonsu_dash::remote::{self, Message, ServeGuard};
use serde_json::{json, Value};

/// Messages for the UI thread, each with where its answer goes.
static PENDING: Mutex<Vec<(Message, Sender<Value>)>> = Mutex::new(Vec::new());

/// What commands changed in the graph, for the UI thread to show.
static CHANGES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn changed(note: String) {
    CHANGES.lock().unwrap_or_else(PoisonError::into_inner).push(note);
}

/// The changes commands made since the last call.
pub fn take_changes() -> Vec<String> {
    std::mem::take(&mut *CHANGES.lock().unwrap_or_else(PoisonError::into_inner))
}

fn query_error(e: khonsu_core::QueryError) -> Value {
    json!({"error": {"kind": e.kind, "message": e.message, "detail": {"line": e.line, "column": e.column}}})
}

/// Writes need the window to have opened the graph for writing.
fn writable() -> Result<(), Value> {
    match crate::db::with_graph(|g| Ok(g.mode())) {
        Ok(AccessMode::ReadWrite) => Ok(()),
        Ok(AccessMode::ReadOnly) => Err(error("read_only", "the window opened this graph read-only; reopen it for writing there")),
        Err(e) => Err(query_error(e)),
    }
}

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
            // A write needs --write here too, as on a graph no window holds.
            let writing = khonsu_dash::model::writing_keyword(request.language, &request.text);
            if let Some(keyword) = &writing {
                if !request.write {
                    return error("read_only", format!("{keyword} writes to the graph; pass --write"));
                }
                if let Err(e) = writable() {
                    return e;
                }
            }
            let options = QueryOptions { max_rows: request.max_rows };
            match crate::db::with_graph(|g| g.run(request.language, &request.text, &request.params, options)) {
                Ok(result) => {
                    if let Some(keyword) = &writing {
                        changed(format!("khonsu: {keyword}"));
                    }
                    serde_json::to_value(result).unwrap_or(Value::Null)
                }
                Err(e) => query_error(e),
            }
        }
        Message::Schema(request) => {
            if let Err(e) = is_open_graph(&request.graph) {
                return e;
            }
            match crate::db::with_graph(|g| g.schema()) {
                Ok(schema) => serde_json::to_value(schema).unwrap_or(Value::Null),
                Err(e) => query_error(e),
            }
        }
        Message::Explain(request) => {
            if let Err(e) = is_open_graph(&request.graph) {
                return e;
            }
            match crate::db::with_graph(|g| g.explain(request.language, &request.text)) {
                Ok(plan) => json!({ "plan": plan }),
                Err(e) => query_error(e),
            }
        }
        Message::Sync(request) => {
            if let Err(e) = is_open_graph(&request.graph) {
                return e;
            }
            if let Err(e) = writable() {
                return e;
            }
            let Some(source) = settings::load_sources().into_iter().find(|s| s.id == request.source) else {
                return error("not_found", format!("no source {:?}", request.source));
            };
            let mode = if request.incremental { SyncMode::Incremental } else { SyncMode::Full };
            let cancel = AtomicBool::new(false);
            let outcome = crate::db::with_graph(|g| {
                Ok(plugin::sync_source(g, &source, mode, &cancel, |_| {}))
            });
            match outcome {
                Ok(Ok(report)) => {
                    changed(format!("khonsu: {} ({} nodes, {} relationships)", source.name, report.merge.nodes_written, report.merge.edges_written));
                    serde_json::to_value(report).unwrap_or(Value::Null)
                }
                Ok(Err(e)) => error("plugin", e.to_string()),
                Err(e) => query_error(e),
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

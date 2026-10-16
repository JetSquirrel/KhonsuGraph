//! The line between `khonsu` commands and the running window, after
//! DuckLocal's `remote`: an agent tells the window what it is doing to a
//! dashboard, asks it to show a spec or a query, and reads what it shows.
//!
//! The window listens on 127.0.0.1 at a port it picks, and writes the port,
//! its pid and a random token to `window.json` in the config directory. A
//! command connects, sends one JSON line `{token, message}` and, for `open`
//! and `state`, reads one JSON line back. Nothing waits on a window that is
//! not there: a missing file or a refused connection is "no window".

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use khonsu_core::Language;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::presence::Update;

const FILE: &str = "window.json";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);

/// What a command asks of the window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    Presence(Update),
    Open(OpenRequest),
    State(StateRequest),
    /// Run a query on the window's open graph: HawDB leases a graph to one
    /// process, so while the window has it, a command asks the window.
    Query(QueryRequest),
    /// `khonsu check` on the window's open graph, for the same reason.
    Check(CheckRequest),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// The graph the command meant; the window refuses if it has another.
    pub graph: String,
    pub language: Language,
    pub text: String,
    pub max_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckRequest {
    pub graph: String,
    /// The spec, absolute.
    pub path: String,
    #[serde(default)]
    pub summary: bool,
}

/// `khonsu open`: a graph or `.dash` files to open, and a query to show in a
/// tab of its own, run there when `run` is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    /// Absolute paths: graph directories and `.dash` files.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub language: Option<Language>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub run: bool,
}

/// `khonsu open --state`: what the window shows, all of it, only the tabs
/// showing one spec (`path`), or only the active tab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StateRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Envelope {
    token: String,
    message: Message,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WindowFile {
    port: u16,
    pid: u32,
    token: String,
}

fn window_file() -> PathBuf {
    khonsu_core::settings::config_dir().join(FILE)
}

fn connect() -> Option<(TcpStream, String)> {
    let text = std::fs::read_to_string(window_file()).ok()?;
    let window: WindowFile = serde_json::from_str(&text).ok()?;
    let addr = SocketAddr::from(([127, 0, 0, 1], window.port));
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    Some((stream, window.token))
}

fn send(stream: &mut TcpStream, token: String, message: &Message) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&Envelope { token, message: message.clone() })?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// Fire and forget; true when a window took it.
pub fn notify(message: &Message) -> bool {
    let Some((mut stream, token)) = connect() else {
        return false;
    };
    send(&mut stream, token, message).is_ok()
}

/// Send and wait for the window's answer. `None` when no window is running.
pub fn request(message: &Message) -> Option<Result<Json, String>> {
    let (mut stream, token) = connect()?;
    if let Err(e) = send(&mut stream, token, message) {
        return Some(Err(e.to_string()));
    }
    let mut line = String::new();
    Some(match BufReader::new(stream).read_line(&mut line) {
        Ok(_) => serde_json::from_str(&line).map_err(|e| format!("the window answered {line:?}: {e}")),
        Err(e) => Err(e.to_string()),
    })
}

/// Whether a window is listening.
pub fn window_running() -> bool {
    connect().is_some()
}

/// The window's side: listen, record where, and hand each message to
/// `handle`, whose answer goes back to the command. Runs on its own thread
/// until the process ends; `window.json` is removed when the returned guard
/// drops.
pub fn serve(handle: impl Fn(Message) -> Json + Send + Sync + 'static) -> std::io::Result<ServeGuard> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let port = listener.local_addr()?.port();
    let token = token();
    let path = window_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_vec(&WindowFile {
            port,
            pid: std::process::id(),
            token: token.clone(),
        })?,
    )?;
    let handle = std::sync::Arc::new(handle);
    std::thread::Builder::new().name("khonsu-remote".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let handle = handle.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                });
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let answer = match serde_json::from_str::<Envelope>(&line) {
                    Ok(envelope) if envelope.token == token => handle(envelope.message),
                    Ok(_) => json!({"error": {"kind": "token", "message": "wrong token"}}),
                    Err(e) => json!({"error": {"kind": "protocol", "message": e.to_string()}}),
                };
                let mut stream = stream;
                let _ = writeln!(stream, "{answer}");
            });
        }
    })?;
    Ok(ServeGuard { path })
}

pub struct ServeGuard {
    path: PathBuf,
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Not secret against the local user, only against stray connections.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

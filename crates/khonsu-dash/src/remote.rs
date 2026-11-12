//! The line between `khonsu` commands and the running window, after
//! DuckLocal's `remote`: an agent tells the window what it is doing to a
//! dashboard, asks it to show a spec or a query, and reads what it shows.
//!
//! Each window listens on 127.0.0.1 at a port it picks, and registers itself
//! in `windows/` in the config directory: a file of its own holding the port,
//! its pid and a random token, readable by the user alone, and beside it a
//! lock file the window holds an OS lock on while it lives. So:
//!
//! - a second window neither takes over the first one's registration nor
//!   removes it when it closes;
//! - a registration left by a crash is known for what it is, because the
//!   system dropped its lock with the process. It is removed, and nothing is
//!   sent to its port, which may belong to another program by then.
//!
//! A command connects and sends a nonce. The window answers with a hash of
//! its token and that nonce, which only the process that wrote the
//! registration can make; only then does the command send one JSON line
//! `{token, message}` and, when it wants an answer, read one JSON line back.
//! The window reads both lines with a size cap and a timeout before it knows
//! who is there. Nothing waits on a window that is not there: no registration
//! or a refused connection is "no window".

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use khonsu_core::Language;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::presence::Update;

const DIR: &str = "windows";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// How long either side waits for the other's handshake line.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the window waits for the request once the handshake is done.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest handshake line, and the longest request: a query's text, or a
/// list of paths.
const MAX_HELLO_BYTES: usize = 1024;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

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
    /// `khonsu plugin sync` into the window's open graph, for the same
    /// reason. The window reruns its open dashboards afterwards.
    Sync(SyncRequest),
}

impl Message {
    /// How long a command waits for the answer. A sync takes as long as its
    /// source does, so it waits until the window answers or goes away.
    fn answer_wait(&self) -> Option<Duration> {
        match self {
            Message::Sync(_) => None,
            _ => Some(Duration::from_secs(10)),
        }
    }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    /// The graph the source writes to; the window refuses if it has another.
    pub graph: String,
    /// The source's ID.
    pub source: String,
    #[serde(default)]
    pub incremental: bool,
    /// Remove nothing the source no longer has.
    #[serde(default)]
    pub append: bool,
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

/// The first line a command sends: something only it knows, for the window
/// to prove itself with.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hello {
    hello: String,
}

/// The window's answer to [`Hello`]: a hash of its token and the command's
/// nonce, which only the process that wrote the registration can make.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Proof {
    proof: String,
}

/// One window's registration: `windows/<pid>-<id>.json` in the config
/// directory, beside a `.lock` file that the window holds an OS lock on for
/// as long as it lives.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WindowFile {
    port: u16,
    pid: u32,
    token: String,
    /// Unix milliseconds: the newest window is asked first.
    #[serde(default)]
    started: u128,
}

fn registry() -> PathBuf {
    khonsu_core::settings::config_dir().join(DIR)
}

fn lock_beside(registration: &Path) -> PathBuf {
    registration.with_extension("lock")
}

fn proof(token: &str, nonce: &str) -> String {
    khonsu_core::util::sha256_hex(format!("{token}:{nonce}").as_bytes())
}

/// Whether a process holds the lock at `path`: the window that made it is
/// still running. The system drops the lock when the process ends, however
/// it ends, so this can't be left behind by a crash the way a pid can.
fn is_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// The windows registered in `dir` whose process is alive, newest first. A
/// registration whose window has gone is removed on the way.
fn live_windows(dir: &Path) -> Vec<WindowFile> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let lock = lock_beside(&path);
        if !is_held(&lock) {
            // Left by a window that crashed. Nothing is sent to its port,
            // which may be another program's by now.
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&lock);
            continue;
        }
        if let Some(window) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<WindowFile>(&bytes).ok()) {
            found.push(window);
        }
    }
    found.sort_by_key(|window| std::cmp::Reverse(window.started));
    found
}

fn read_capped_line(reader: &mut impl BufRead, max: usize) -> std::io::Result<String> {
    let mut line = Vec::new();
    let read = reader.take(max as u64 + 1).read_until(b'\n', &mut line)?;
    if read > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("a line longer than {max} bytes")));
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// A connection to `window`, once it has proved it holds the registration's
/// token. Until then it has been sent a nonce and nothing else: not the
/// token, and no query text.
fn open(window: &WindowFile) -> Option<BufReader<TcpStream>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], window.port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    let nonce = random_hex();
    let mut hello = serde_json::to_string(&Hello { hello: nonce.clone() }).ok()?;
    hello.push('\n');
    stream.write_all(hello.as_bytes()).ok()?;
    let mut reader = BufReader::new(stream);
    let line = read_capped_line(&mut reader, MAX_HELLO_BYTES).ok()?;
    let answer: Proof = serde_json::from_str(&line).ok()?;
    (answer.proof == proof(&window.token, &nonce)).then_some(reader)
}

fn send(stream: &mut TcpStream, token: &str, message: &Message) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&Envelope { token: token.to_string(), message: message.clone() })?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// Fire and forget, to every window; true when one took it.
pub fn notify(message: &Message) -> bool {
    notify_in(&registry(), message)
}

fn notify_in(dir: &Path, message: &Message) -> bool {
    let mut taken = false;
    for window in live_windows(dir) {
        if let Some(mut reader) = open(&window) {
            taken |= send(reader.get_mut(), &window.token, message).is_ok();
        }
    }
    taken
}

/// Send and wait for a window's answer. `None` when no window is running.
/// With several windows the newest is asked first; one that answers that it
/// has no graph open, or another graph, passes the question on to the next.
pub fn request(message: &Message) -> Option<Result<Json, String>> {
    request_in(&registry(), message)
}

fn request_in(dir: &Path, message: &Message) -> Option<Result<Json, String>> {
    let mut refusal = None;
    for window in live_windows(dir) {
        let Some(mut reader) = open(&window) else {
            continue;
        };
        let answer = (|| {
            reader.get_mut().set_read_timeout(message.answer_wait()).map_err(|e| e.to_string())?;
            send(reader.get_mut(), &window.token, message).map_err(|e| e.to_string())?;
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            serde_json::from_str::<Json>(&line).map_err(|e| format!("the window answered {line:?}: {e}"))
        })();
        let elsewhere = answer.as_ref().is_ok_and(|value| matches!(value["error"]["kind"].as_str(), Some("no_graph" | "other_graph")));
        if !elsewhere {
            return Some(answer);
        }
        refusal.get_or_insert(answer);
    }
    refusal
}

/// Whether a window is listening.
pub fn window_running() -> bool {
    live_windows(&registry()).iter().any(|window| open(window).is_some())
}

/// The window's side: listen, record where, and hand each message to
/// `handle`, whose answer goes back to the command. Runs on its own thread
/// until the process ends; the registration is removed when the returned
/// guard drops, and is this window's alone: closing another window leaves it.
pub fn serve(handle: impl Fn(Message) -> Json + Send + Sync + 'static) -> std::io::Result<ServeGuard> {
    serve_in(&registry(), handle)
}

fn serve_in(dir: &Path, handle: impl Fn(Message) -> Json + Send + Sync + 'static) -> std::io::Result<ServeGuard> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let port = listener.local_addr()?.port();
    let token = random_hex();
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}-{}.json", std::process::id(), &random_hex()[..8]));
    // Locked before the registration exists, so that it is never seen
    // without its owner.
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(lock_beside(&path))?;
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => std::io::Error::other("the window's registration is locked by another process"),
        TryLockError::Error(error) => error,
    })?;
    let registration = serde_json::to_vec(&WindowFile {
        port,
        pid: std::process::id(),
        token: token.clone(),
        started: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default(),
    })?;
    write_private(&path, &registration)?;
    let handle = std::sync::Arc::new(handle);
    std::thread::Builder::new().name("khonsu-remote".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let handle = handle.clone();
            let token = token.clone();
            std::thread::spawn(move || answer(stream, &token, handle.as_ref()));
        }
    })?;
    Ok(ServeGuard { path, _lock: lock })
}

/// One connection: prove who we are, then read one message and answer it.
/// Both lines are bounded and timed, so a stray or hostile connection costs a
/// thread a few seconds and a few megabytes at most.
fn answer(stream: TcpStream, token: &str, handle: &(dyn Fn(Message) -> Json + Send + Sync)) {
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Ok(line) = read_capped_line(&mut reader, MAX_HELLO_BYTES) else {
        return;
    };
    let Ok(hello) = serde_json::from_str::<Hello>(&line) else {
        let _ = writeln!(out, "{}", json!({"error": {"kind": "protocol", "message": "expected a hello"}}));
        return;
    };
    let Ok(proof) = serde_json::to_string(&Proof { proof: proof(token, &hello.hello) }) else {
        return;
    };
    if writeln!(out, "{proof}").is_err() {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(Some(REQUEST_TIMEOUT));
    let answer = match read_capped_line(&mut reader, MAX_REQUEST_BYTES) {
        // A command that only wanted to know a window is here.
        Ok(line) if line.is_empty() => return,
        Ok(line) => match serde_json::from_str::<Envelope>(&line) {
            Ok(envelope) if envelope.token == token => {
                // The answer may take as long as the work does.
                let _ = out.set_write_timeout(None);
                handle(envelope.message)
            }
            Ok(_) => json!({"error": {"kind": "token", "message": "wrong token"}}),
            Err(e) => json!({"error": {"kind": "protocol", "message": e.to_string()}}),
        },
        Err(e) => json!({"error": {"kind": "protocol", "message": e.to_string()}}),
    };
    let _ = writeln!(out, "{answer}");
}

/// Writes a file only this user can read: it holds the token.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub struct ServeGuard {
    path: PathBuf,
    /// Held for the life of the window: what says it is alive.
    _lock: File,
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        // Its own registration, by a name no other window has.
        let _ = std::fs::remove_file(&self.path);
        let _ = self._lock.unlock();
        let _ = std::fs::remove_file(lock_beside(&self.path));
    }
}

/// 128 bits from the system's randomness, by way of two hashers' keys: no
/// secret against the local user, who can read the registration, but not to
/// be guessed by anything else that can reach the port.
fn random_hex() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default());
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    format!("{:016x}{:016x}", part(), part())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn state() -> Message {
        Message::State(StateRequest::default())
    }

    fn registrations(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().flatten().filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json")).count()
    }

    #[test]
    fn a_command_reaches_the_window_and_gets_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        assert!(request_in(dir.path(), &state()).is_none(), "no window");
        let guard = serve_in(dir.path(), |message| json!({"got": matches!(message, Message::State(_))})).unwrap();
        assert_eq!(request_in(dir.path(), &state()), Some(Ok(json!({"got": true}))));
        assert!(notify_in(dir.path(), &state()));
        drop(guard);
        assert!(request_in(dir.path(), &state()).is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing is left behind");
    }

    /// A second window used to take the one registration file, and take it
    /// away when it closed, leaving the first window unreachable.
    #[test]
    fn closing_one_window_leaves_the_other_registered() {
        let dir = tempfile::tempdir().unwrap();
        let first = serve_in(dir.path(), |_| json!({"window": "first"})).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let second = serve_in(dir.path(), |_| json!({"window": "second"})).unwrap();
        assert_eq!(registrations(dir.path()), 2);
        assert_eq!(request_in(dir.path(), &state()), Some(Ok(json!({"window": "second"}))), "the newest answers");
        drop(second);
        assert_eq!(registrations(dir.path()), 1);
        assert_eq!(request_in(dir.path(), &state()), Some(Ok(json!({"window": "first"}))));
        drop(first);
    }

    #[test]
    fn a_window_without_the_graph_passes_the_question_to_one_that_has_it() {
        let dir = tempfile::tempdir().unwrap();
        let _has = serve_in(dir.path(), |_| json!({"rows": [[1]]})).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let _other = serve_in(dir.path(), |_| json!({"error": {"kind": "other_graph", "message": "not that one"}})).unwrap();
        assert_eq!(request_in(dir.path(), &state()), Some(Ok(json!({"rows": [[1]]}))));
        // When none has it, the refusal is the answer.
        let alone = tempfile::tempdir().unwrap();
        let _none = serve_in(alone.path(), |_| json!({"error": {"kind": "no_graph", "message": "none open"}})).unwrap();
        let answer = request_in(alone.path(), &state()).unwrap().unwrap();
        assert_eq!(answer["error"]["kind"], "no_graph");
    }

    /// A listener that records everything sent to it and never proves itself:
    /// whatever has the port a crashed window once had.
    fn impostor() -> (u16, Arc<Mutex<Vec<u8>>>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (sink, count) = (heard.clone(), connections.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                count.fetch_add(1, Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                // It answers like a window would, with a proof it can't make.
                let _ = writeln!(stream, "{}", json!({"proof": "0".repeat(64)}));
                let mut bytes = Vec::new();
                let _ = stream.read_to_end(&mut bytes);
                sink.lock().unwrap().extend(bytes);
            }
        });
        (port, heard, connections)
    }

    fn register(dir: &Path, name: &str, port: u16) -> PathBuf {
        let path = dir.join(format!("{name}.json"));
        let window = WindowFile { port, pid: 1, token: "the-secret-token".into(), started: u128::MAX };
        std::fs::write(&path, serde_json::to_vec(&window).unwrap()).unwrap();
        std::fs::write(lock_beside(&path), b"").unwrap();
        path
    }

    /// A registration left by a crash named a port, and whatever bound that
    /// port later was sent the token and the query.
    #[test]
    fn a_registration_left_by_a_crash_is_removed_and_its_port_is_sent_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (port, heard, connections) = impostor();
        let stale = register(dir.path(), "999-stale", port);
        let query = Message::Query(QueryRequest { graph: "g".into(), language: Language::Cypher, text: "MATCH (secret) RETURN secret".into(), max_rows: 1 });
        assert!(request_in(dir.path(), &query).is_none(), "no live window");
        assert!(!notify_in(dir.path(), &query));
        assert!(!stale.exists() && !lock_beside(&stale).exists(), "the stale registration is gone");
        assert_eq!(connections.load(Ordering::SeqCst), 0, "nothing connected to its port");
        assert!(heard.lock().unwrap().is_empty());
    }

    /// Even a registration that reads as live gets only a nonce until the
    /// other end proves it holds the token.
    #[test]
    fn a_listener_that_cannot_prove_itself_is_sent_neither_token_nor_query() {
        let dir = tempfile::tempdir().unwrap();
        let (port, heard, connections) = impostor();
        let path = register(dir.path(), "999-live", port);
        // Held, as a running window's would be.
        let lock = OpenOptions::new().write(true).open(lock_beside(&path)).unwrap();
        lock.try_lock().unwrap();
        let query = Message::Query(QueryRequest { graph: "g".into(), language: Language::Cypher, text: "MATCH (secret) RETURN secret".into(), max_rows: 1 });
        assert!(request_in(dir.path(), &query).is_none());
        assert_eq!(connections.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(500));
        let heard = String::from_utf8_lossy(&heard.lock().unwrap()).into_owned();
        assert!(heard.contains("hello"), "{heard}");
        assert!(!heard.contains("the-secret-token") && !heard.contains("secret) RETURN"), "{heard}");
    }

    #[test]
    fn the_window_refuses_a_wrong_token_an_oversized_line_and_a_silent_connection() {
        let dir = tempfile::tempdir().unwrap();
        let handled = Arc::new(AtomicUsize::new(0));
        let count = handled.clone();
        let _guard = serve_in(dir.path(), move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            json!({})
        })
        .unwrap();
        let window = live_windows(dir.path()).remove(0);

        // The right handshake, then the wrong token.
        let mut reader = open(&window).expect("the window proves itself");
        send(reader.get_mut(), "not-the-token", &state()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("wrong token"), "{line}");

        // A first line with no end is cut off, not read into memory.
        let mut stream = TcpStream::connect(("127.0.0.1", window.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        let mut sent = 0;
        while sent < MAX_HELLO_BYTES * 4 && stream.write_all(&chunk).is_ok() {
            sent += chunk.len();
        }
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        assert!(!String::from_utf8_lossy(&rest).contains("proof"), "an oversized hello got an answer");

        // A request line past the cap is refused after the handshake.
        let mut reader = open(&window).unwrap();
        let big = Message::Query(QueryRequest { graph: "g".into(), language: Language::Cypher, text: "x".repeat(MAX_REQUEST_BYTES + 1), max_rows: 1 });
        let _ = send(reader.get_mut(), &window.token, &big);
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        assert!(!line.contains("{}") || line.contains("protocol"), "{line}");

        // A connection that says nothing is dropped after the handshake wait.
        let mut silent = TcpStream::connect(("127.0.0.1", window.port)).unwrap();
        silent.set_read_timeout(Some(HANDSHAKE_TIMEOUT + Duration::from_secs(5))).unwrap();
        let started = std::time::Instant::now();
        let mut rest = Vec::new();
        let _ = silent.read_to_end(&mut rest);
        assert!(started.elapsed() < HANDSHAKE_TIMEOUT + Duration::from_secs(4), "it was held open");
        assert_eq!(handled.load(Ordering::SeqCst), 0, "none of these reached the window");

        // And the window still answers a real command.
        assert_eq!(request_in(dir.path(), &state()), Some(Ok(json!({}))));
    }

    #[cfg(unix)]
    #[test]
    fn the_registration_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _guard = serve_in(dir.path(), |_| json!({})).unwrap();
        let file = std::fs::read_dir(dir.path()).unwrap().flatten().find(|entry| entry.path().extension().is_some_and(|ext| ext == "json")).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}

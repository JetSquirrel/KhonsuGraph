//! A plugin for tests: sends the JSON lines in its `lines` setting, then exits
//! with its `exit` setting, so a test can make a sync fail in any way it likes.
//! Not built into the app or shipped.
//!
//! With `KHONSU_SCRIPTED_TRACE` set to a file, each run appends the operation
//! it was started for and whether it was sent secrets, so a test can tell
//! that it was never started at all.

use std::io::Write;
use std::process::ExitCode;

use khonsu_plugin_protocol::{read_request, ConfigField, Describe, Emitter, FieldKind, Message, Permissions};

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_default();
    let trace = |line: &str| {
        if let Some(path) = std::env::var_os("KHONSU_SCRIPTED_TRACE") {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{line}");
            }
        }
    };
    trace(&command);
    if command == "describe" {
        let describe = Describe {
            name: "Scripted".into(),
            version: "0.1.0".into(),
            description: "Sends what a test tells it to".into(),
            config: ["lines", "exit"]
                .map(|key| ConfigField {
                    key: key.into(),
                    label: key.into(),
                    kind: FieldKind::String,
                    required: false,
                    secret: false,
                    help: String::new(),
                    default: None,
                })
                .into_iter()
                .chain([ConfigField {
                    key: "token".into(),
                    label: "token".into(),
                    kind: FieldKind::String,
                    required: false,
                    secret: true,
                    help: String::new(),
                    default: None,
                }])
                .collect(),
            permissions: Permissions::default(),
            labels: Vec::new(),
            relationship_types: Vec::new(),
            supports_incremental: true,
            analyses: Vec::new(),
        };
        Emitter::stdout().emit(&Message::Describe(describe)).unwrap();
        return ExitCode::SUCCESS;
    }
    let request = read_request().unwrap();
    if !request.secrets.is_empty() {
        trace("secrets");
    }
    if command == "test" {
        Emitter::stdout().emit(&Message::TestResult { ok: true, message: String::new() }).unwrap();
        return ExitCode::SUCCESS;
    }
    let mut out = std::io::stdout().lock();
    for line in request.config.get("lines").and_then(|lines| lines.as_array()).into_iter().flatten() {
        writeln!(out, "{line}").unwrap();
    }
    out.flush().unwrap();
    let code = request.config.get("exit").and_then(|exit| exit.as_u64()).unwrap_or(0);
    ExitCode::from(code as u8)
}

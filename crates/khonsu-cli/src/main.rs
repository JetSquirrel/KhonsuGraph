//! `khonsu`: windowless access to KhonsuGraph graphs for agents and scripts.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {...}}` and exit non-zero. Graphs open read-only unless a
//! command needs to write and `--write` is given.

mod dash;

use clap::{Args, Parser, Subcommand};
use khonsu_core::plugin::{self, SyncEvent};
use khonsu_core::protocol::{FieldKind, SyncMode};
use khonsu_core::settings::{self, Source};
use khonsu_core::{normalize_path, secrets, AccessMode, Graph, Language, OpenError, QueryError, QueryOptions};
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

const PROGRAM: &str = "khonsu CLI";

#[derive(Parser)]
#[command(name = "khonsu", version, about = "KhonsuGraph command line: query HawDB graphs and sync data sources, with JSON output")]
struct Cli {
    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct DbArg {
    /// Graph directory. Defaults to $KHONSU_DB, then the graph last opened in the app.
    #[arg(long, short = 'd')]
    db: Option<PathBuf>,
}

#[derive(Args)]
struct QueryText {
    /// Cypher query text.
    #[arg(long, conflicts_with_all = ["sql", "file"])]
    cypher: Option<String>,
    /// PostgreSQL-dialect SQL text.
    #[arg(long, conflicts_with = "file")]
    sql: Option<String>,
    /// Read the query from a file (`-` for stdin); `.sql` files are SQL.
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Cypher or SQL query and print rows as JSON.
    Query {
        #[command(flatten)]
        db: DbArg,
        #[command(flatten)]
        text: QueryText,
        /// Parameter `name=value`; value is parsed as JSON when it can be. SQL uses `1=..`, `2=..` for $1, $2.
        #[arg(long = "param", short = 'p')]
        params: Vec<String>,
        /// Rows to print; the rest are dropped and `truncated` is true.
        #[arg(long, default_value_t = 200)]
        max_rows: usize,
        /// Open the graph for writing (needed for CREATE, MERGE, INSERT, ...).
        #[arg(long)]
        write: bool,
    },
    /// Show the plan HawDB chooses for a query.
    Explain {
        #[command(flatten)]
        db: DbArg,
        #[command(flatten)]
        text: QueryText,
    },
    /// Labels, relationship types, properties and tables. Read this before querying.
    Schema {
        #[command(flatten)]
        db: DbArg,
    },
    /// Create a new, empty graph directory.
    Init { path: PathBuf },
    /// Data-source plugins.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Configured data sources.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Validate a .dash dashboard and run every query read-only.
    Check(dash::CheckArgs),
    /// Review threads on a .dash dashboard's plots: list, reply, resolve.
    Comments(dash::CommentsArgs),
    /// Tell the running window where you are about to edit a dashboard.
    Presence(dash::PresenceArgs),
    /// Show a graph, a .dash file or a query in the window, or read what it shows (--state).
    Open(dash::OpenArgs),
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Built-in and installed plugins.
    List,
    /// A plugin's configuration fields, permissions and output types.
    Describe { id: String },
    /// Install a plugin from a local directory containing khonsu-plugin.json.
    Install {
        dir: PathBuf,
        /// Approve the permissions the plugin declares (required to sync with it).
        #[arg(long)]
        yes: bool,
    },
    Uninstall { id: String },
    /// Sync a configured source into its graph (or preview with --dry-run).
    Sync {
        /// Source ID (see `khonsu source list`).
        #[arg(long)]
        source: String,
        /// Pull only changes since the last sync, when the plugin supports it.
        #[arg(long)]
        incremental: bool,
        /// Count and sample records without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum SourceCommand {
    List,
    /// Add a source. Fields the plugin marks secret go to the system keychain.
    Add {
        #[command(flatten)]
        db: DbArg,
        #[arg(long)]
        plugin: String,
        #[arg(long)]
        name: String,
        /// Configuration `key=value`; value is parsed as JSON when it can be.
        #[arg(long = "config", short = 'c')]
        config: Vec<String>,
        /// Test the connection before saving.
        #[arg(long)]
        test: bool,
    },
    Remove { id: String },
}

#[derive(Debug)]
struct CliError {
    kind: String,
    message: String,
    detail: Value,
    /// Exit status: 1 for failures, 2 for a spec or argument mistake.
    code: u8,
}

impl CliError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            detail: Value::Null,
            code: 1,
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            ..Self::new("usage", message)
        }
    }
}

impl From<OpenError> for CliError {
    fn from(err: OpenError) -> Self {
        let kind = match &err {
            OpenError::Locked(_) => "locked",
            OpenError::NotAGraph(_) | OpenError::Missing(_) => "not_a_graph",
            OpenError::NotEmpty(_) => "not_empty",
            _ => "open",
        };
        let detail = match &err {
            OpenError::Locked(Some(owner)) => json!({ "owner": owner }),
            OpenError::Engine { hint: Some(hint), .. } => json!({ "hint": hint }),
            _ => Value::Null,
        };
        Self {
            kind: kind.into(),
            message: err.to_string(),
            detail,
            code: 1,
        }
    }
}

impl From<QueryError> for CliError {
    fn from(err: QueryError) -> Self {
        Self {
            kind: err.kind.clone(),
            message: err.message.clone(),
            detail: json!({ "line": err.line, "column": err.column }),
            code: 1,
        }
    }
}

impl From<plugin::PluginError> for CliError {
    fn from(err: plugin::PluginError) -> Self {
        let detail = match &err {
            plugin::PluginError::NotApproved { permissions, .. } => json!({ "permissions": permissions }),
            _ => Value::Null,
        };
        Self {
            kind: "plugin".into(),
            message: err.to_string(),
            detail,
            code: 1,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        Self::new("io", err.to_string())
    }
}

fn resolve_db(arg: &DbArg) -> Result<PathBuf, CliError> {
    arg.db
        .clone()
        .or_else(|| std::env::var_os("KHONSU_DB").map(PathBuf::from))
        .or_else(|| settings::Settings::load().last_opened)
        .ok_or_else(|| CliError::new("usage", "no graph given: pass --db <dir> or set KHONSU_DB"))
}

fn read_text(text: &QueryText) -> Result<(Language, String), CliError> {
    match (&text.cypher, &text.sql, &text.file) {
        (Some(q), _, _) => Ok((Language::Cypher, q.clone())),
        (_, Some(q), _) => Ok((Language::Sql, q.clone())),
        (_, _, Some(path)) => {
            let mut body = String::new();
            if path.as_os_str() == "-" {
                std::io::stdin().read_to_string(&mut body)?;
            } else {
                body = std::fs::read_to_string(path)?;
            }
            let is_sql = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql"));
            Ok((if is_sql { Language::Sql } else { Language::Cypher }, body))
        }
        _ => Err(CliError::new("usage", "give a query with --cypher, --sql or --file")),
    }
}

fn key_values(pairs: &[String]) -> Result<Map<String, Value>, CliError> {
    let mut map = Map::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| CliError::new("usage", format!("expected key=value, got {pair:?}")))?;
        let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        map.insert(key.trim().trim_start_matches('$').to_string(), value);
    }
    Ok(map)
}

fn run(command: Command) -> Result<Value, CliError> {
    match command {
        Command::Query {
            db,
            text,
            params,
            max_rows,
            write,
        } => {
            let path = resolve_db(&db)?;
            let (language, text) = read_text(&text)?;
            let mode = if write { AccessMode::ReadWrite } else { AccessMode::ReadOnly };
            let mut graph = match Graph::open(&path, mode, PROGRAM) {
                Ok(graph) => graph,
                // The window holds the graph: HawDB leases it to one process,
                // so a read goes through the window.
                Err(OpenError::Locked(owner)) if !write && params.is_empty() => {
                    return dash::query_via_window(&path, language, &text, max_rows)
                        .unwrap_or_else(|| Err(OpenError::Locked(owner).into()));
                }
                Err(e) => return Err(e.into()),
            };
            let result = graph.run(language, &text, &key_values(&params)?, QueryOptions { max_rows })?;
            Ok(serde_json::to_value(result).expect("result serializes"))
        }
        Command::Explain { db, text } => {
            let path = resolve_db(&db)?;
            let (language, text) = read_text(&text)?;
            let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
            Ok(json!({ "plan": graph.explain(language, &text)? }))
        }
        Command::Schema { db } => {
            let path = resolve_db(&db)?;
            let mut graph = Graph::open(&path, AccessMode::ReadOnly, PROGRAM)?;
            Ok(serde_json::to_value(graph.schema()?).expect("schema serializes"))
        }
        Command::Init { path } => {
            let graph = Graph::create(&path, PROGRAM)?;
            Ok(json!({ "created": graph.path() }))
        }
        Command::Plugin { command } => run_plugin(command),
        Command::Source { command } => run_source(command),
        Command::Check(args) => dash::check(args),
        Command::Comments(args) => dash::comments(args),
        Command::Presence(args) => dash::presence(args),
        Command::Open(args) => dash::open(args),
    }
}

fn run_plugin(command: PluginCommand) -> Result<Value, CliError> {
    match command {
        PluginCommand::List => {
            let plugins: Vec<Value> = plugin::list()
                .into_iter()
                .map(|p| {
                    let approved = p.builtin || settings::is_approved(p.id(), &p.manifest.version, &p.manifest.permissions);
                    json!({
                        "id": p.manifest.id,
                        "name": p.manifest.name,
                        "version": p.manifest.version,
                        "builtin": p.builtin,
                        "approved": approved,
                        "description": p.manifest.description,
                        "permissions": p.manifest.permissions,
                        "dir": p.dir,
                    })
                })
                .collect();
            Ok(json!({ "plugins": plugins }))
        }
        PluginCommand::Describe { id } => {
            let info = plugin::find(&id)?;
            Ok(serde_json::to_value(plugin::describe(&info)?).expect("describe serializes"))
        }
        PluginCommand::Install { dir, yes } => {
            let info = plugin::install(&dir)?;
            if yes {
                settings::approve(info.id(), &info.manifest.version, &info.manifest.permissions)?;
            }
            Ok(json!({
                "installed": info.manifest.id,
                "version": info.manifest.version,
                "dir": info.dir,
                "permissions": info.manifest.permissions,
                "approved": yes,
                "note": if yes { "" } else { "re-run with --yes to approve these permissions before syncing" },
            }))
        }
        PluginCommand::Uninstall { id } => {
            plugin::uninstall(&id)?;
            Ok(json!({ "uninstalled": id }))
        }
        PluginCommand::Sync {
            source,
            incremental,
            dry_run,
        } => {
            let source = settings::load_sources()
                .into_iter()
                .find(|s| s.id == source)
                .ok_or_else(|| CliError::new("not_found", format!("no source {source:?}; see `khonsu source list`")))?;
            let mode = if incremental { SyncMode::Incremental } else { SyncMode::Full };
            let cancel = AtomicBool::new(false);
            let progress = |event: SyncEvent| {
                // Progress goes to stderr so stdout stays one JSON document.
                if let Ok(line) = serde_json::to_string(&event) {
                    eprintln!("{line}");
                }
            };
            if dry_run {
                let info = plugin::find(&source.plugin)?;
                let request = plugin::request_for(&source, mode)?;
                let preview = plugin::preview(&info, &request, &cancel, progress)?;
                return Ok(json!({ "dry_run": true, "preview": preview }));
            }
            let mut graph = Graph::open(&source.graph, AccessMode::ReadWrite, PROGRAM)?;
            let report = plugin::sync_source(&mut graph, &source, mode, &cancel, progress)?;
            Ok(serde_json::to_value(report).expect("report serializes"))
        }
    }
}

fn run_source(command: SourceCommand) -> Result<Value, CliError> {
    match command {
        SourceCommand::List => Ok(json!({ "sources": settings::load_sources() })),
        SourceCommand::Add {
            db,
            plugin: plugin_id,
            name,
            config,
            test,
        } => {
            let graph = resolve_db(&db)?;
            let info = plugin::find(&plugin_id)?;
            let describe = plugin::describe(&info)?;
            let values = key_values(&config)?;
            let id = settings::new_source_id(&name);
            let mut plain = Map::new();
            let mut secret_keys = Vec::new();
            for (key, value) in values {
                let field = describe.config.iter().find(|f| f.key == key);
                match field {
                    None => return Err(CliError::new("usage", format!("{plugin_id} has no config field {key:?}"))),
                    Some(f) if f.secret => {
                        let text = value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
                        secrets::set(&id, &key, &text).map_err(|e| CliError::new("keychain", e))?;
                        secret_keys.push(key);
                    }
                    Some(f) if f.kind == FieldKind::File => {
                        // Store absolute paths so syncs work from any directory.
                        let text = value.as_str().unwrap_or_default();
                        let abs = std::path::absolute(text).unwrap_or_else(|_| PathBuf::from(text));
                        plain.insert(key, Value::String(abs.display().to_string()));
                    }
                    Some(_) => {
                        plain.insert(key, value);
                    }
                }
            }
            for field in describe.config.iter().filter(|f| f.required) {
                if !plain.contains_key(&field.key) && !secret_keys.contains(&field.key) {
                    return Err(CliError::new("usage", format!("missing required config {:?}", field.key)));
                }
            }
            let source = Source {
                id: id.clone(),
                name,
                plugin: plugin_id,
                graph: normalize_path(&graph),
                config: plain,
                secret_keys,
                cursor: None,
                patterns: Vec::new(),
                last_sync: None,
            };
            let mut tested = Value::Null;
            if test {
                let (ok, message) = plugin::test(&info, &plugin::request_for(&source, SyncMode::Full)?)?;
                if !ok {
                    for key in &source.secret_keys {
                        let _ = secrets::delete(&id, key);
                    }
                    return Err(CliError::new("test_failed", message));
                }
                tested = json!({ "ok": ok, "message": message });
            }
            settings::upsert_source(source.clone())?;
            Ok(json!({ "added": source, "test": tested }))
        }
        SourceCommand::Remove { id } => {
            let sources = settings::load_sources();
            let source = sources
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| CliError::new("not_found", format!("no source {id:?}")))?;
            for key in &source.secret_keys {
                let _ = secrets::delete(&id, key);
            }
            settings::remove_source(&id)?;
            Ok(json!({ "removed": id }))
        }
    }
}

fn print(value: &Value, pretty: bool) {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    println!("{}", text.expect("JSON serializes"));
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(value) => {
            print(&value, cli.pretty);
            ExitCode::SUCCESS
        }
        Err(err) => {
            let mut error = json!({ "kind": err.kind, "message": err.message });
            if !err.detail.is_null() {
                error["detail"] = err.detail;
            }
            print(&json!({ "error": error }), cli.pretty);
            ExitCode::from(err.code.max(1))
        }
    }
}

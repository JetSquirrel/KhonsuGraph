//! Built-in CSV data source.
//!
//! Each node table row becomes one node and each edge table row one
//! relationship, so imported counts equal file row counts (minus rows that
//! repeat a stable ID). Columns other than the mapped ones become properties,
//! or only those a table lists in `columns`. Cells such as `NA` or `NULL`
//! (`null_values`) are missing values: the property is left out.

use khonsu_plugin_protocol::{
    read_request, ConfigField, Describe, EdgeRecord, Emitter, FieldKind, Message, NodeRecord, Permissions, Request,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Missing-value markers when `null_values` is not set; R, pandas and SQL
/// exports write these.
const DEFAULT_NULL_VALUES: [&str; 5] = ["NA", "N/A", "NULL", "null", "NaN"];

/// One column, or several whose values are joined with `:` into a composite
/// key, e.g. `["game_id", "play_id"]` -> `2023090700:56`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Columns {
    One(String),
    Many(Vec<String>),
}

impl Columns {
    fn names(&self) -> &[String] {
        match self {
            Columns::One(name) => std::slice::from_ref(name),
            Columns::Many(names) => names,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NodeTable {
    path: String,
    /// Fixed label for every row, unless `label_column` is set.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    label_column: Option<String>,
    /// Stable ID column(s); defaults to `id`, else the first column.
    #[serde(default)]
    id_column: Option<Columns>,
    /// Columns kept as properties; all but the label column when unset.
    #[serde(default)]
    columns: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct EdgeTable {
    path: String,
    #[serde(default)]
    rel_type: Option<String>,
    #[serde(default)]
    type_column: Option<String>,
    source_column: Columns,
    target_column: Columns,
    /// Stable ID column(s); defaults to `<file name>:<row number>`.
    #[serde(default)]
    id_column: Option<Columns>,
    /// Columns kept as properties; all but the endpoint and type columns
    /// when unset.
    #[serde(default)]
    columns: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct Config {
    base_dir: Option<PathBuf>,
    delimiter: u8,
    infer_types: bool,
    null_values: Vec<String>,
    nodes: Vec<NodeTable>,
    edges: Vec<EdgeTable>,
}

fn describe() -> Describe {
    let field = |key: &str, label: &str, kind: FieldKind, help: &str| ConfigField {
        key: key.into(),
        label: label.into(),
        kind,
        required: false,
        secret: false,
        help: help.into(),
        default: None,
    };
    Describe {
        name: "CSV".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "Node and edge tables from local CSV files. One row is one node or one relationship.".into(),
        config: vec![
            field("node_file", "Node file", FieldKind::File, "CSV with one node per row"),
            field("node_label", "Node label", FieldKind::String, "Label for every node row, e.g. Host"),
            field("node_id_column", "Node ID column", FieldKind::String, "Stable ID column (default: id, else the first column)"),
            field("edge_file", "Edge file", FieldKind::File, "CSV with one relationship per row"),
            field("edge_type", "Relationship type", FieldKind::String, "Type for every edge row, e.g. DEPENDS_ON"),
            field("edge_source_column", "Source column", FieldKind::String, "Column holding the source node ID (default: source)"),
            field("edge_target_column", "Target column", FieldKind::String, "Column holding the target node ID (default: target)"),
            field(
                "nodes",
                "Node tables (advanced)",
                FieldKind::Json,
                r#"[{"path", "label" | "label_column", "id_column", "columns"}]; used instead of the single-file fields. An ID or endpoint column can be a list, joined with ':' (["game_id","play_id"])"#,
            ),
            field(
                "edges",
                "Edge tables (advanced)",
                FieldKind::Json,
                r#"[{"path", "rel_type" | "type_column", "source_column", "target_column", "id_column", "columns"}]"#,
            ),
            ConfigField {
                default: Some(Value::String(",".into())),
                ..field("delimiter", "Delimiter", FieldKind::String, "One character; \\t for tab")
            },
            ConfigField {
                default: Some(Value::Bool(true)),
                ..field("infer_types", "Infer types", FieldKind::Bool, "Store numbers and true/false as typed values")
            },
            ConfigField {
                default: Some(Value::from(DEFAULT_NULL_VALUES.to_vec())),
                ..field(
                    "null_values",
                    "Missing values",
                    FieldKind::Json,
                    "Cells that mean no value; the property is left out. [] keeps them as text",
                )
            },
        ],
        permissions: Permissions {
            read_files: true,
            ..Permissions::default()
        },
        labels: Vec::new(),
        relationship_types: Vec::new(),
        supports_incremental: false,
    }
}

fn text(config: &Map<String, Value>, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_config(request: &Request) -> Result<Config, String> {
    let c = &request.config;
    let delimiter = match text(c, "delimiter").as_deref() {
        None => b',',
        Some("\\t") | Some("tab") => b'\t',
        Some(d) if d.len() == 1 => d.as_bytes()[0],
        Some(d) => return Err(format!("delimiter must be one character, got {d:?}")),
    };
    let mut nodes: Vec<NodeTable> = match c.get("nodes") {
        Some(Value::Null) | None => Vec::new(),
        Some(Value::String(s)) if s.trim().is_empty() => Vec::new(),
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("nodes: {e}"))?,
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("nodes: {e}"))?,
    };
    let mut edges: Vec<EdgeTable> = match c.get("edges") {
        Some(Value::Null) | None => Vec::new(),
        Some(Value::String(s)) if s.trim().is_empty() => Vec::new(),
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("edges: {e}"))?,
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("edges: {e}"))?,
    };
    if let Some(path) = text(c, "node_file") {
        nodes.push(NodeTable {
            label: Some(text(c, "node_label").unwrap_or_else(|| label_from_path(&path))),
            path,
            label_column: None,
            id_column: text(c, "node_id_column").map(Columns::One),
            columns: None,
        });
    }
    if let Some(path) = text(c, "edge_file") {
        edges.push(EdgeTable {
            rel_type: Some(text(c, "edge_type").unwrap_or_else(|| "RELATED_TO".into())),
            path,
            type_column: None,
            source_column: Columns::One(text(c, "edge_source_column").unwrap_or_else(|| "source".into())),
            target_column: Columns::One(text(c, "edge_target_column").unwrap_or_else(|| "target".into())),
            id_column: None,
            columns: None,
        });
    }
    if nodes.is_empty() && edges.is_empty() {
        return Err("choose at least one node or edge file".into());
    }
    for table in &nodes {
        if table.label.is_none() && table.label_column.is_none() {
            return Err(format!("{}: set a label or a label column", table.path));
        }
    }
    for table in &edges {
        if table.rel_type.is_none() && table.type_column.is_none() {
            return Err(format!("{}: set a relationship type or a type column", table.path));
        }
    }
    Ok(Config {
        base_dir: text(c, "base_dir").map(PathBuf::from),
        delimiter,
        infer_types: c.get("infer_types").and_then(Value::as_bool).unwrap_or(true),
        null_values: null_values(c.get("null_values"))?,
        nodes,
        edges,
    })
}

/// A JSON list, or text: JSON or comma-separated (`NA, -`).
fn null_values(value: Option<&Value>) -> Result<Vec<String>, String> {
    let list = |v: &Value| -> Result<Vec<String>, String> {
        serde_json::from_value(v.clone()).map_err(|_| "null_values: expected a list of strings".to_string())
    };
    match value {
        None | Some(Value::Null) => Ok(DEFAULT_NULL_VALUES.map(String::from).to_vec()),
        Some(Value::String(s)) if s.trim_start().starts_with('[') => {
            list(&serde_json::from_str(s).map_err(|e| format!("null_values: {e}"))?)
        }
        Some(Value::String(s)) => Ok(s.split(',').map(str::trim).filter(|v| !v.is_empty()).map(String::from).collect()),
        Some(v) => list(v),
    }
}

/// `hosts.csv` -> `Hosts`.
fn label_from_path(path: &str) -> String {
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("Node");
    let mut chars = stem.chars().filter(|c| c.is_alphanumeric() || *c == '_');
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Node".into(),
    }
}

impl Config {
    fn resolve(&self, path: &str) -> PathBuf {
        let path = PathBuf::from(path);
        match &self.base_dir {
            Some(base) if path.is_relative() => base.join(path),
            _ => path,
        }
    }

    fn reader(&self, path: &str) -> Result<(csv::Reader<std::fs::File>, Vec<String>), String> {
        let resolved = self.resolve(path);
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(self.delimiter)
            .flexible(false)
            .from_path(&resolved)
            .map_err(|e| format!("{}: {e}", resolved.display()))?;
        let headers = reader
            .headers()
            .map_err(|e| format!("{}: {e}", resolved.display()))?
            .iter()
            .map(|h| h.trim().trim_start_matches('\u{feff}').to_string())
            .collect();
        Ok((reader, headers))
    }

    fn properties(&self, row: &csv::StringRecord, headers: &[String], cols: &[usize]) -> Map<String, Value> {
        cols.iter()
            .filter_map(|&col| Some((headers[col].clone(), self.value(row.get(col)?)?)))
            .collect()
    }

    fn value(&self, raw: &str) -> Option<Value> {
        if raw.is_empty() || self.null_values.iter().any(|null| null == raw.trim()) {
            return None;
        }
        if !self.infer_types {
            return Some(Value::String(raw.to_string()));
        }
        let trimmed = raw.trim();
        if let Ok(i) = trimmed.parse::<i64>() {
            // Keep leading zeros (zip codes, IDs) as text.
            if !(trimmed.len() > 1 && trimmed.starts_with('0')) {
                return Some(Value::from(i));
            }
        }
        if trimmed.contains(['.', 'e', 'E']) {
            if let Some(number) = trimmed.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                return Some(Value::Number(number));
            }
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => Some(Value::String(raw.to_string())),
        }
    }
}

fn column(headers: &[String], name: &str, path: &str) -> Result<usize, String> {
    headers
        .iter()
        .position(|h| h == name)
        .ok_or_else(|| format!("{path}: no column named {name:?} (columns: {})", headers.join(", ")))
}

fn key_columns(headers: &[String], columns: &Columns, path: &str) -> Result<Vec<usize>, String> {
    if columns.names().is_empty() {
        return Err(format!("{path}: an empty column list"));
    }
    columns.names().iter().map(|name| column(headers, name, path)).collect()
}

/// The key's parts joined with `:`; empty when one of them is.
fn key(row: &csv::StringRecord, cols: &[usize]) -> String {
    let parts: Vec<&str> = cols.iter().map(|&col| row.get(col).unwrap_or("").trim()).collect();
    if parts.iter().any(|part| part.is_empty()) {
        return String::new();
    }
    parts.join(":")
}

/// Which columns become properties: those listed, else all but `mapped`.
fn property_columns(headers: &[String], listed: Option<&[String]>, mapped: &[usize], path: &str) -> Result<Vec<usize>, String> {
    match listed {
        Some(names) => names.iter().map(|name| column(headers, name, path)).collect(),
        None => Ok((0..headers.len()).filter(|col| !mapped.contains(col)).collect()),
    }
}

fn count_rows(config: &Config, path: &str) -> Result<u64, String> {
    let (mut reader, _) = config.reader(path)?;
    let mut n = 0;
    for row in reader.records() {
        row.map_err(|e| format!("{path}: {e}"))?;
        n += 1;
    }
    Ok(n)
}

fn sync<W: Write>(config: &Config, out: &mut Emitter<W>) -> Result<(u64, u64), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut total = 0;
    for table in &config.nodes {
        total += count_rows(config, &table.path)?;
    }
    for table in &config.edges {
        total += count_rows(config, &table.path)?;
    }
    let (mut nodes, mut edges, mut done) = (0u64, 0u64, 0u64);

    for table in &config.nodes {
        let (mut reader, headers) = config.reader(&table.path)?;
        let id_cols = match &table.id_column {
            Some(columns) => key_columns(&headers, columns, &table.path)?,
            None => vec![headers.iter().position(|h| h == "id").unwrap_or(0)],
        };
        let label_col = table
            .label_column
            .as_deref()
            .map(|name| column(&headers, name, &table.path))
            .transpose()?;
        let property_cols = property_columns(&headers, table.columns.as_deref(), label_col.as_slice(), &table.path)?;
        for (i, row) in reader.records().enumerate() {
            let row = row.map_err(|e| format!("{}: {e}", table.path))?;
            let line = i + 2;
            let id = key(&row, &id_cols);
            if id.is_empty() {
                let names: Vec<&str> = id_cols.iter().map(|&col| headers[col].as_str()).collect();
                return Err(format!("{} line {line}: empty ID in {}", table.path, names.join(", ")));
            }
            let label = match label_col {
                Some(col) => row.get(col).unwrap_or("").trim().to_string(),
                None => table.label.clone().unwrap_or_default(),
            };
            if label.is_empty() {
                return Err(format!("{} line {line}: empty label", table.path));
            }
            let properties = config.properties(&row, &headers, &property_cols);
            out.node(NodeRecord {
                id,
                label,
                properties,
            })
            .map_err(io)?;
            nodes += 1;
            done += 1;
            if done % 1000 == 0 {
                out.progress(format!("reading {}", table.path), Some(done), Some(total)).map_err(io)?;
            }
        }
    }

    for table in &config.edges {
        let (mut reader, headers) = config.reader(&table.path)?;
        let source_cols = key_columns(&headers, &table.source_column, &table.path)?;
        let target_cols = key_columns(&headers, &table.target_column, &table.path)?;
        let id_cols = table
            .id_column
            .as_ref()
            .map(|columns| key_columns(&headers, columns, &table.path))
            .transpose()?;
        let type_col = table
            .type_column
            .as_deref()
            .map(|name| column(&headers, name, &table.path))
            .transpose()?;
        let mapped: Vec<usize> = source_cols.iter().chain(&target_cols).chain(&type_col).copied().collect();
        let property_cols = property_columns(&headers, table.columns.as_deref(), &mapped, &table.path)?;
        let file_name = Path::new(&table.path).file_name().and_then(|s| s.to_str()).unwrap_or(&table.path).to_string();
        for (i, row) in reader.records().enumerate() {
            let row = row.map_err(|e| format!("{}: {e}", table.path))?;
            let line = i + 2;
            let get = |col: usize| row.get(col).unwrap_or("").trim().to_string();
            let (source, target) = (key(&row, &source_cols), key(&row, &target_cols));
            if source.is_empty() || target.is_empty() {
                return Err(format!("{} line {line}: empty source or target", table.path));
            }
            let id = match id_cols.as_deref().map(|cols| key(&row, cols)) {
                Some(id) if !id.is_empty() => id,
                _ => format!("{file_name}:{line}"),
            };
            let rel_type = match type_col {
                Some(col) => get(col),
                None => table.rel_type.clone().unwrap_or_default(),
            };
            if rel_type.is_empty() {
                return Err(format!("{} line {line}: empty relationship type", table.path));
            }
            let properties = config.properties(&row, &headers, &property_cols);
            out.edge(EdgeRecord {
                id,
                source,
                target,
                rel_type,
                properties,
            })
            .map_err(io)?;
            edges += 1;
            done += 1;
            if done % 1000 == 0 {
                out.progress(format!("reading {}", table.path), Some(done), Some(total)).map_err(io)?;
            }
        }
    }
    out.progress("done", Some(done), Some(total)).map_err(io)?;
    Ok((nodes, edges))
}

fn test(config: &Config) -> Result<String, String> {
    let mut parts = Vec::new();
    for table in &config.nodes {
        parts.push(format!("{}: {} rows", table.path, count_rows(config, &table.path)?));
    }
    for table in &config.edges {
        let (_, headers) = config.reader(&table.path)?;
        key_columns(&headers, &table.source_column, &table.path)?;
        key_columns(&headers, &table.target_column, &table.path)?;
        parts.push(format!("{}: {} rows", table.path, count_rows(config, &table.path)?));
    }
    Ok(parts.join("; "))
}

fn main() -> ExitCode {
    let mut out = Emitter::stdout();
    let command = std::env::args().nth(1).unwrap_or_default();
    let result = match command.as_str() {
        "describe" => out.emit(&Message::Describe(describe())).map_err(|e| e.to_string()),
        "test" => read_request()
            .map_err(|e| e.to_string())
            .and_then(|r| parse_config(&r))
            .and_then(|c| test(&c))
            .and_then(|message| out.emit(&Message::TestResult { ok: true, message }).map_err(|e| e.to_string()))
            .or_else(|message| out.emit(&Message::TestResult { ok: false, message }).map_err(|e| e.to_string())),
        "sync" => read_request()
            .map_err(|e| e.to_string())
            .and_then(|r| parse_config(&r))
            .and_then(|c| sync(&c, &mut out))
            .and_then(|(nodes, edges)| out.emit(&Message::Done { nodes, edges }).map_err(|e| e.to_string())),
        other => Err(format!("unknown command {other:?}; expected describe, test or sync")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = out.emit(&Message::Error { message: message.clone() });
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use khonsu_plugin_protocol::parse_line;

    fn run(dir: &Path, config: serde_json::Value) -> Result<Vec<Message>, String> {
        let request = Request {
            protocol: 1,
            config: {
                let mut map = config.as_object().unwrap().clone();
                map.insert("base_dir".into(), Value::String(dir.display().to_string()));
                map
            },
            ..Request::default()
        };
        let config = parse_config(&request)?;
        let mut out = Emitter::new(Vec::new());
        sync(&config, &mut out)?;
        let bytes = out.into_inner();
        Ok(String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter_map(|l| parse_line(l).map(|m| m.unwrap()))
            .collect())
    }

    #[test]
    fn one_record_per_row_with_typed_properties() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hosts.csv"), "id,name,cpu,zip,up\nh1,web,4,02139,true\nh2,db,,10001,false\n").unwrap();
        std::fs::write(dir.path().join("deps.csv"), "source,target,weight\nh1,h2,0.5\nh1,h2,1.5\n").unwrap();
        let messages = run(
            dir.path(),
            serde_json::json!({"node_file": "hosts.csv", "edge_file": "deps.csv", "edge_type": "DEPENDS_ON"}),
        )
        .unwrap();
        let nodes: Vec<&NodeRecord> = messages.iter().filter_map(|m| if let Message::Node(n) = m { Some(n) } else { None }).collect();
        let edges: Vec<&EdgeRecord> = messages.iter().filter_map(|m| if let Message::Edge(e) = m { Some(e) } else { None }).collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 2);
        assert_eq!(nodes[0].label, "Hosts");
        assert_eq!(nodes[0].properties["cpu"], 4);
        assert_eq!(nodes[0].properties["zip"], "02139");
        assert_eq!(nodes[0].properties["up"], true);
        assert!(!nodes[1].properties.contains_key("cpu"));
        assert_eq!(edges[0].id, "deps.csv:2");
        assert_eq!(edges[1].properties["weight"], 1.5);
        assert_ne!(edges[0].id, edges[1].id);
    }

    #[test]
    fn missing_value_markers_leave_the_property_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.csv"), "id,yards,team\np1,NA,NULL\np2,12,NA\n").unwrap();
        let nodes = |config| -> Vec<NodeRecord> {
            run(dir.path(), config)
                .unwrap()
                .into_iter()
                .filter_map(|m| if let Message::Node(n) = m { Some(n) } else { None })
                .collect()
        };
        let default = nodes(serde_json::json!({"node_file": "p.csv"}));
        assert!(!default[0].properties.contains_key("yards"));
        assert!(!default[0].properties.contains_key("team"));
        assert_eq!(default[1].properties["yards"], 12);
        let kept = nodes(serde_json::json!({"node_file": "p.csv", "null_values": []}));
        assert_eq!(kept[0].properties["yards"], "NA");
        let custom = nodes(serde_json::json!({"node_file": "p.csv", "null_values": "NULL"}));
        assert_eq!(custom[0].properties["yards"], "NA");
        assert!(!custom[0].properties.contains_key("team"));
    }

    #[test]
    fn composite_keys_and_chosen_columns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pp.csv"),
            "game_id,play_id,nfl_id,quarter,tackle\n2023,56,p1,1,1\n2023,56,p2,1,0\n2023,80,p1,2,0\n",
        )
        .unwrap();
        let messages = run(
            dir.path(),
            serde_json::json!({
                "nodes": [{"path": "pp.csv", "label": "Play", "id_column": ["game_id", "play_id"], "columns": ["quarter"]}],
                "edges": [{
                    "path": "pp.csv", "rel_type": "IN_PLAY", "source_column": "nfl_id",
                    "target_column": ["game_id", "play_id"], "id_column": ["nfl_id", "game_id", "play_id"],
                    "columns": ["tackle"]
                }]
            }),
        )
        .unwrap();
        let nodes: Vec<&NodeRecord> = messages.iter().filter_map(|m| if let Message::Node(n) = m { Some(n) } else { None }).collect();
        let edges: Vec<&EdgeRecord> = messages.iter().filter_map(|m| if let Message::Edge(e) = m { Some(e) } else { None }).collect();
        assert_eq!(nodes[0].id, "2023:56");
        assert_eq!(nodes[2].id, "2023:80");
        assert_eq!(nodes[0].properties.keys().collect::<Vec<_>>(), ["quarter"]);
        assert_eq!(edges[1].id, "p2:2023:56");
        assert_eq!(edges[1].target, "2023:56");
        assert_eq!(edges[1].properties.keys().collect::<Vec<_>>(), ["tackle"]);
    }

    #[test]
    fn mapping_errors_name_the_file_and_column() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("e.csv"), "from,to\na,b\n").unwrap();
        let err = run(dir.path(), serde_json::json!({"edge_file": "e.csv"})).unwrap_err();
        assert!(err.contains("e.csv") && err.contains("source"), "{err}");
    }
}

//! Official Neo4j data source: copies a database's nodes and relationships into a
//! KhonsuGraph graph, over Neo4j's HTTP API and in read access mode.
//!
//! Cartography fills a Neo4j database from AWS, GCP, Kubernetes, Okta and more, so
//! this is how its graph gets here. Nothing below knows Cartography's labels: its
//! bookkeeping properties are only the default for what to leave out.
//!
//! HawDB has one label per node, and Neo4j has any number. Each node takes one as
//! its label (see `Naming::primary`), and all of them go in `neo4j_labels`.
//!
//! A stable ID is `<label>:<id property>`, since Neo4j only keeps an ID unique
//! within a label, if at all. A node with no such property falls back on its
//! `elementId`, which lasts until the database is rebuilt.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::net::IpAddr;
use std::process::ExitCode;
use std::time::Duration;

use base64::Engine as _;
use khonsu_plugin_protocol::{fnv1a, read_request, ConfigField, Describe, EdgeRecord, Emitter, FieldKind, Message, NodeRecord, Permissions, Request};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Rewritten on every Cartography sync, so keeping them would make every record
/// look changed each time.
const VOLATILE: [&str; 2] = ["lastupdated", "_module_version"];
const LABELS_PROPERTY: &str = "neo4j_labels";
const PROGRESS_EVERY: u64 = 5_000;

const NODES: &str = "MATCH (n) RETURN elementId(n), labels(n), properties(n), n[$key]";
const EDGES: &str = "MATCH (a)-[r]->(b) RETURN elementId(a), labels(a), a[$key], type(r), elementId(b), labels(b), b[$key], properties(r), elementId(r)";

fn describe() -> Describe {
    let field = |key: &str, label: &str, kind: FieldKind, help: &str, default: Option<Value>| ConfigField {
        key: key.into(),
        label: label.into(),
        kind,
        required: false,
        secret: false,
        help: help.into(),
        default,
    };
    Describe {
        name: "Neo4j".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "Copies the nodes and relationships of a Neo4j database, such as one Cartography fills. Sends only read queries.".into(),
        config: vec![
            ConfigField {
                required: true,
                ..field("url", "HTTP address", FieldKind::String, "Neo4j's HTTP address, not bolt://", Some(json!("http://127.0.0.1:7474")))
            },
            field("database", "Database", FieldKind::String, "", Some(json!("neo4j"))),
            field("username", "User", FieldKind::String, "A reader role is enough", Some(json!("neo4j"))),
            ConfigField {
                secret: true,
                ..field("password", "Password", FieldKind::String, "Stored in the system keychain. Leave empty if the server has no authentication", None)
            },
            field("id_property", "ID property", FieldKind::String, "The property that identifies a node from one sync to the next", Some(json!("id"))),
            field(
                "exclude_properties",
                "Properties to leave out",
                FieldKind::Json,
                "Properties rewritten on every run of whatever fills the database",
                Some(json!(VOLATILE)),
            ),
            field(
                "label_priority",
                "Label priority",
                FieldKind::Json,
                "Labels to prefer when a node has several, first one first. Otherwise a node takes its rarest label",
                Some(json!([])),
            ),
        ],
        permissions: Permissions {
            network: vec!["The Neo4j server you configure".into()],
            read_files: false,
            commands: Vec::new(),
            notes: vec!["Sends only read queries, in read access mode".into()],
        },
        // Whatever the database holds.
        labels: Vec::new(),
        relationship_types: Vec::new(),
        supports_incremental: false,
        analyses: Vec::new(),
    }
}

struct Settings {
    endpoint: String,
    authorization: Option<String>,
    loopback: bool,
    secure: bool,
    id_property: String,
    exclude: HashSet<String>,
    priority: Vec<String>,
}

fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A list given as JSON or, from a form or a command line, as comma-separated text.
fn list(map: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    match map.get(key)? {
        Value::Array(items) => Some(items.iter().filter_map(Value::as_str).map(str::to_string).collect()),
        Value::String(items) => Some(items.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()),
        _ => None,
    }
}

impl Settings {
    fn from_request(request: &Request) -> Result<Self, String> {
        let config = &request.config;
        let url = text(config, "url").unwrap_or_else(|| "http://127.0.0.1:7474".into());
        let url = url.trim_end_matches('/');
        let Some((scheme, rest)) = url.split_once("://") else {
            return Err(format!("{url} is not an HTTP address, like http://127.0.0.1:7474"));
        };
        if !matches!(scheme, "http" | "https") {
            return Err(format!("{scheme}:// is not supported: use Neo4j's HTTP address, like http://127.0.0.1:7474"));
        }
        let host = rest.split('/').next().unwrap_or_default();
        let host = host.rsplit('@').next().unwrap_or_default();
        let name = host.strip_prefix('[').and_then(|h| h.split(']').next()).unwrap_or_else(|| host.split(':').next().unwrap_or_default());
        let loopback = name == "localhost" || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());

        let database = text(config, "database").unwrap_or_else(|| "neo4j".into());
        if !database.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
            return Err(format!("{database} is not a Neo4j database name"));
        }
        let authorization = text(&request.secrets, "password").map(|password| {
            let username = text(config, "username").unwrap_or_else(|| "neo4j".into());
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")))
        });
        Ok(Self {
            endpoint: format!("{url}/db/{database}/tx/commit"),
            authorization,
            loopback,
            secure: scheme == "https",
            id_property: text(config, "id_property").unwrap_or_else(|| "id".into()),
            exclude: list(config, "exclude_properties").unwrap_or_else(|| VOLATILE.map(String::from).to_vec()).into_iter().collect(),
            priority: list(config, "label_priority").unwrap_or_default(),
        })
    }

    /// Runs statements in one read transaction, and hands over each row as it
    /// arrives, with the index of its statement.
    fn query(&self, statements: &[&str], on_row: &mut dyn FnMut(usize, Vec<Value>) -> Result<(), String>) -> Result<(), String> {
        let statements: Vec<Value> = statements
            .iter()
            .map(|statement| json!({"statement": statement, "parameters": {"key": self.id_property}, "resultDataContents": ["row"]}))
            .collect();
        let builder = ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(10)));
        // A proxy set for the outside world can't reach a server on this machine.
        let builder = if self.loopback { builder.proxy(None) } else { builder };
        let agent = builder.build().new_agent();
        let mut post = agent
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            // The server refuses a write made in this mode, whatever the statement.
            .header("access-mode", "READ");
        if let Some(authorization) = &self.authorization {
            post = post.header("Authorization", authorization);
        }
        let response = post.send(json!({"statements": statements}).to_string()).map_err(|err| format!("can't reach Neo4j: {err}"))?;
        let status = response.status().as_u16();
        match status {
            200..=299 => read_answer(BufReader::new(response.into_body().into_reader()), on_row),
            401 | 403 => Err(format!("Neo4j refused the user name or password (HTTP {status})")),
            404 => Err(format!("no Neo4j database at {} (HTTP 404)", self.endpoint)),
            _ => Err(format!("Neo4j answered HTTP {status}")),
        }
    }
}

#[derive(Deserialize)]
struct Row {
    row: Vec<Value>,
}

#[derive(Deserialize)]
struct QueryError {
    #[serde(default)]
    message: String,
}

type OnRow<'a> = &'a mut dyn FnMut(usize, Vec<Value>) -> Result<(), String>;

/// Walks `{"results": [{"data": [{"row": [...]}]}], "errors": [...]}` as it
/// arrives. A database doesn't fit in memory as one JSON value, and Neo4j reports
/// an error that comes up midway at the end, after the rows already sent.
fn read_answer(reader: impl Read, mut on_row: OnRow) -> Result<(), String> {
    let mut stream = serde_json::Deserializer::from_reader(reader);
    let errors = Answer { on_row: &mut on_row }.deserialize(&mut stream).map_err(|err| {
        if err.is_eof() {
            "the answer from Neo4j ended early".to_string()
        } else if err.is_data() || err.is_syntax() {
            format!("couldn't read the answer from Neo4j: {err}")
        } else {
            err.to_string()
        }
    })?;
    match errors.into_iter().next() {
        Some(error) => Err(format!("Neo4j: {}", error.message)),
        None => Ok(()),
    }
}

struct Answer<'a, 'b> {
    on_row: &'a mut OnRow<'b>,
}

impl<'de> DeserializeSeed<'de> for Answer<'_, '_> {
    type Value = Vec<QueryError>;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Answer<'_, '_> {
    type Value = Vec<QueryError>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a Neo4j answer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut errors = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "results" => map.next_value_seed(Results { on_row: &mut *self.on_row })?,
                "errors" => errors = map.next_value()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(errors)
    }
}

struct Results<'a, 'b> {
    on_row: &'a mut OnRow<'b>,
}

impl<'de> DeserializeSeed<'de> for Results<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Results<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of results")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut statement = 0;
        while seq.next_element_seed(OneResult { statement, on_row: &mut *self.on_row })?.is_some() {
            statement += 1;
        }
        Ok(())
    }
}

struct OneResult<'a, 'b> {
    statement: usize,
    on_row: &'a mut OnRow<'b>,
}

impl<'de> DeserializeSeed<'de> for OneResult<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for OneResult<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a result")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "data" {
                map.next_value_seed(Rows { statement: self.statement, on_row: &mut *self.on_row })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct Rows<'a, 'b> {
    statement: usize,
    on_row: &'a mut OnRow<'b>,
}

impl<'de> DeserializeSeed<'de> for Rows<'_, '_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Rows<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(row) = seq.next_element::<Row>()? {
            (self.on_row)(self.statement, row.row).map_err(de::Error::custom)?;
        }
        Ok(())
    }
}

/// A label or relationship type as a plain name: HawDB's schema and the words
/// KhonsuGraph builds queries from take no spaces or punctuation.
fn plain_name(name: &str) -> String {
    let mut plain: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if plain.is_empty() {
        return "Unlabeled".into();
    }
    if plain.starts_with(|c: char| c.is_ascii_digit()) {
        plain.insert(0, '_');
    }
    plain
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

struct Naming<'a> {
    settings: &'a Settings,
    /// How many nodes hold each label.
    counts: HashMap<String, u64>,
    /// IDs already taken, hashed to keep this small on a large database.
    taken: HashSet<u64>,
    /// Nodes that lost out on their ID, by `elementId`.
    renamed: HashMap<String, String>,
}

impl Naming<'_> {
    /// The label a node goes by. A label on the priority list wins. Otherwise the
    /// rarest: a shared label such as `AWSPrincipal` covers at least the nodes of a
    /// narrower one such as `AWSRole`. On a tie, the one Neo4j lists first, which is
    /// usually the older: `min_by_key` keeps the first of equals.
    fn primary(&self, labels: &[String]) -> String {
        let chosen = self
            .settings
            .priority
            .iter()
            .find(|label| labels.contains(label))
            .or_else(|| labels.iter().min_by_key(|label| self.counts.get(*label).copied().unwrap_or(0)));
        chosen.map_or_else(|| "Unlabeled".to_string(), |label| plain_name(label))
    }

    fn stable_id(&self, element: &str, label: &str, key: &Value) -> String {
        match key {
            Value::Null => format!("element:{element}"),
            Value::String(key) => format!("{label}:{key}"),
            other => format!("{label}:{other}"),
        }
    }

    fn endpoint(&self, element: &Value, labels: &Value, key: &Value) -> String {
        let element = element.as_str().unwrap_or_default();
        if let Some(renamed) = self.renamed.get(element) {
            return renamed.clone();
        }
        self.stable_id(element, &self.primary(&strings(labels)), key)
    }
}

fn kept(properties: Value, exclude: &HashSet<String>) -> Map<String, Value> {
    let Value::Object(properties) = properties else {
        return Map::new();
    };
    properties.into_iter().filter(|(key, value)| !value.is_null() && !exclude.contains(key)).collect()
}

fn sync<W: Write>(settings: &Settings, out: &mut Emitter<W>) -> Result<(), String> {
    if settings.authorization.is_some() && !settings.secure && !settings.loopback {
        out.emit(&Message::Log { level: "warn".into(), message: "The password goes to Neo4j unencrypted: use an https address".into() }).map_err(|e| e.to_string())?;
    }
    let mut counts = HashMap::new();
    let mut total_nodes = 0;
    let mut total_edges = 0;
    settings.query(&["MATCH (n) UNWIND labels(n) AS label RETURN label, count(*)", "MATCH (n) RETURN count(n)", "MATCH ()-[r]->() RETURN count(r)"], &mut |statement, row| {
        let first = row.first().unwrap_or(&Value::Null);
        match statement {
            0 => {
                if let (Some(label), Some(count)) = (first.as_str(), row.get(1).and_then(Value::as_u64)) {
                    counts.insert(label.to_string(), count);
                }
            }
            1 => total_nodes = first.as_u64().unwrap_or(0),
            _ => total_edges = first.as_u64().unwrap_or(0),
        }
        Ok(())
    })?;

    // A full sync removes what it doesn't see. A database that restarted without
    // its volume, or the wrong one, must not empty the graph.
    if total_nodes == 0 {
        out.emit(&Message::Done {
            nodes: 0,
            edges: 0,
            complete: false,
            gaps: vec!["this Neo4j database has no nodes, so nothing was changed".into()],
        })
        .map_err(|e| e.to_string())?;
        return Ok(());
    }

    let mut naming = Naming { settings, counts, taken: HashSet::new(), renamed: HashMap::new() };
    let mut taken_edges = HashSet::new();
    let (mut nodes, mut edges, mut no_id, mut shared_id, mut parallel) = (0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
    let written = |e: std::io::Error| format!("couldn't send records: {e}");

    // One transaction, so the relationships go with these nodes.
    settings.query(&[NODES, EDGES], &mut |statement, row| {
        let mut row = row.into_iter();
        let mut next = || row.next().unwrap_or(Value::Null);
        if statement == 0 {
            let (element, labels, properties, key) = (next(), next(), next(), next());
            let element = element.as_str().unwrap_or_default();
            let labels = strings(&labels);
            let label = naming.primary(&labels);
            let mut id = naming.stable_id(element, &label, &key);
            if key.is_null() {
                no_id += 1;
            } else if !naming.taken.insert(fnv1a(&id)) {
                // Neo4j allows it unless a constraint says otherwise.
                id = format!("{id}#{element}");
                naming.renamed.insert(element.to_string(), id.clone());
                shared_id += 1;
            }
            let mut sorted = labels;
            sorted.sort();
            let mut properties = kept(properties, &settings.exclude);
            properties.insert(LABELS_PROPERTY.into(), json!(sorted));
            out.node(NodeRecord { id, label, properties }).map_err(written)?;
            nodes += 1;
            if nodes % PROGRESS_EVERY == 0 || nodes == total_nodes {
                out.progress("nodes", Some(nodes), Some(total_nodes)).map_err(written)?;
            }
        } else {
            let (a_element, a_labels, a_key, kind, b_element, b_labels, b_key, properties, element) = (next(), next(), next(), next(), next(), next(), next(), next(), next());
            let source = naming.endpoint(&a_element, &a_labels, &a_key);
            let target = naming.endpoint(&b_element, &b_labels, &b_key);
            let kind = plain_name(kind.as_str().unwrap_or_default());
            let mut hash = fnv1a(&format!("{source}\0{kind}\0{target}"));
            if !taken_edges.insert(hash) {
                // Parallel relationships: one type, one pair.
                hash = fnv1a(&format!("{source}\0{kind}\0{target}\0{}", element.as_str().unwrap_or_default()));
                taken_edges.insert(hash);
                parallel += 1;
            }
            out.edge(EdgeRecord { id: format!("{kind}:{hash:016x}"), source, target, rel_type: kind, properties: kept(properties, &settings.exclude) }).map_err(written)?;
            edges += 1;
            if edges % PROGRESS_EVERY == 0 || edges == total_edges {
                out.progress("relationships", Some(edges), Some(total_edges)).map_err(written)?;
            }
        }
        Ok(())
    })?;

    let key = &settings.id_property;
    let mut warn = |message: String| out.emit(&Message::Log { level: "warn".into(), message }).map_err(written);
    if no_id > 0 {
        warn(format!("{no_id} nodes have no {key} property, so they use Neo4j's elementId, which changes if the database is rebuilt"))?;
    }
    if shared_id > 0 {
        warn(format!("{shared_id} nodes share a label and {key} with another, so they use Neo4j's elementId too"))?;
    }
    if parallel > 0 {
        warn(format!("{parallel} relationships repeat a type between the same two nodes, so they use Neo4j's elementId in their ID"))?;
    }
    out.emit(&Message::Done { nodes, edges, complete: true, gaps: Vec::new() }).map_err(written)
}

fn test(settings: &Settings) -> Message {
    let mut nodes = 0;
    match settings.query(&["MATCH (n) RETURN count(n)"], &mut |_, row| {
        nodes = row.first().and_then(Value::as_u64).unwrap_or(0);
        Ok(())
    }) {
        Ok(()) => Message::TestResult { ok: true, message: format!("connected to {}: {nodes} nodes", settings.endpoint) },
        Err(message) => Message::TestResult { ok: false, message },
    }
}

fn run<W: Write>(command: &str, out: &mut Emitter<W>, request: Option<Request>) -> Result<(), String> {
    let settings = |request: Option<Request>| Settings::from_request(&request.ok_or("couldn't read the request")?);
    match command {
        "describe" => out.emit(&Message::Describe(describe())).map_err(|e| e.to_string()),
        "test" => match settings(request) {
            Ok(settings) => out.emit(&test(&settings)).map_err(|e| e.to_string()),
            Err(message) => out.emit(&Message::TestResult { ok: false, message }).map_err(|e| e.to_string()),
        },
        "sync" => sync(&settings(request)?, out),
        other => Err(format!("unknown command {other:?}")),
    }
}

fn main() -> ExitCode {
    let mut out = Emitter::stdout();
    let command = std::env::args().nth(1).unwrap_or_default();
    let request = if command == "describe" { None } else { read_request().ok() };
    match run(&command, &mut out, request) {
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
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Answers each request in turn, and reports what it was asked.
    fn server(answers: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (asked, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut sent = vec![0; length];
                reader.read_exact(&mut sent).unwrap();
                asked.send(format!("{head}\n{}", String::from_utf8(sent).unwrap())).unwrap();
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (url, requests)
    }

    fn request(url: &str, config: Value) -> Request {
        let mut map = config.as_object().unwrap().clone();
        map.insert("url".into(), json!(url));
        let mut secrets = Map::new();
        secrets.insert("password".into(), json!("s3cret"));
        Request { config: map, secrets, ..Request::default() }
    }

    fn messages(output: Vec<u8>) -> Vec<Message> {
        String::from_utf8(output).unwrap().lines().filter_map(|line| parse_line(line).map(Result::unwrap)).collect()
    }

    fn results(sets: &[&[Value]]) -> String {
        let results: Vec<Value> = sets
            .iter()
            .map(|rows| json!({"columns": [], "data": rows.iter().map(|row| json!({"row": row, "meta": []})).collect::<Vec<_>>()}))
            .collect();
        json!({"results": results, "errors": []}).to_string()
    }

    fn sync_against(answers: Vec<(u16, String)>, config: Value) -> (Result<(), String>, Vec<Message>, Vec<String>) {
        let (url, asked) = server(answers);
        let mut out = Emitter::new(Vec::new());
        let result = run("sync", &mut out, Some(request(&url, config)));
        (result, messages(out.into_inner()), asked.try_iter().collect())
    }

    fn counts() -> String {
        results(&[
            &[json!(["AWSRole", 2]), json!(["AWSPrincipal", 3]), json!(["Thing", 1])],
            &[json!([4])],
            &[json!([2])],
        ])
    }

    #[test]
    fn a_database_comes_across_with_its_labels_and_relationships() {
        let graph = results(&[
            &[
                json!(["e:1", ["AWSPrincipal", "AWSRole"], {"id": "arn:r1", "name": "r1", "lastupdated": 7, "_module_version": "1"}, "arn:r1"]),
                json!(["e:2", ["AWSUser", "AWSPrincipal"], {"id": "arn:u1"}, "arn:u1"]),
                json!(["e:3", ["Thing"], {"id": 5, "when": "2024-05-01T10:00:00Z", "nothing": null}, 5]),
                json!(["e:4", [], {"name": "no id"}, null]),
            ],
            &[
                json!(["e:1", ["AWSPrincipal", "AWSRole"], "arn:r1", "TRUSTS", "e:3", ["Thing"], 5, {"lastupdated": 7, "why": "x"}, "r:1"]),
                json!(["e:3", ["Thing"], 5, "SEES", "e:4", [], null, {}, "r:2"]),
            ],
        ]);
        let (result, messages, asked) = sync_against(vec![(200, counts()), (200, graph)], json!({"id_property": "id"}));
        result.unwrap();

        let nodes: Vec<&NodeRecord> = messages.iter().filter_map(|m| if let Message::Node(n) = m { Some(n) } else { None }).collect();
        let edges: Vec<&EdgeRecord> = messages.iter().filter_map(|m| if let Message::Edge(e) = m { Some(e) } else { None }).collect();
        assert_eq!(nodes.len(), 4);
        assert_eq!(edges.len(), 2);

        // The rarest label, every label kept, and bookkeeping left out.
        assert_eq!(nodes[0].label, "AWSRole");
        assert_eq!(nodes[0].id, "AWSRole:arn:r1");
        assert_eq!(nodes[0].properties["neo4j_labels"], json!(["AWSPrincipal", "AWSRole"]));
        assert!(!nodes[0].properties.contains_key("lastupdated"));
        assert!(!nodes[0].properties.contains_key("_module_version"));
        // A number is an ID too, and a null is left out.
        assert_eq!(nodes[2].id, "Thing:5");
        assert!(!nodes[2].properties.contains_key("nothing"));
        // No ID property, and no label at all.
        assert_eq!((nodes[3].id.as_str(), nodes[3].label.as_str()), ("element:e:4", "Unlabeled"));

        // Relationships find their ends under those same IDs.
        assert_eq!((edges[0].source.as_str(), edges[0].target.as_str(), edges[0].rel_type.as_str()), ("AWSRole:arn:r1", "Thing:5", "TRUSTS"));
        assert_eq!(edges[0].properties, json!({"why": "x"}).as_object().unwrap().clone());
        assert_eq!(edges[1].target, "element:e:4");

        assert!(matches!(messages.last(), Some(Message::Done { nodes: 4, edges: 2, complete: true, .. })));
        assert!(messages.iter().any(|m| matches!(m, Message::Log { message, .. } if message.contains("elementId"))));

        // Read mode, the password, and both reads in one transaction.
        let head = asked[1].to_lowercase();
        assert!(head.contains("access-mode: read"), "{head}");
        assert!(head.contains("authorization: basic"), "{head}");
        assert!(head.starts_with("post /db/neo4j/tx/commit"), "{head}");
        assert!(asked[1].contains("MATCH (n) RETURN") && asked[1].contains("MATCH (a)-[r]->(b)"), "{}", asked[1]);
    }

    #[test]
    fn an_id_is_only_unique_within_a_label() {
        let graph = results(&[
            &[
                json!(["e:1", ["Movie"], {"id": 1}, 1]),
                json!(["e:2", ["Person"], {"id": 1}, 1]),
                json!(["e:3", ["Person"], {"id": 1}, 1]),
            ],
            &[json!(["e:3", ["Person"], 1, "ACTED_IN", "e:1", ["Movie"], 1, {}, "r:1"])],
        ]);
        let (result, messages, _) = sync_against(vec![(200, counts()), (200, graph)], json!({}));
        result.unwrap();
        let ids: Vec<&str> = messages.iter().filter_map(|m| if let Message::Node(n) = m { Some(n.id.as_str()) } else { None }).collect();
        assert_eq!(ids, ["Movie:1", "Person:1", "Person:1#e:3"]);
        let edge = messages.iter().find_map(|m| if let Message::Edge(e) = m { Some(e) } else { None }).unwrap();
        assert_eq!(edge.source, "Person:1#e:3", "the relationship stays with the node that was renamed");
    }

    #[test]
    fn parallel_relationships_stay_apart_and_ids_hold_from_one_sync_to_the_next() {
        let graph = || {
            results(&[
                &[json!(["e:1", ["A"], {"id": "a"}, "a"]), json!(["e:2", ["B"], {"id": "b"}, "b"])],
                &[
                    json!(["e:1", ["A"], "a", "R", "e:2", ["B"], "b", {"n": 1}, "r:1"]),
                    json!(["e:1", ["A"], "a", "R", "e:2", ["B"], "b", {"n": 2}, "r:2"]),
                ],
            ])
        };
        let ids = || {
            let (result, messages, _) = sync_against(vec![(200, counts()), (200, graph())], json!({}));
            result.unwrap();
            messages.iter().filter_map(|m| if let Message::Edge(e) = m { Some(e.id.clone()) } else { None }).collect::<Vec<_>>()
        };
        let first = ids();
        assert_eq!(first.len(), 2);
        assert_ne!(first[0], first[1]);
        assert_eq!(first, ids());
    }

    #[test]
    fn a_priority_list_and_odd_names_are_respected() {
        let graph = results(&[
            &[json!(["e:1", ["AWSRole", "Has Space"], {"id": "x"}, "x"]), json!(["e:2", ["Has Space"], {"id": "y"}, "y"])],
            &[json!(["e:1", ["AWSRole", "Has Space"], "x", "A-B", "e:2", ["Has Space"], "y", {}, "r:1"])],
        ]);
        let (result, messages, _) = sync_against(vec![(200, counts()), (200, graph)], json!({"label_priority": "Has Space"}));
        result.unwrap();
        let nodes: Vec<&NodeRecord> = messages.iter().filter_map(|m| if let Message::Node(n) = m { Some(n) } else { None }).collect();
        assert_eq!(nodes[0].label, "Has_Space");
        assert_eq!(nodes[0].properties["neo4j_labels"], json!(["AWSRole", "Has Space"]));
        let edge = messages.iter().find_map(|m| if let Message::Edge(e) = m { Some(e) } else { None }).unwrap();
        assert_eq!(edge.rel_type, "A_B");
        assert_eq!(edge.source, "Has_Space:x");
    }

    /// A database that restarted without its volume would otherwise empty the graph.
    #[test]
    fn an_empty_database_removes_nothing() {
        let empty = results(&[&[], &[json!([0])], &[json!([0])]]);
        let (result, messages, asked) = sync_against(vec![(200, empty)], json!({}));
        result.unwrap();
        assert_eq!(asked.len(), 1, "no second read");
        match messages.last() {
            Some(Message::Done { complete: false, gaps, .. }) => assert!(gaps[0].contains("no nodes"), "{gaps:?}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn failures_fail_the_sync_and_say_why() {
        let fail = |answers: Vec<(u16, String)>| sync_against(answers, json!({})).0.unwrap_err();
        assert!(fail(vec![(401, "{}".into())]).contains("refused the user name or password"));
        assert!(fail(vec![(404, "".into())]).contains("no Neo4j database"));
        assert!(fail(vec![(500, "".into())]).contains("HTTP 500"));
        assert!(fail(vec![(200, "<html>".into())]).contains("couldn't read the answer"));
        // Neo4j reports an error that comes up midway after the rows it already sent.
        let midway = json!({"results": [{"data": [{"row": [1]}]}], "errors": [{"code": "X", "message": "out of memory"}]}).to_string();
        assert_eq!(fail(vec![(200, midway)]), "Neo4j: out of memory");
        // A connection lost midway: `done` is never sent, so nothing is removed.
        let cut = r#"{"results":[{"data":[{"row":[1]},"#.to_string();
        assert_eq!(fail(vec![(200, cut)]), "the answer from Neo4j ended early");
    }

    #[test]
    fn a_failure_midway_never_says_done() {
        let graph = json!({"results": [{"data": [{"row": ["e:1", ["A"], {"id": "a"}, "a"]}]}], "errors": [{"message": "stopped"}]}).to_string();
        let (result, messages, _) = sync_against(vec![(200, counts()), (200, graph)], json!({}));
        assert!(result.is_err());
        assert!(!messages.iter().any(|m| matches!(m, Message::Done { .. })));
    }

    #[test]
    fn test_reports_what_it_finds() {
        let (url, _asked) = server(vec![(200, results(&[&[json!([42])]]))]);
        let mut out = Emitter::new(Vec::new());
        run("test", &mut out, Some(request(&url, json!({})))).unwrap();
        match &messages(out.into_inner())[0] {
            Message::TestResult { ok: true, message } => assert!(message.contains("42 nodes"), "{message}"),
            other => panic!("{other:?}"),
        }

        let (url, _asked) = server(vec![(401, "{}".into())]);
        let mut out = Emitter::new(Vec::new());
        run("test", &mut out, Some(request(&url, json!({})))).unwrap();
        assert!(matches!(&messages(out.into_inner())[0], Message::TestResult { ok: false, .. }));
    }

    #[test]
    fn settings_catch_a_bolt_address_and_a_bad_database() {
        let settings = |config: Value| Settings::from_request(&Request { config: config.as_object().unwrap().clone(), ..Request::default() });
        assert!(settings(json!({"url": "bolt://localhost:7687"})).err().unwrap().contains("HTTP address"));
        assert!(settings(json!({"url": "localhost:7474"})).is_err());
        assert!(settings(json!({"database": "a/../b"})).err().unwrap().contains("database name"));
        let ok = settings(json!({"url": "https://graph.example.com:7473/", "database": "prod", "exclude_properties": ["a"]})).unwrap();
        assert_eq!(ok.endpoint, "https://graph.example.com:7473/db/prod/tx/commit");
        assert!(!ok.loopback && ok.secure && ok.authorization.is_none());
        assert_eq!(ok.exclude, HashSet::from(["a".to_string()]));
        assert!(settings(json!({"url": "http://localhost:7474"})).unwrap().loopback);
        assert!(settings(json!({"url": "http://[::1]:7474"})).unwrap().loopback);
        assert!(settings(json!({"url": "http://127.0.0.1"})).unwrap().loopback);
        assert!(!settings(json!({"url": "http://127.example.com"})).unwrap().loopback);
    }

    #[test]
    fn describe_offers_the_password_as_a_secret() {
        let describe = describe();
        assert!(describe.config.iter().any(|field| field.key == "password" && field.secret));
        assert!(!describe.supports_incremental);
    }
}

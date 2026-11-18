//! Cypher in the ontology's words, and Cypher checked against a graph.
//!
//! **Rewriting.** In `khonsu query --semantic`, a label that names a concept
//! (`(d:Device)`) becomes every label the mappings give that concept or what
//! extends it, in this graph (`(d:Host:Server)`): HawDB reads `(:A:B)` as A
//! or B. A relationship type that names a relationship (`[:runs_on]`) becomes
//! the types mapped to it (`[:RAN_ON|EXEC_ON]`). Anything else is left as
//! written, so labels and types of the graph itself still work. A concept no
//! label in the graph holds is refused, since an empty answer would read as
//! "nothing found".
//!
//! **Checking.** [`validate`] reads the patterns of a statement and says, before
//! it runs, what the graph cannot match: a label or type it does not hold, a
//! `(label)-[type]->(label)` it holds only the other way round or not at all,
//! a variable-length pattern without an upper bound, and patterns in one MATCH
//! that share no variable (every pair of their rows is returned).
//!
//! Both read the text with a scanner, not a parser: they skip strings,
//! comments and map literals, and only look inside `(...)` and `[...]` that
//! are patterns. What they cannot read is left alone, never guessed at.

use crate::ontology::Library;
use crate::schema::GraphSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A label or type in a statement, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Name {
    text: String,
    start: usize,
    end: usize,
}

/// A node pattern: its variable and labels.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Node {
    variable: Option<String>,
    labels: Vec<Name>,
}

/// A relationship pattern between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Rel {
    variable: Option<String>,
    types: Vec<Name>,
    /// `->`: `Some(true)`, `<-`: `Some(false)`, either way: `None`.
    forward: Option<bool>,
    /// A `*` with no upper bound.
    unbounded: bool,
}

/// One chain of nodes and relationships, `(a)-[r]->(b)<-[s]-(c)`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Chain {
    nodes: Vec<Node>,
    rels: Vec<Rel>,
    /// Which MATCH clause it is in (counted from 0), and which comma-separated
    /// part of it; `None` outside a MATCH.
    clause: Option<(usize, usize)>,
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The end of a quoted string or name starting at `at`.
fn past_quote(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != quote {
        i += if bytes[i] == b'\\' && quote != b'`' { 2 } else { 1 };
    }
    (i + 1).min(bytes.len())
}

/// The statement with strings, comments and map literals blanked out to
/// spaces, byte for byte, so positions stay the same.
fn blanked(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut braces = 0usize;
    while i < bytes.len() {
        let blank = |out: &mut Vec<u8>, from: usize, to: usize| out[from..to.min(bytes.len())].iter_mut().for_each(|b| *b = b' ');
        match bytes[i] {
            b'\'' | b'"' => {
                let end = past_quote(bytes, i);
                blank(&mut out, i, end);
                i = end;
            }
            b'`' => i = past_quote(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = bytes[i..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |p| i + p);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(bytes.len(), |p| i + 2 + p + 2);
                blank(&mut out, i, end);
                i = end;
            }
            b'{' => {
                braces += 1;
                out[i] = b' ';
                i += 1;
            }
            b'}' if braces > 0 => {
                braces -= 1;
                out[i] = b' ';
                i += 1;
            }
            _ => {
                if braces > 0 {
                    out[i] = b' ';
                }
                i += 1;
            }
        }
    }
    out
}

/// A name at `at` (plain or backticked) and where it ends.
fn name_at(text: &str, bytes: &[u8], at: usize) -> Option<(Name, usize)> {
    if bytes.get(at) == Some(&b'`') {
        let end = past_quote(bytes, at);
        let inner = text.get(at + 1..end.saturating_sub(1))?;
        return Some((Name { text: inner.to_string(), start: at, end }, end));
    }
    let len = bytes[at..].iter().take_while(|b| ident_byte(**b)).count();
    if len == 0 || bytes[at].is_ascii_digit() {
        return None;
    }
    Some((Name { text: text[at..at + len].to_string(), start: at, end: at + len }, at + len))
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

/// A node pattern starting at the `(` at `at`, and the index past its `)`.
/// `None` when the parentheses are not a node pattern.
fn node_at(text: &str, bytes: &[u8], at: usize) -> Option<(Node, usize)> {
    // A `(` right after a name is a function call: count(n), shortestPath(...).
    // After a keyword and a space it is a pattern: MATCH (n).
    if at > 0 && ident_byte(bytes[at - 1]) {
        return None;
    }
    let mut i = skip_space(bytes, at + 1);
    let mut node = Node::default();
    if let Some((name, end)) = name_at(text, bytes, i) {
        node.variable = Some(name.text);
        i = skip_space(bytes, end);
    }
    while bytes.get(i) == Some(&b':') {
        let (name, end) = name_at(text, bytes, skip_space(bytes, i + 1))?;
        node.labels.push(name);
        i = skip_space(bytes, end);
    }
    // A map literal was blanked; what is left must be the end.
    (bytes.get(i) == Some(&b')')).then_some((node, i + 1))
}

/// A relationship pattern starting at `at` (a `-` or `<`), and the index past
/// it. Accepts `--`, `-->`, `<--`, `-[...]-`, `-[...]->`, `<-[...]-`.
fn rel_at(text: &str, bytes: &[u8], at: usize) -> Option<(Rel, usize)> {
    let mut rel = Rel::default();
    let mut i = at;
    let mut back = false;
    if bytes.get(i) == Some(&b'<') {
        back = true;
        i += 1;
    }
    if bytes.get(i) != Some(&b'-') {
        return None;
    }
    i = skip_space(bytes, i + 1);
    if bytes.get(i) == Some(&b'[') {
        let close = i + bytes[i..].iter().position(|b| *b == b']')?;
        let inner_end = close;
        let mut j = skip_space(bytes, i + 1);
        if let Some((name, end)) = name_at(text, bytes, j) {
            rel.variable = Some(name.text);
            j = skip_space(bytes, end);
        }
        if bytes.get(j) == Some(&b':') {
            j = skip_space(bytes, j + 1);
            loop {
                let (name, end) = name_at(text, bytes, j)?;
                rel.types.push(name);
                j = skip_space(bytes, end);
                if bytes.get(j) == Some(&b'|') {
                    j = skip_space(bytes, j + 1);
                    if bytes.get(j) == Some(&b':') {
                        j = skip_space(bytes, j + 1);
                    }
                    continue;
                }
                break;
            }
        }
        if bytes.get(j) == Some(&b'*') {
            let range = String::from_utf8_lossy(&bytes[j + 1..inner_end]).replace(' ', "");
            rel.unbounded = match range.split_once("..") {
                Some((_, upper)) => upper.is_empty(),
                None => range.is_empty(),
            };
        }
        i = skip_space(bytes, close + 1);
        if bytes.get(i) != Some(&b'-') {
            return None;
        }
        i += 1;
    } else if bytes.get(i) != Some(&b'-') {
        return None;
    } else {
        i += 1;
    }
    let forward = bytes.get(i) == Some(&b'>');
    if forward {
        i += 1;
    }
    rel.forward = match (back, forward) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        (false, false) => None,
        (true, true) => return None,
    };
    Some((rel, i))
}

/// The keyword starting at `at`, upper-cased, when there is a word there.
fn word_at(bytes: &[u8], at: usize) -> Option<String> {
    if at > 0 && ident_byte(bytes[at - 1]) {
        return None;
    }
    let len = bytes[at..].iter().take_while(|b| ident_byte(**b)).count();
    (len > 0).then(|| String::from_utf8_lossy(&bytes[at..at + len]).to_ascii_uppercase())
}

/// Every pattern chain in `text`.
fn chains(text: &str) -> Vec<Chain> {
    let bytes = blanked(text);
    let mut out = Vec::new();
    let mut i = 0;
    // The MATCH clause being read, and the comma-separated part of it.
    let mut clause: Option<(usize, usize)> = None;
    let mut matches = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        if let Some(word) = word_at(&bytes, i) {
            let len = word.len();
            match word.as_str() {
                "MATCH" => {
                    clause = Some((matches, 0));
                    matches += 1;
                }
                "WHERE" | "WITH" | "RETURN" | "UNWIND" | "CREATE" | "MERGE" | "SET" | "DELETE" | "DETACH" | "REMOVE" | "ORDER" | "CALL" | "UNION" | "OPTIONAL" => clause = None,
                _ => {}
            }
            i += len;
            continue;
        }
        match bytes[i] {
            b'`' => {
                i = past_quote(&bytes, i);
                continue;
            }
            b',' if depth == 0 => {
                if let Some((c, part)) = clause {
                    clause = Some((c, part + 1));
                }
            }
            b'(' => {
                if let Some((first, mut end)) = node_at(text, &bytes, i) {
                    let mut chain = Chain { nodes: vec![first], rels: Vec::new(), clause };
                    loop {
                        let at = skip_space(&bytes, end);
                        let Some((rel, after)) = rel_at(text, &bytes, at) else { break };
                        let at = skip_space(&bytes, after);
                        if bytes.get(at) != Some(&b'(') {
                            break;
                        }
                        let Some((node, after)) = node_at(text, &bytes, at) else { break };
                        chain.rels.push(rel);
                        chain.nodes.push(node);
                        end = after;
                    }
                    out.push(chain);
                    i = end;
                    continue;
                }
                depth += 1;
            }
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    out
}

/// A replacement made in a statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    /// What was written: `Device` or `runs_on`.
    pub written: String,
    /// `label` or `type`.
    pub kind: String,
    /// The concept or relationship it names.
    pub means: String,
    /// What it became in the graph's terms.
    pub became: Vec<String>,
}

/// A statement in the graph's labels and types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewritten {
    pub text: String,
    pub replacements: Vec<Replacement>,
    pub drafts: Vec<String>,
}

/// Why a statement could not be rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unanswerable {
    /// Concepts or relationships no label or type in the graph holds.
    pub missing: Vec<String>,
}

fn quote_name(name: &str) -> String {
    if name.bytes().all(ident_byte) && !name.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        name.to_string()
    } else {
        format!("`{}`", name.replace('`', "``"))
    }
}

/// `text` with concept labels and relationship names replaced by what holds
/// them in `schema`.
pub fn rewrite(library: &Library, schema: &GraphSchema, text: &str) -> Result<Rewritten, Unanswerable> {
    let present_labels: BTreeSet<&str> = schema.labels.iter().filter(|l| l.count > 0).map(|l| l.name.as_str()).collect();
    let present_types: BTreeSet<&str> = schema.relationship_types.iter().filter(|t| t.count > 0).map(|t| t.name.as_str()).collect();
    // Replacements by position: (start, end, new text).
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut replacements: BTreeMap<(String, String), Replacement> = BTreeMap::new();
    let mut missing = BTreeSet::new();
    let (mut concepts, mut relationships) = (Vec::new(), Vec::new());
    for chain in chains(text) {
        for node in &chain.nodes {
            // `(n:A:B)` is already a union; each part is widened, and the
            // parts are written back together.
            if node.labels.is_empty() {
                continue;
            }
            let mut union: Vec<String> = Vec::new();
            let mut changed = false;
            for name in &node.labels {
                match library.concept(&name.text) {
                    Some(concept) => {
                        let concept = concept.value.name.clone();
                        let mut labels: Vec<String> = library.labels_for(&concept).into_iter().map(|(l, _)| l).filter(|l| present_labels.contains(l.as_str())).collect();
                        if present_labels.contains(name.text.as_str()) && !labels.contains(&name.text) {
                            labels.push(name.text.clone());
                        }
                        labels.sort();
                        labels.dedup();
                        if labels.is_empty() {
                            missing.insert(concept.clone());
                            continue;
                        }
                        concepts.push(concept.clone());
                        if labels != [name.text.clone()] {
                            changed = true;
                        }
                        replacements.insert(("label".into(), name.text.clone()), Replacement { written: name.text.clone(), kind: "label".into(), means: concept, became: labels.clone() });
                        union.extend(labels);
                    }
                    None => union.push(name.text.clone()),
                }
            }
            if changed {
                let start = node.labels.first().expect("labels").start;
                let end = node.labels.last().expect("labels").end;
                union.dedup();
                edits.push((start, end, union.iter().map(|l| quote_name(l)).collect::<Vec<_>>().join(":")));
            }
        }
        for rel in &chain.rels {
            if rel.types.is_empty() {
                continue;
            }
            let mut union: Vec<String> = Vec::new();
            let mut changed = false;
            for name in &rel.types {
                match library.relationship(&name.text) {
                    Some(relationship) if !present_types.contains(name.text.as_str()) => {
                        let relationship = relationship.value.name.clone();
                        let types: Vec<String> = library.types_for(&relationship).into_iter().map(|(t, _)| t).filter(|t| present_types.contains(t.as_str())).collect();
                        if types.is_empty() {
                            missing.insert(relationship.clone());
                            continue;
                        }
                        relationships.push(relationship.clone());
                        changed = true;
                        replacements.insert(("type".into(), name.text.clone()), Replacement { written: name.text.clone(), kind: "type".into(), means: relationship, became: types.clone() });
                        union.extend(types);
                    }
                    _ => union.push(name.text.clone()),
                }
            }
            if changed {
                let start = rel.types.first().expect("types").start;
                let end = rel.types.last().expect("types").end;
                let mut seen = BTreeSet::new();
                union.retain(|t| seen.insert(t.clone()));
                edits.push((start, end, union.iter().map(|t| quote_name(t)).collect::<Vec<_>>().join("|")));
            }
        }
    }
    if !missing.is_empty() {
        return Err(Unanswerable { missing: missing.into_iter().collect() });
    }
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = text.to_string();
    for (start, end, new) in edits {
        out.replace_range(start..end, &new);
    }
    Ok(Rewritten { text: out, replacements: replacements.into_values().collect(), drafts: library.drafts(&concepts, &relationships) })
}

/// Something a statement asks that the graph cannot match, or that costs
/// more than it seems to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Issue {
    /// `error`: the pattern cannot match. `warning`: it can, but see why.
    pub severity: String,
    /// `unknown_label`, `unknown_type`, `wrong_direction`, `no_such_pattern`,
    /// `unbounded`, `cartesian`.
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

fn close<'a>(word: &str, names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let lower = word.to_lowercase();
    let mut scored: Vec<(usize, String)> = names
        .map(|n| (crate::util::edit_distance(&lower, &n.to_lowercase()), n.to_string()))
        .filter(|(d, n)| *d <= (n.len() / 3).max(2))
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, n)| n).collect()
}

/// What in `text` the graph described by `schema` cannot match.
pub fn validate(schema: &GraphSchema, text: &str) -> Vec<Issue> {
    let labels: BTreeSet<&str> = schema.labels.iter().map(|l| l.name.as_str()).collect();
    let types: BTreeMap<&str, &Vec<(String, String)>> = schema.relationship_types.iter().map(|t| (t.name.as_str(), &t.patterns)).collect();
    let mut issues = BTreeSet::new();
    let issue = |severity: &str, kind: &str, message: String, suggestions: Vec<String>| Issue { severity: severity.into(), kind: kind.into(), message, suggestions };
    let found = chains(text);
    for chain in &found {
        for node in &chain.nodes {
            for name in node.labels.iter().filter(|n| !labels.contains(n.text.as_str())) {
                issues.insert(issue("error", "unknown_label", format!("the graph has no label {}", name.text), close(&name.text, labels.iter().copied())));
            }
        }
        for rel in &chain.rels {
            for name in rel.types.iter().filter(|n| !types.contains_key(n.text.as_str())) {
                issues.insert(issue("error", "unknown_type", format!("the graph has no relationship type {}", name.text), close(&name.text, types.keys().copied())));
            }
            if rel.unbounded {
                issues.insert(issue("warning", "unbounded", "a variable-length pattern has no upper bound: HawDB stops at 256 hops and fails rather than say there is no path; give one, such as *1..6".into(), Vec::new()));
            }
        }
        // Each hop between labelled ends, against the patterns the graph holds.
        for (k, rel) in chain.rels.iter().enumerate() {
            let (a, b) = (&chain.nodes[k], &chain.nodes[k + 1]);
            if a.labels.is_empty() || b.labels.is_empty() || rel.types.is_empty() || rel.unbounded || rel.forward.is_none() {
                continue;
            }
            let (from, to) = if rel.forward == Some(true) { (a, b) } else { (b, a) };
            let mut forward_hit = false;
            let mut backward_hit = false;
            for t in rel.types.iter().filter_map(|t| types.get(t.text.as_str())) {
                for (s, e) in t.iter() {
                    let s_in = |node: &Node| node.labels.iter().any(|l| &l.text == s);
                    let e_in = |node: &Node| node.labels.iter().any(|l| &l.text == e);
                    forward_hit |= s_in(from) && e_in(to);
                    backward_hit |= s_in(to) && e_in(from);
                }
            }
            if forward_hit || !rel.types.iter().all(|t| types.contains_key(t.text.as_str())) || !from.labels.iter().chain(&to.labels).all(|l| labels.contains(l.text.as_str())) {
                continue;
            }
            let shown = |node: &Node| node.labels.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(":");
            let via = rel.types.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
            let pattern = format!("(:{})-[:{via}]->(:{})", shown(from), shown(to));
            if backward_hit {
                issues.insert(issue("error", "wrong_direction", format!("the graph holds {via} only the other way round from {pattern}"), Vec::new()));
            } else {
                issues.insert(issue("error", "no_such_pattern", format!("the graph holds no {pattern}"), Vec::new()));
            }
        }
    }
    // Comma-separated parts of one MATCH that share no variable.
    let mut by_clause: BTreeMap<usize, BTreeMap<usize, BTreeSet<String>>> = BTreeMap::new();
    for chain in &found {
        if let Some((clause, part)) = chain.clause {
            let variables = by_clause.entry(clause).or_default().entry(part).or_default();
            variables.extend(chain.nodes.iter().filter_map(|n| n.variable.clone()));
            variables.extend(chain.rels.iter().filter_map(|r| r.variable.clone()));
        }
    }
    for parts in by_clause.values().filter(|parts| parts.len() > 1) {
        let parts: Vec<&BTreeSet<String>> = parts.values().collect();
        for (i, part) in parts.iter().enumerate() {
            let shared = parts.iter().enumerate().any(|(j, other)| i != j && !part.is_disjoint(other));
            if !shared {
                issues.insert(issue("warning", "cartesian", "a MATCH joins patterns that share no variable, so every pair of their rows is returned".into(), Vec::new()));
            }
        }
    }
    issues.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::schema::{LabelInfo, RelationshipTypeInfo};

    fn schema() -> GraphSchema {
        let label = |name: &str| LabelInfo { name: name.into(), count: 1, properties: vec![] };
        let rel = |name: &str, patterns: &[(&str, &str)]| RelationshipTypeInfo {
            name: name.into(),
            count: 1,
            properties: vec![],
            patterns: patterns.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        };
        GraphSchema {
            node_count: 4,
            relationship_count: 3,
            labels: vec![label("Host"), label("Server"), label("Process"), label("User")],
            relationship_types: vec![rel("RAN_ON", &[("Process", "Host")]), rel("EXEC_ON", &[("Process", "Server")]), rel("STARTED", &[("User", "Process")])],
            tables: vec![],
        }
    }

    fn library() -> Library {
        let mapping = r#"{"format": "khonsu-mapping", "version": 1, "id": "t", "revision": "1",
            "labels": [{"label": "Host", "concept": "Device"}, {"label": "Server", "concept": "Device"}, {"label": "User", "concept": "Account"},
                       {"label": "Process", "concept": "Process"}],
            "relationship_types": [{"type": "RAN_ON", "relationship": "runs_on"}, {"type": "EXEC_ON", "relationship": "runs_on"}]}"#;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.mapping.json");
        std::fs::write(&file, mapping).unwrap();
        Library::load_only(&[file])
    }

    #[test]
    fn concepts_and_relationships_become_the_graphs_labels_and_types() {
        let rewritten = rewrite(&library(), &schema(), "MATCH (p:Process)-[:runs_on]->(d:Device) WHERE d.name = 'a:Device' RETURN d").unwrap();
        assert_eq!(rewritten.text, "MATCH (p:Process)-[:EXEC_ON|RAN_ON]->(d:Host:Server) WHERE d.name = 'a:Device' RETURN d");
        let written: Vec<&str> = rewritten.replacements.iter().map(|r| r.written.as_str()).collect();
        assert_eq!(written, ["Device", "Process", "runs_on"]);
        assert!(rewritten.drafts.contains(&"concept:Device".to_string()));
    }

    #[test]
    fn labels_the_graph_has_and_maps_are_left_alone() {
        let text = "MATCH (u:User {name: 'x:Device'})-[:STARTED]->(p) // (:Device)\nRETURN count(p) AS n";
        let rewritten = rewrite(&library(), &schema(), text).unwrap();
        assert_eq!(rewritten.text, text);
    }

    #[test]
    fn a_concept_nothing_in_the_graph_holds_is_refused() {
        let err = rewrite(&library(), &schema(), "MATCH (l:Location) RETURN l").unwrap_err();
        assert_eq!(err.missing, ["Location"]);
    }

    #[test]
    fn validation_names_what_cannot_match() {
        let kinds = |text: &str| validate(&schema(), text).into_iter().map(|i| i.kind).collect::<Vec<_>>();
        assert!(kinds("MATCH (p:Process)-[:RAN_ON]->(h:Host) RETURN h").is_empty());
        assert_eq!(kinds("MATCH (h:Host)-[:RAN_ON]->(p:Process) RETURN h"), ["wrong_direction"]);
        assert_eq!(kinds("MATCH (u:User)-[:RAN_ON]->(h:Host) RETURN h"), ["no_such_pattern"]);
        assert_eq!(kinds("MATCH (h:Hots) RETURN h"), ["unknown_label"]);
        assert_eq!(validate(&schema(), "MATCH (h:Hots) RETURN h")[0].suggestions, ["Host"]);
        assert_eq!(kinds("MATCH (p:Process)-[:RAN_ONN]->(h) RETURN h"), ["unknown_type"]);
        assert_eq!(kinds("MATCH (u:User)-[:STARTED*]->(p) RETURN p"), ["unbounded"]);
        assert!(kinds("MATCH (u:User)-[:STARTED*1..3]->(p) RETURN p").is_empty());
        assert_eq!(kinds("MATCH (u:User), (h:Host) RETURN u, h"), ["cartesian"]);
        assert!(kinds("MATCH (u:User), (u)-[:STARTED]->(p) RETURN u, p").is_empty());
        // Backwards, either way, and a union of labels or types.
        assert!(kinds("MATCH (h:Host)<-[:RAN_ON]-(p:Process) RETURN h").is_empty());
        assert!(kinds("MATCH (p:Process)-[:RAN_ON]-(h:Host) RETURN h").is_empty());
        assert!(kinds("MATCH (p:Process)-[:RAN_ON|EXEC_ON]->(d:Host:Server) RETURN d").is_empty());
        // A function call is not a node pattern.
        assert!(kinds("MATCH (p:Process) RETURN count(p) AS n").is_empty());
        assert!(kinds("MATCH p = shortestPath((u:User)-[:STARTED*..4]->(h:Process)) RETURN p").is_empty());
    }
}

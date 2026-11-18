//! The ontology library: what records mean, kept in files.
//!
//! An ontology file (`*.ontology.json`) declares concepts (`Device`,
//! `Account`, ...) with what they extend, and the relationships allowed
//! between them. A mapping file (`*.mapping.json`) says which graph labels are
//! which concept and which relationship types are which relationship. Neither
//! is written into a graph: the library is read from files each time, so a
//! graph made again from its sources means the same thing (see
//! `docs/graph-contents.md`).
//!
//! The library is the built-in core ontology, then the files in the user's
//! `ontology` folder, in each plugin's `ontology` folder, and in any paths
//! given. A file that cannot be used is reported in `problems` and left out;
//! the rest still loads. Every concept, relationship and mapping is `draft`
//! until a person certifies it, and answers say which drafts they used.
//!
//! Nothing here knows any provider's labels: those are in mapping files.

use crate::investigation::{check_header, FileError};
use crate::schema::GraphSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "khonsu-ontology";
pub const MAPPING_FORMAT: &str = "khonsu-mapping";
pub const VERSION: u32 = 1;
pub const SUFFIX: &str = ".ontology.json";
pub const MAPPING_SUFFIX: &str = ".mapping.json";

/// The core ontology, shipped with KhonsuGraph.
const CORE: &str = include_str!("../ontology/core.ontology.json");

/// Whether a definition has been reviewed by a person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Draft,
    Certified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    #[serde(default)]
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub name: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    #[serde(default)]
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OntologyFile {
    pub id: String,
    pub revision: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub concepts: Vec<Concept>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelMapping {
    pub label: String,
    pub concept: String,
    /// The mapping file's status when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeMapping {
    #[serde(rename = "type")]
    pub rel_type: String,
    pub relationship: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MappingFile {
    pub id: String,
    pub revision: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub labels: Vec<LabelMapping>,
    #[serde(default)]
    pub relationship_types: Vec<TypeMapping>,
}

/// Where a file came from: `builtin`, `user`, `plugin:<id>` or `path`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

/// A file or definition that was left out, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    pub message: String,
}

/// An ontology or mapping file as identified in answers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub revision: String,
    pub origin: String,
}

/// A concept with the ontology that declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared<T> {
    pub value: T,
    pub ontology: String,
}

/// A label or relationship type's meaning, from a mapping file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapped {
    /// The concept or relationship.
    pub to: String,
    pub status: Status,
    pub mapping: String,
}

/// The loaded library, checked and indexed.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub versions: Vec<Version>,
    pub problems: Vec<Problem>,
    /// By name, as declared (case kept); looked up case-insensitively.
    concepts: BTreeMap<String, Declared<Concept>>,
    relationships: BTreeMap<String, Declared<Relationship>>,
    labels: BTreeMap<String, Vec<Mapped>>,
    types: BTreeMap<String, Vec<Mapped>>,
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn invalid(message: impl Into<String>) -> FileError {
    FileError { kind: "invalid", message: message.into() }
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl OntologyFile {
    pub fn parse(text: &str) -> Result<Self, FileError> {
        let value = check_header(text, FORMAT, VERSION)?;
        let file: Self = serde_json::from_value(value).map_err(|error| invalid(format!("its contents are not usable: {error}")))?;
        if file.id.trim().is_empty() || file.revision.trim().is_empty() {
            return Err(invalid("it needs an id and a revision"));
        }
        let mut seen = BTreeSet::new();
        for name in file.concepts.iter().map(|c| &c.name).chain(file.relationships.iter().map(|r| &r.name)) {
            if !is_name(name) {
                return Err(invalid(format!("{name:?} is not a name: use letters, digits and '_'")));
            }
            if !seen.insert(key(name)) {
                return Err(invalid(format!("{name:?} is declared twice")));
            }
        }
        Ok(file)
    }
}

impl MappingFile {
    pub fn parse(text: &str) -> Result<Self, FileError> {
        let value = check_header(text, MAPPING_FORMAT, VERSION)?;
        let file: Self = serde_json::from_value(value).map_err(|error| invalid(format!("its contents are not usable: {error}")))?;
        if file.id.trim().is_empty() || file.revision.trim().is_empty() {
            return Err(invalid("it needs an id and a revision"));
        }
        if let Some(bad) = file.labels.iter().map(|m| &m.label).chain(file.relationship_types.iter().map(|m| &m.rel_type)).find(|name| !is_name(name)) {
            return Err(invalid(format!("{bad:?} is not a label or relationship type name")));
        }
        Ok(file)
    }
}

/// The folders the library is read from, after the built-in core.
pub fn folders() -> Vec<(String, PathBuf)> {
    let mut folders = vec![("user".to_string(), crate::settings::config_dir().join("ontology"))];
    folders.extend(crate::plugin::list().into_iter().map(|plugin| (format!("plugin:{}", plugin.id()), plugin.dir.join("ontology"))));
    folders
}

/// The ontology and mapping files in `folder`, sorted, or `folder` itself
/// when it is one.
fn files_in(path: &Path) -> Vec<PathBuf> {
    let wanted = |path: &Path| path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.ends_with(SUFFIX) || name.ends_with(MAPPING_SUFFIX)
    });
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| wanted(path))
        .collect();
    files.sort();
    files
}

/// What was read, before it is checked against itself.
#[derive(Default)]
struct Read {
    ontologies: Vec<(Source, OntologyFile)>,
    mappings: Vec<(Source, MappingFile)>,
    problems: Vec<Problem>,
}

impl Read {
    fn file(&mut self, origin: &str, path: &Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let source = Source { origin: origin.into(), file: Some(path.to_path_buf()) };
        let problem = |message: String| Problem { file: Some(path.to_path_buf()), message };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return self.problems.push(problem(format!("cannot read it: {error}"))),
        };
        if name.ends_with(MAPPING_SUFFIX) {
            match MappingFile::parse(&text) {
                Ok(file) => self.mappings.push((source, file)),
                Err(error) => self.problems.push(problem(error.message)),
            }
        } else if name.ends_with(SUFFIX) {
            match OntologyFile::parse(&text) {
                Ok(file) => self.ontologies.push((source, file)),
                Err(error) => self.problems.push(problem(error.message)),
            }
        } else {
            self.problems.push(problem(format!("it is neither a {SUFFIX} nor a {MAPPING_SUFFIX} file")));
        }
    }
}

impl Library {
    /// The core ontology, the user's and plugins' folders, and `extra` (files
    /// or folders), in that order.
    pub fn load(extra: &[PathBuf]) -> Self {
        let mut read = Read::default();
        read.ontologies.push((Source { origin: "builtin".into(), file: None }, OntologyFile::parse(CORE).expect("the core ontology is valid")));
        for (origin, folder) in folders() {
            for file in files_in(&folder) {
                read.file(&origin, &file);
            }
        }
        for path in extra {
            if !path.exists() {
                read.problems.push(Problem { file: Some(path.clone()), message: "it does not exist".into() });
            }
            for file in files_in(path) {
                read.file("path", &file);
            }
        }
        Self::build(read)
    }

    /// Only the core ontology and the given files: what tests and a person
    /// checking a pack use, without what happens to be installed.
    pub fn load_only(extra: &[PathBuf]) -> Self {
        let mut read = Read::default();
        read.ontologies.push((Source { origin: "builtin".into(), file: None }, OntologyFile::parse(CORE).expect("the core ontology is valid")));
        for path in extra {
            for file in files_in(path) {
                read.file("path", &file);
            }
        }
        Self::build(read)
    }

    fn build(read: Read) -> Self {
        let mut library = Library { problems: read.problems, ..Library::default() };
        let mut ids = BTreeSet::new();
        for (source, file) in &read.ontologies {
            let left_out = |message: String| Problem { file: source.file.clone(), message };
            if !ids.insert(file.id.clone()) {
                library.problems.push(left_out(format!("ontology {:?} is already loaded; this copy is left out", file.id)));
                continue;
            }
            library.versions.push(Version { id: file.id.clone(), revision: file.revision.clone(), origin: source.origin.clone() });
            for concept in &file.concepts {
                if let Some(other) = library.concepts.get(&key(&concept.name)) {
                    library.problems.push(left_out(format!("concept {} is already declared by {}; left out", concept.name, other.ontology)));
                    continue;
                }
                library.concepts.insert(key(&concept.name), Declared { value: concept.clone(), ontology: file.id.clone() });
            }
            for relationship in &file.relationships {
                if let Some(other) = library.relationships.get(&key(&relationship.name)) {
                    library.problems.push(left_out(format!("relationship {} is already declared by {}; left out", relationship.name, other.ontology)));
                    continue;
                }
                library.relationships.insert(key(&relationship.name), Declared { value: relationship.clone(), ontology: file.id.clone() });
            }
        }
        // References are checked once everything is in, so files may come in any order.
        let names: BTreeSet<String> = library.concepts.keys().cloned().collect();
        for declared in library.concepts.values_mut() {
            let concept = &mut declared.value;
            let unknown: Vec<String> = concept.extends.iter().filter(|parent| !names.contains(&key(parent))).cloned().collect();
            if !unknown.is_empty() {
                library.problems.push(Problem { file: None, message: format!("{} extends {}, which no ontology declares; that is ignored", concept.name, unknown.join(", ")) });
                concept.extends.retain(|parent| names.contains(&key(parent)));
            }
        }
        for name in library.concepts.keys().cloned().collect::<Vec<_>>() {
            if library.ancestors_strict(&name).contains(&name) {
                library.problems.push(Problem { file: None, message: format!("{} extends itself through its parents; its parents are ignored", library.concepts[&name].value.name) });
                library.concepts.get_mut(&name).expect("present").value.extends.clear();
            }
        }
        let bad: Vec<String> = library
            .relationships
            .iter()
            .filter(|(_, r)| !names.contains(&key(&r.value.from)) || !names.contains(&key(&r.value.to)))
            .map(|(k, _)| k.clone())
            .collect();
        for k in bad {
            let r = library.relationships.remove(&k).expect("present");
            library.problems.push(Problem { file: None, message: format!("relationship {} joins {} to {}, and one of them is not a concept; left out", r.value.name, r.value.from, r.value.to) });
        }
        for (source, file) in &read.mappings {
            let left_out = |message: String| Problem { file: source.file.clone(), message };
            if !ids.insert(file.id.clone()) {
                library.problems.push(left_out(format!("{:?} is already loaded; this copy is left out", file.id)));
                continue;
            }
            library.versions.push(Version { id: file.id.clone(), revision: file.revision.clone(), origin: source.origin.clone() });
            for mapping in &file.labels {
                let Some(concept) = library.concepts.get(&key(&mapping.concept)) else {
                    library.problems.push(left_out(format!("label {} maps to {}, which is not a concept; left out", mapping.label, mapping.concept)));
                    continue;
                };
                let mapped = Mapped { to: concept.value.name.clone(), status: mapping.status.unwrap_or(file.status), mapping: file.id.clone() };
                library.labels.entry(mapping.label.clone()).or_default().push(mapped);
            }
            for mapping in &file.relationship_types {
                let Some(relationship) = library.relationships.get(&key(&mapping.relationship)) else {
                    library.problems.push(left_out(format!("type {} maps to {}, which is not a relationship; left out", mapping.rel_type, mapping.relationship)));
                    continue;
                };
                let mapped = Mapped { to: relationship.value.name.clone(), status: mapping.status.unwrap_or(file.status), mapping: file.id.clone() };
                library.types.entry(mapping.rel_type.clone()).or_default().push(mapped);
            }
        }
        library
    }

    pub fn concept(&self, name: &str) -> Option<&Declared<Concept>> {
        self.concepts.get(&key(name))
    }

    pub fn relationship(&self, name: &str) -> Option<&Declared<Relationship>> {
        self.relationships.get(&key(name))
    }

    pub fn concepts(&self) -> impl Iterator<Item = &Declared<Concept>> {
        self.concepts.values()
    }

    pub fn relationships(&self) -> impl Iterator<Item = &Declared<Relationship>> {
        self.relationships.values()
    }

    /// What `label` is, by the mappings.
    pub fn label(&self, label: &str) -> &[Mapped] {
        self.labels.get(label).map(Vec::as_slice).unwrap_or_default()
    }

    /// What relationship type `rel_type` is, by the mappings.
    pub fn rel_type(&self, rel_type: &str) -> &[Mapped] {
        self.types.get(rel_type).map(Vec::as_slice).unwrap_or_default()
    }

    /// Concepts `name` extends, directly or not, without itself.
    fn ancestors_strict(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([key(name)]);
        while let Some(next) = queue.pop_front() {
            for parent in self.concepts.get(&next).map(|c| c.value.extends.clone()).unwrap_or_default() {
                if seen.insert(key(&parent)) {
                    queue.push_back(key(&parent));
                }
            }
        }
        seen
    }

    /// `name` and every concept it extends, by declared name.
    pub fn ancestors(&self, name: &str) -> Vec<String> {
        let mut out = vec![key(name)];
        out.extend(self.ancestors_strict(name));
        out.iter().filter_map(|k| self.concepts.get(k)).map(|c| c.value.name.clone()).collect()
    }

    /// `name` and every concept that extends it, directly or not.
    pub fn descendants(&self, name: &str) -> Vec<String> {
        let target = key(name);
        self.concepts
            .values()
            .filter(|c| key(&c.value.name) == target || self.ancestors_strict(&c.value.name).contains(&target))
            .map(|c| c.value.name.clone())
            .collect()
    }

    /// Whether `a` is `b` or extends it.
    pub fn is_a(&self, a: &str, b: &str) -> bool {
        key(a) == key(b) || self.ancestors_strict(a).contains(&key(b))
    }

    /// The labels that are `concept` or something that extends it.
    pub fn labels_for(&self, concept: &str) -> Vec<(String, Mapped)> {
        let wanted: BTreeSet<String> = self.descendants(concept).iter().map(|c| key(c)).collect();
        self.labels
            .iter()
            .flat_map(|(label, mapped)| mapped.iter().filter(|m| wanted.contains(&key(&m.to))).map(move |m| (label.clone(), m.clone())))
            .collect()
    }

    /// The types mapped to `relationship`.
    pub fn types_for(&self, relationship: &str) -> Vec<(String, Mapped)> {
        let wanted = key(relationship);
        self.types
            .iter()
            .flat_map(|(rel_type, mapped)| mapped.iter().filter(|m| key(&m.to) == wanted).map(move |m| (rel_type.clone(), m.clone())))
            .collect()
    }

    /// Every label any mapping names.
    pub fn mapped_labels(&self) -> impl Iterator<Item = &String> {
        self.labels.keys()
    }

    /// The draft definitions behind `concepts` and `relationships` (names),
    /// including what they extend.
    pub fn drafts(&self, concepts: &[String], relationships: &[String]) -> Vec<String> {
        let mut out = BTreeSet::new();
        for concept in concepts {
            for name in self.ancestors(concept) {
                if self.concept(&name).is_some_and(|c| c.value.status == Status::Draft) {
                    out.insert(format!("concept:{name}"));
                }
            }
        }
        for relationship in relationships {
            if let Some(r) = self.relationship(relationship).filter(|r| r.value.status == Status::Draft) {
                out.insert(format!("relationship:{}", r.value.name));
            }
        }
        out.into_iter().collect()
    }
}

/// How a word was understood.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Match {
    /// `concept`, `relationship`, `label` or `type`.
    pub kind: String,
    /// The concept or relationship it means.
    pub name: String,
    /// `name`, `synonym`, `label` (a graph label mapped to it) or `type`.
    pub via: String,
    pub status: Status,
}

/// What `word` means in the library: a concept or relationship by name or
/// synonym, or a label or relationship type a mapping gives a meaning.
/// Case and the difference between spaces, `-` and `_` are ignored.
pub fn resolve(library: &Library, word: &str) -> Vec<Match> {
    let plain = |text: &str| text.trim().to_lowercase().replace(['-', '_'], " ");
    let wanted = plain(word);
    let mut out = BTreeSet::new();
    for c in library.concepts() {
        let concept = &c.value;
        if plain(&concept.name) == wanted {
            out.insert(Match { kind: "concept".into(), name: concept.name.clone(), via: "name".into(), status: concept.status });
        } else if concept.synonyms.iter().any(|s| plain(s) == wanted) {
            out.insert(Match { kind: "concept".into(), name: concept.name.clone(), via: "synonym".into(), status: concept.status });
        }
    }
    for r in library.relationships() {
        let relationship = &r.value;
        if plain(&relationship.name) == wanted {
            out.insert(Match { kind: "relationship".into(), name: relationship.name.clone(), via: "name".into(), status: relationship.status });
        } else if relationship.synonyms.iter().any(|s| plain(s) == wanted) {
            out.insert(Match { kind: "relationship".into(), name: relationship.name.clone(), via: "synonym".into(), status: relationship.status });
        }
    }
    for (label, mapped) in &library.labels {
        if plain(label) == wanted {
            for m in mapped {
                out.insert(Match { kind: "concept".into(), name: m.to.clone(), via: format!("label:{label}"), status: m.status });
            }
        }
    }
    for (rel_type, mapped) in &library.types {
        if plain(rel_type) == wanted {
            for m in mapped {
                out.insert(Match { kind: "relationship".into(), name: m.to.clone(), via: format!("type:{rel_type}"), status: m.status });
            }
        }
    }
    out.into_iter().collect()
}

/// Names close to `word`, for "did you mean".
pub fn suggestions(library: &Library, word: &str) -> Vec<String> {
    let wanted = word.trim().to_lowercase();
    let mut scored: Vec<(usize, String)> = library
        .concepts()
        .flat_map(|c| std::iter::once(c.value.name.clone()).chain(c.value.synonyms.clone()))
        .chain(library.relationships().map(|r| r.value.name.clone()))
        .chain(library.labels.keys().cloned())
        .map(|name| (crate::util::edit_distance(&wanted, &name.to_lowercase()), name))
        .filter(|(distance, name)| *distance <= (name.len() / 3).max(2))
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(5).map(|(_, name)| name).collect()
}

/// Whether a graph holds what a question about `terms` needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub terms: Vec<TermCoverage>,
    /// Terms the graph can answer for, out of all.
    pub covered: usize,
    pub total: usize,
    pub drafts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermCoverage {
    pub term: String,
    pub matches: Vec<Match>,
    /// Labels or types in the graph that hold it, with their counts.
    pub found: Vec<Found>,
    /// Why not, when nothing was found: `unknown_term`, `no_mapping` or
    /// `not_in_graph`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    /// `label` or `type`.
    pub kind: String,
    pub name: String,
    pub count: u64,
}

impl Coverage {
    pub fn complete(&self) -> bool {
        self.covered == self.total
    }
}

/// For each term: what it means, and which labels or types of `schema` hold
/// records of it. A term is covered when at least one does.
pub fn check(library: &Library, schema: &GraphSchema, terms: &[String]) -> Coverage {
    let label_counts: BTreeMap<&str, u64> = schema.labels.iter().map(|l| (l.name.as_str(), l.count)).collect();
    let type_counts: BTreeMap<&str, u64> = schema.relationship_types.iter().map(|t| (t.name.as_str(), t.count)).collect();
    let mut out = Vec::new();
    let (mut concepts, mut relationships) = (Vec::new(), Vec::new());
    for term in terms {
        let matches = resolve(library, term);
        let mut found = BTreeMap::new();
        let mut mapped_any = false;
        for m in &matches {
            if m.kind == "concept" {
                concepts.push(m.name.clone());
                for (label, _) in library.labels_for(&m.name) {
                    mapped_any = true;
                    if let Some(count) = label_counts.get(label.as_str()).filter(|count| **count > 0) {
                        found.insert(("label".to_string(), label.clone()), *count);
                    }
                }
            } else {
                relationships.push(m.name.clone());
                for (rel_type, _) in library.types_for(&m.name) {
                    mapped_any = true;
                    if let Some(count) = type_counts.get(rel_type.as_str()).filter(|count| **count > 0) {
                        found.insert(("type".to_string(), rel_type.clone()), *count);
                    }
                }
            }
        }
        let missing = if !found.is_empty() {
            None
        } else if matches.is_empty() {
            Some("unknown_term".to_string())
        } else if !mapped_any {
            Some("no_mapping".to_string())
        } else {
            Some("not_in_graph".to_string())
        };
        let suggestions = if matches.is_empty() { suggestions(library, term) } else { Vec::new() };
        out.push(TermCoverage {
            term: term.clone(),
            matches,
            found: found.into_iter().map(|((kind, name), count)| Found { kind, name, count }).collect(),
            missing,
            suggestions,
        });
    }
    let covered = out.iter().filter(|t| t.missing.is_none()).count();
    Coverage { total: out.len(), covered, terms: out, drafts: library.drafts(&concepts, &relationships) }
}

/// One step of a path between concepts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Step {
    /// The concept or label the step leaves from.
    pub from: String,
    /// The relationship (declared paths) or relationship type (graph paths).
    pub via: String,
    pub to: String,
    /// True when the step goes against the relationship's direction.
    pub reverse: bool,
    /// For a graph step: the relationship the type is, by the mappings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub means: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Route {
    pub steps: Vec<Step>,
    /// For a graph path: the Cypher pattern, with `a` and `b` its ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// The most paths returned.
pub const MAX_PATHS: usize = 20;

/// The fewest-hop ways from `from` to `to`, within `max_hops`, over the
/// relationships the ontology declares. A relationship declared on a concept
/// holds for what extends it, either way along it.
pub fn declared_paths(library: &Library, from: &str, to: &str, max_hops: usize) -> Vec<Route> {
    let (Some(from), Some(to)) = (library.concept(from), library.concept(to)) else { return Vec::new() };
    let (from, to) = (from.value.name.clone(), to.value.name.clone());
    let reached = |concept: &str| library.is_a(concept, &to) || library.is_a(&to, concept);
    let edges = |concept: &str| -> Vec<Step> {
        let mut out = Vec::new();
        for r in library.relationships() {
            let r = &r.value;
            if library.is_a(concept, &r.from) {
                out.push(Step { from: concept.to_string(), via: r.name.clone(), to: r.to.clone(), reverse: false, means: None });
            }
            if library.is_a(concept, &r.to) {
                out.push(Step { from: concept.to_string(), via: r.name.clone(), to: r.from.clone(), reverse: true, means: None });
            }
        }
        out
    };
    shortest(&from, max_hops, reached, edges).into_iter().map(|steps| Route { steps, pattern: None }).collect()
}

/// The fewest-hop ways between the labels of `from` and of `to` in a graph,
/// within `max_hops`, over the `(label)-[type]->(label)` patterns the graph
/// holds, either way along them.
pub fn graph_paths(library: &Library, schema: &GraphSchema, from: &str, to: &str, max_hops: usize) -> Vec<Route> {
    let starts: BTreeSet<String> = library.labels_for(from).into_iter().map(|(label, _)| label).collect();
    let ends: BTreeSet<String> = library.labels_for(to).into_iter().map(|(label, _)| label).collect();
    let present: BTreeSet<&str> = schema.labels.iter().filter(|l| l.count > 0).map(|l| l.name.as_str()).collect();
    let mut out = Vec::new();
    for start in starts.iter().filter(|label| present.contains(label.as_str())) {
        let edges = |label: &str| -> Vec<Step> {
            let mut steps = Vec::new();
            for t in &schema.relationship_types {
                let means = library.rel_type(&t.name).first().map(|m| m.to.clone());
                for (source, target) in &t.patterns {
                    if source == label {
                        steps.push(Step { from: label.to_string(), via: t.name.clone(), to: target.clone(), reverse: false, means: means.clone() });
                    }
                    if target == label && source != target {
                        steps.push(Step { from: label.to_string(), via: t.name.clone(), to: source.clone(), reverse: true, means: means.clone() });
                    }
                }
            }
            steps
        };
        for steps in shortest(start, max_hops, |label| ends.contains(label), edges) {
            let pattern = cypher_pattern(&steps);
            out.push(Route { steps, pattern: Some(pattern) });
        }
    }
    out.sort_by_key(|p| p.steps.len());
    out.truncate(MAX_PATHS);
    out
}

fn cypher_pattern(steps: &[Step]) -> String {
    let mut text = format!("(a:{})", steps.first().map(|s| s.from.as_str()).unwrap_or_default());
    for (i, step) in steps.iter().enumerate() {
        let end = if i + 1 == steps.len() { format!("(b:{})", step.to) } else { format!("(:{})", step.to) };
        if step.reverse {
            text.push_str(&format!("<-[:{}]-{end}", step.via));
        } else {
            text.push_str(&format!("-[:{}]->{end}", step.via));
        }
    }
    text
}

/// Every path of the fewest hops from `start` to something `reached` accepts,
/// visiting no node twice, up to [`MAX_PATHS`]. A start that is already
/// reached gives no path: there is nothing to walk.
fn shortest(start: &str, max_hops: usize, reached: impl Fn(&str) -> bool, edges: impl Fn(&str) -> Vec<Step>) -> Vec<Vec<Step>> {
    let mut frontier: Vec<Vec<Step>> = vec![Vec::new()];
    for _ in 0..max_hops {
        let mut next = Vec::new();
        let mut done = Vec::new();
        for path in &frontier {
            let at = path.last().map(|s| s.to.clone()).unwrap_or_else(|| start.to_string());
            let visited: BTreeSet<&str> = std::iter::once(start).chain(path.iter().map(|s| s.to.as_str())).collect();
            for step in edges(&at) {
                if visited.contains(step.to.as_str()) && !reached(&step.to) {
                    continue;
                }
                let mut longer = path.clone();
                let arrived = reached(&step.to);
                longer.push(step);
                if arrived {
                    done.push(longer);
                } else {
                    next.push(longer);
                }
            }
        }
        if !done.is_empty() {
            done.sort();
            done.dedup();
            done.truncate(MAX_PATHS);
            return done;
        }
        if next.is_empty() {
            break;
        }
        // Keep the search bounded on a dense schema: the first paths found are enough.
        next.truncate(10_000);
        frontier = next;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{LabelInfo, RelationshipTypeInfo};

    fn library(extra: &str) -> Library {
        let mut read = Read::default();
        read.ontologies.push((Source { origin: "builtin".into(), file: None }, OntologyFile::parse(CORE).unwrap()));
        read.mappings.push((Source { origin: "test".into(), file: None }, MappingFile::parse(extra).unwrap()));
        Library::build(read)
    }

    const MAPPING: &str = r#"{"format": "khonsu-mapping", "version": 1, "id": "lab", "revision": "1",
        "labels": [{"label": "Host", "concept": "Device"}, {"label": "User", "concept": "Account"},
                   {"label": "Process", "concept": "Process"}, {"label": "IP", "concept": "IPAddress", "status": "certified"},
                   {"label": "Ghost", "concept": "NoSuchThing"}],
        "relationship_types": [{"type": "RAN_ON", "relationship": "runs_on"}, {"type": "STARTED", "relationship": "acts_on"}]}"#;

    fn schema() -> GraphSchema {
        let label = |name: &str, count| LabelInfo { name: name.into(), count, properties: vec![] };
        let rel = |name: &str, patterns: &[(&str, &str)]| RelationshipTypeInfo {
            name: name.into(),
            count: 3,
            properties: vec![],
            patterns: patterns.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        };
        GraphSchema {
            node_count: 10,
            relationship_count: 9,
            labels: vec![label("Host", 2), label("User", 1), label("Process", 5), label("Technique", 2)],
            relationship_types: vec![rel("RAN_ON", &[("Process", "Host")]), rel("STARTED", &[("User", "Process")]), rel("SPAWNED", &[("Process", "Process")])],
            tables: vec![],
        }
    }

    #[test]
    fn the_core_ontology_loads_without_problems() {
        let library = Library::load_only(&[]);
        assert!(library.problems.is_empty(), "{:?}", library.problems);
        assert!(library.is_a("Person", "Entity"));
        assert!(library.is_a("IPAddress", "Identifier"));
        assert!(!library.is_a("Identifier", "Entity"));
        assert_eq!(library.versions[0].id, "khonsu-core");
    }

    #[test]
    fn a_mapping_to_an_unknown_concept_is_reported_and_left_out() {
        let library = library(MAPPING);
        assert!(library.label("Ghost").is_empty());
        assert!(library.problems.iter().any(|p| p.message.contains("NoSuchThing")), "{:?}", library.problems);
        assert_eq!(library.label("IP")[0].status, Status::Certified);
        assert_eq!(library.label("Host")[0].status, Status::Draft);
    }

    #[test]
    fn words_resolve_by_name_synonym_and_label() {
        let library = library(MAPPING);
        let names = |word: &str| resolve(&library, word).into_iter().map(|m| format!("{}:{}:{}", m.kind, m.name, m.via)).collect::<Vec<_>>();
        assert_eq!(names("device"), ["concept:Device:name"]);
        assert!(names("host").contains(&"concept:Device:synonym".to_string()));
        assert!(names("host").contains(&"concept:Device:label:Host".to_string()));
        assert!(names("ran_on").contains(&"relationship:runs_on:type:RAN_ON".to_string()));
        assert!(names("blorp").is_empty());
        assert_eq!(suggestions(&library, "devise"), ["Device"]);
    }

    #[test]
    fn a_concept_covers_the_labels_of_what_extends_it() {
        let library = library(MAPPING);
        let labels: Vec<String> = library.labels_for("Asset").into_iter().map(|(label, _)| label).collect();
        assert_eq!(labels, ["Host"]);
        let coverage = check(&library, &schema(), &["device".into(), "account".into(), "ip".into(), "blorp".into(), "location".into()]);
        let missing: Vec<Option<&str>> = coverage.terms.iter().map(|t| t.missing.as_deref()).collect();
        assert_eq!(missing, [None, None, Some("not_in_graph"), Some("unknown_term"), Some("no_mapping")]);
        assert_eq!((coverage.covered, coverage.total), (2, 5));
        assert!(!coverage.complete());
        assert!(coverage.drafts.contains(&"concept:Device".to_string()));
    }

    #[test]
    fn declared_paths_follow_relationships_and_what_extends_them() {
        let library = library(MAPPING);
        let paths = declared_paths(&library, "Process", "Device", 3);
        assert_eq!(paths[0].steps.len(), 1);
        assert!(paths.iter().any(|p| p.steps[0].via == "runs_on"));
        // Person is an Agent, which holds an Account: one hop, against nothing.
        let paths = declared_paths(&library, "Person", "Account", 3);
        assert!(paths.iter().any(|p| p.steps.len() == 1 && p.steps[0].via == "holds" && !p.steps[0].reverse));
    }

    #[test]
    fn graph_paths_give_a_cypher_pattern_over_the_labels_present() {
        let library = library(MAPPING);
        let paths = graph_paths(&library, &schema(), "Account", "Device", 4);
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert_eq!(paths[0].pattern.as_deref(), Some("(a:User)-[:STARTED]->(:Process)-[:RAN_ON]->(b:Host)"));
        assert_eq!(paths[0].steps[1].means.as_deref(), Some("runs_on"));
        let back = graph_paths(&library, &schema(), "Device", "Account", 4);
        assert_eq!(back[0].pattern.as_deref(), Some("(a:Host)<-[:RAN_ON]-(:Process)<-[:STARTED]-(b:User)"));
        assert!(graph_paths(&library, &schema(), "Account", "Device", 1).is_empty());
    }
}

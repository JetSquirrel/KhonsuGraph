# Ontology

A graph's labels say where records came from: `Host`, `EntraUser`,
`WazuhAgent`. The ontology says what they are: a `Device`, an `Account`. With
it, a question can be asked in the same words whatever sources a graph was
built from, and an agent can find out whether a graph can answer it before it
writes a query.

The ontology lives in files, never in the graph (see
[what goes in a graph](graph-contents.md)).

## Files

**An ontology file** (`*.ontology.json`) declares concepts and the relationships
allowed between them:

```json
{
  "format": "khonsu-ontology",
  "version": 1,
  "id": "attack",
  "revision": "0.1.0",
  "concepts": [
    { "name": "Technique", "description": "An ATT&CK technique.", "synonyms": ["ttp"] }
  ],
  "relationships": [
    { "name": "indicates", "from": "Entity", "to": "Technique", "description": "Something seen matches a rule for the technique." }
  ]
}
```

A concept may `extends` others, from any loaded ontology. A relationship holds
for whatever extends its ends: `holds` from `Agent` to `Account` also joins a
`Person` to an `Account`. Names are letters, digits and `_`, and unique across
the library.

**A mapping file** (`*.mapping.json`) says what a graph's labels and relationship
types are:

```json
{
  "format": "khonsu-mapping",
  "version": 1,
  "id": "security-example",
  "revision": "1",
  "status": "draft",
  "labels": [{ "label": "Host", "concept": "Device" }],
  "relationship_types": [{ "type": "RAN_ON", "relationship": "runs_on" }]
}
```

Every concept, relationship and mapping is `draft` until a person reviews it
and sets `"status": "certified"`. Answers list the drafts they rest on under
`drafts`; an agent says so when it reports one.

## Where the library comes from

In this order, the first copy of an ID winning:

1. `khonsu-core`, built in: about twenty concepts every investigation shares
   (`Entity`, `Agent`, `Person`, `Organization`, `Account`, `Asset`, `Device`,
   `Resource`, `Location`, `Artifact`, `Process`, `Identifier`, `IPAddress`,
   `Event`, `Observation`, ...) and the relationships between them. Domain
   packs extend these; they don't change them.
2. `ontology/` in the config folder.
3. `ontology/` in each installed plugin's folder, so a plugin ships the meaning
   of the labels it writes.
4. Each `--ontology PATH` given, a file or a folder.

A file that can't be used, a duplicate name, a reference to a concept nobody
declares, or a cycle in `extends` is reported under `problems` and left out;
the rest still loads.

The security example's ATT&CK pack and mapping are in
[`examples/security/ontology`](../examples/security/ontology).

## Commands

```sh
khonsu ontology describe [--brief] [--concept NAME] [-d DIR] [--ontology PATH]
khonsu ontology resolve WORD... [--ontology PATH]
khonsu ontology check -d DIR --entities account,device,ip [--ontology PATH]
khonsu ontology path --from CONCEPT --to CONCEPT [--max-hops 4] [-d DIR] [--ontology PATH]
```

- **describe** lists the concepts and relationships, or one concept in full: what
  it extends, what extends it, its relationships and the labels that hold it.
  With `-d` it compares the library with the graph: labels that are mapped
  (with counts), labels that aren't, relationship types that aren't, and labels
  mapped but absent.
- **resolve** says what words mean: a concept or relationship by name or
  synonym, or a label or type a mapping gives a meaning, with the labels and
  types that hold it. A word that means nothing gets `suggestions`.
- **check** says, for each thing a question is about, whether the graph holds
  records of it. A term is `missing` as `unknown_term` (the library doesn't know
  the word), `no_mapping` (no label is mapped to it) or `not_in_graph` (the
  labels mapped to it have no records here). When any is missing it exits **3**
  with kind `not_investigable`, and the coverage is under `error.detail`.
- **path** gives the fewest-hop ways from one concept to another: `declared`,
  over the relationships the ontology allows, and with `-d` `graph.paths`, over
  the `(label)-[type]->(label)` patterns the graph actually holds, each with a
  Cypher `pattern` whose ends are `a` and `b`, ready for `MATCH`. A path is a
  way the data connects, not proof that anything happened along it.

Every answer carries `ontology_versions`: the ontology and mapping files used,
by ID, revision and origin.

## Queries in the ontology's words

```sh
khonsu query -d DIR --semantic --cypher "MATCH (t:Technique)<-[:indicates]-(p:Process)-[:runs_on]->(d:Device) RETURN t.name, d.name"
khonsu query -d DIR --validate-only --cypher "MATCH (h:Host)-[:RAN_ON]->(p:Process) RETURN p"
```

With `--semantic`, a label that names a concept becomes every label in this
graph mapped to it or to what extends it, and a relationship type that names a
relationship becomes the types mapped to it: `(d:Device)` becomes
`(d:Host:Server)` and `[:runs_on]` becomes `[:EXEC_ON|RAN_ON]` (HawDB reads both
as "any of"). Labels and types the graph has are left as written. A concept
nothing in the graph holds exits 3 rather than answer with no rows. The result
has `semantic`: the `statement` that ran, each replacement, the `drafts` used
and `ontology_versions`. A semantic query only reads; `--write` is refused with
it.

Before it runs, and alone with `--validate-only` (which runs nothing), the
statement is checked against the graph:

| Kind | Severity | What |
|---|---|---|
| `unknown_label`, `unknown_type` | error | The graph has no such label or relationship type; `suggestions` has close names |
| `wrong_direction` | error | The graph holds `(A)-[T]->(B)` only the other way round |
| `no_such_pattern` | error | The graph holds no `(A)-[T]->(B)` either way |
| `unbounded` | warning | A variable-length pattern with no upper bound: HawDB stops at 256 hops and fails |
| `cartesian` | warning | Parts of one MATCH share no variable, so every pair of their rows comes back |

An error exits 2 with kind `invalid_query` and the issues under `error.detail`;
warnings are reported and the statement runs. The check reads patterns with a
scanner, not a parser: it skips strings, comments and maps, and what it cannot
read it leaves alone rather than guess.

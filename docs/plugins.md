# Writing a data-source plugin

A plugin turns some outside system into nodes and relationships. KhonsuGraph
runs it as a separate program, talks to it in JSON lines over stdin/stdout,
and writes what it reports into the graph. A plugin never opens the graph
itself, so it can be written in any language and cannot corrupt a graph.

The official plugins are worked examples: `plugins/csv` (local files),
`plugins/aws` (a cloud account through the `aws` CLI) and
[`plugins/neo4j`](../plugins/neo4j/README.md) (another graph database). The smallest complete
plugin is `examples/plugins/folders`, about a hundred lines of Python with no
dependencies.

Protocol version 1 is experimental and may change before 1.0.

## The folder

```text
folders/
  khonsu-plugin.json   # the manifest, read without running anything
  plugin.py            # whatever the manifest's entry runs
```

```json
{
  "id": "folders",
  "name": "Folders",
  "version": "0.1.0",
  "protocol": 1,
  "entry": ["python3", "plugin.py"],
  "description": "A folder tree as Folder and File nodes joined by CONTAINS",
  "permissions": { "read_files": true, "commands": ["python3"] }
}
```

`entry` is the command and its arguments. The first element is either a file
in the plugin folder (`bin/sync`, or a compiled `khonsu-plugin-foo`; `.exe` is
added on Windows) or, when no such file exists and the name has no folder in
it, a program on `PATH` such as `python3` or `node`. The program runs with the
plugin folder as its working directory.

`permissions` is what the person approves before the first sync, and again
whenever the version, the permissions or any file in the plugin's folder changes:

| Field | Meaning |
| --- | --- |
| `network` | Domains the plugin contacts, e.g. `["*.amazonaws.com"]` |
| `read_files` | Whether it reads local files the person points it at |
| `commands` | Other programs it runs, e.g. `["aws"]` |
| `notes` | Anything else worth knowing, e.g. "a read-only IAM policy is enough" |

Declare everything the plugin does. KhonsuGraph shows these to the person; it
does not sandbox the plugin, and does not enforce them. See
[plugin security](plugin-security.md).

`entry` must stay inside the plugin's folder, apart from a bare program name found on
`PATH`. Don't write files into the folder as the plugin runs, except what an
interpreter caches in `__pycache__`, or it will need approval again each time.

## The three commands

The host starts the program once per operation, with the operation as the
last argument:

| Run | stdin | stdout |
| --- | --- | --- |
| `<entry> describe` | nothing | one `describe` message |
| `<entry> test` | one request line | one `test_result` (or `error`) message |
| `<entry> sync` | one request line | records, progress, then `done` (or `error`) |

Every message is a JSON object on its own line, tagged by `type`. Anything on
stderr is kept as diagnostics and shown when the plugin fails. To cancel, the
host kills the process.

### describe

```json
{"type": "describe", "name": "Folders", "version": "0.1.0",
 "description": "A folder tree as Folder and File nodes joined by CONTAINS",
 "config": [
   {"key": "root", "label": "Folder", "kind": "file", "required": true, "help": "The folder to read"},
   {"key": "max_files", "label": "File limit", "kind": "number", "default": 5000}
 ],
 "permissions": {"read_files": true, "commands": ["python3"]},
 "labels": ["Folder", "File"],
 "relationship_types": ["CONTAINS"],
 "supports_incremental": false}
```

`config` drives the source form in the app and `-c key=value` in the CLI.
Field `kind` is one of `string`, `number`, `bool`, `file` or `json`. A field
with `"secret": true` is stored in the system keychain, never in KhonsuGraph's
settings or logs, and reaches the plugin only in the request's `secrets`.

### The request

```json
{"protocol": 1,
 "config": {"root": "/Users/me/projects", "max_files": 5000},
 "secrets": {},
 "mode": "full",
 "cursor": null}
```

`mode` is `full` or `incremental`. Incremental is sent only when `describe`
said `supports_incremental`, and comes with the last `cursor` the plugin
reported.

### test

Check the configuration the cheapest way you can (the folder exists, the
credentials work) and answer once:

```json
{"type": "test_result", "ok": true, "message": "can read /Users/me/projects"}
```

### sync

Stream records in any order, then finish with `done`:

```json
{"type": "node", "id": "src", "label": "Folder", "properties": {"name": "src"}}
{"type": "node", "id": "src/main.rs", "label": "File", "properties": {"name": "main.rs", "bytes": 1204}}
{"type": "edge", "id": "src>src/main.rs", "source": "src", "target": "src/main.rs", "rel_type": "CONTAINS"}
{"type": "progress", "message": "src", "current": 1, "total": 12}
{"type": "log", "level": "warn", "message": "skipped a file the plugin could not read"}
{"type": "cursor", "value": {"since": "2026-10-16T08:00:00Z"}}
{"type": "done", "nodes": 2, "edges": 1}
```

A sync that ends without `done` is reported as failed. Records already
written are kept and the next sync reconciles them. To fail on purpose, send
`{"type": "error", "message": "..."}`; the person sees the message.

A full sync removes records it did not see, so only finish with a plain `done`
when you enumerated the whole source. If part of it could not be read and you
still have something useful to send, say so:

```json
{"type": "done", "nodes": 2, "edges": 1, "complete": false, "gaps": ["page 3 timed out"]}
```

The sync is then recorded as `partial`: nothing is removed, the cursor doesn't
move, and exposure findings can't read the absence of a record as a clear result.
Leaving out `complete` means complete. Don't put secrets in `gaps` or in messages;
values from the source's secret fields are masked before they are kept.

## Offering an analysis

A plugin can also say something about a graph, as well as fill one. This is
optional, and a plugin that offers nothing goes on working as before. The AWS
plugin's [exposure rules](../plugins/aws/README.md) are one, and
`crates/khonsu-plugin-protocol/examples/analyzer.rs` is a small one with nothing to
do with AWS.

**The boundary.** Your plugin owns what a finding means: its rules, verdicts,
assumptions and words. KhonsuGraph owns the graph. It reads records for you, so an
analysis also works while the window has the graph open, and it checks what you say
against them. KhonsuGraph knows nothing about your labels, rules or verdicts.

List what you offer in `describe`:

```json
{"analyses": [{
  "id": "dead-ends",
  "title": "Dead ends",
  "claim": "Only says what the links in the graph say.",
  "subject_labels": ["Thing"],
  "node_labels": ["Thing"],
  "relationship_types": ["LINKS"],
  "verdicts": [
    {"id": "dead_end", "title": "Dead end", "tone": "attention"},
    {"id": "continues", "title": "Continues", "tone": "quiet"}
  ],
  "rules": [{"rule_id": "demo.dead-end", "title": "No link onward",
             "statement": "A thing with nothing linked after it.", "assumptions": []}],
  "translations": {"zh": {"verdict.dead_end": "死路"}}
}]}
```

| Field | What it does |
|---|---|
| `subject_labels` | The window offers the analysis on a node with one of these |
| `node_labels`, `relationship_types` | What KhonsuGraph reads for you. Each must be a plain name |
| `verdicts` | Every verdict you can give, most in need of attention first, which sets the order of findings. `tone` is `attention`, `undecided` or `quiet` |
| `rules` | Every rule a finding can come from |
| `translations` | By locale: `title`, `claim`, `verdict.<id>`, `rule.<id>`, `condition.<id>`. Anything missing falls back on the English |

`<plugin> analyze` then gets one line on stdin:

```json
{"protocol": 1, "analysis": "dead-ends", "complete": true,
 "nodes": [{"ref": "node:b", "labels": ["Thing"], "name": "b", "properties": {"name": "b"}}],
 "edges": [{"ref": "rel:ab", "from": "node:a", "to": "node:b", "rel_type": "LINKS", "properties": {}}]}
```

It holds no source configuration or secrets, and properties that look like secrets
are masked. `complete` is false when the graph held more than 50,000 records of one
label or type, and then a record's absence says nothing.

A record's `source` says which data source wrote it, and `coverage` says how much of
each source its last sync covered, where that is known:

```json
{"coverage": {"shop": {"run_id": "shop#3", "status": "partial", "gaps": ["page 3 timed out"]}}}
```

What an incomplete source left out could be what your rule was looking for, so don't
let it clear anything. KhonsuGraph lists the incomplete sources behind each finding
in `incomplete_sources` and marks the report incomplete whatever you decide, but the
verdict is yours.

Answer with one line per finding, then `analysis_done`:

```json
{"type": "finding", "finding_id": "finding:1", "rule_id": "demo.dead-end", "verdict": "dead_end",
 "resource": "node:c",
 "conditions": [{"id": "no-link-onward", "statement": "Nothing is linked after it.", "status": "met",
                 "detail": "c does not link onward.",
                 "fields": [{"record": "node:c", "field": "name", "source_field": "name"}]}],
 "unknowns": [], "assumptions": ["Only LINKS is read."],
 "path": {"derivation": "the link that arrives here", "hops": [{"edge": "rel:bc", "along": true}]},
 "depends_on": null, "records": []}
{"type": "analysis_done", "subjects": 3}
```

- **You point at records, and KhonsuGraph reads them.** A field's value comes from
  the graph, not from you. A relationship's ends read as the fields `@from` and `@to`.
- **A path is references.** Each hop names a relationship and whether it was crossed
  along its arrow. KhonsuGraph checks that the hops connect and that the path starts
  or ends at the resource, then rebuilds it from the graph, so it can be drawn and
  saved.
- **A bad answer is refused whole.** That means an unknown reference, rule or verdict,
  a path that doesn't connect, a repeated `finding_id`, or no `analysis_done`. Dropping
  one finding would read as that resource having none.
- **Keep `finding_id` stable:** the same rule and records should give the same ID.
  `fnv1a` in the protocol crate is there for this.
- **Say when you don't know.** A condition's `status` can be `unknown`. Work your
  verdict out from what you were sent, and don't read a missing record as a fact.

An analysis needs the same approval as a sync, checked as each run starts. It may
take 5 minutes, send lines of up to 16 MiB and send 100,000 findings. Past any of these
it is ended and its answer refused.

```sh
khonsu plugin describe my-plugin               # what it offers
khonsu plugin analyze my-plugin dead-ends --db graphs/lab --pretty
```

`--resource ID`, `--verdict ID` and `--max-findings` narrow what is printed, and
`summary` still counts everything.

## Stable IDs and merging

Every node and relationship carries an `id` that must stay the same from one
sync to the next for the same real thing: an ARN, a database key, a path
relative to the configured root. Relationship `source` and `target` are node
IDs from the same plugin.

The host stores each element under `khonsu_id = "<source id>/<your id>"`,
together with `khonsu_source`, `khonsu_plugin` and `khonsu_synced_at`, so two
sources that use the same plugin never collide. On a sync:

- a node that did not change is left alone, one that changed is updated in
  place, and a new ID adds a node;
- a property your plugin sent before and no longer sends is taken off the
  node. The host records which properties a source wrote (`khonsu_keys`), so a
  property someone added by hand stays. HawDB keeps such a property as a null,
  which KhonsuGraph doesn't list; the schema may still name it. A node last
  written before this was recorded is reconciled from its second change on;
- a relationship that did not change is left alone, and a changed one is
  replaced;
- after a **full** sync, nodes and relationships this source wrote before but
  did not report this time are removed. Incremental syncs only add and update;
- an edge whose `source` or `target` was never reported is skipped and counted
  in `skipped_edges`;
- a repeated ID within one sync counts as a duplicate, and the last one wins.

Labels and relationship types should be `UpperCamelCase` and `UPPER_SNAKE`
words. Property values can be strings, numbers, booleans, or lists and maps of
those. Leave a property out rather than sending `null`.

## Try it

```sh
khonsu plugin install examples/plugins/folders --yes   # --yes approves its permissions
khonsu plugin describe folders
khonsu init graphs/files
khonsu source add --db graphs/files --plugin folders --name repo -c root=. --test
khonsu plugin sync --source repo
khonsu query --db graphs/files --cypher \
  "MATCH (d:Folder)-[:CONTAINS]->(f:File) RETURN d.name AS folder, count(f) AS files ORDER BY files DESC LIMIT 5"
```

`install` copies the folder into KhonsuGraph's plugin directory, so edit the
source and install again to update. It also takes a `.zip` with the manifest
at its top or inside its one folder, which is how to hand a plugin to someone
else. `khonsu plugin list` shows what is
installed and whether it is approved; `khonsu plugin uninstall folders` removes
it.

For a Rust plugin, depend on `crates/khonsu-plugin-protocol`: it has the
message types, `read_request()` and an `Emitter` that writes the lines.

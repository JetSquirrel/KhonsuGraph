# Writing a data-source plugin

A plugin turns some outside system into nodes and relationships. KhonsuGraph
runs it as a separate program, talks to it in JSON lines over stdin/stdout,
and writes what it reports into the graph. A plugin never opens the graph
itself, so it can be written in any language and cannot corrupt a graph.

The two official plugins are worked examples: `plugins/csv` (local files) and
`plugins/aws` (a cloud account through the `aws` CLI). The smallest complete
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
whenever the version or the permissions change:

| Field | Meaning |
| --- | --- |
| `network` | Domains the plugin contacts, e.g. `["*.amazonaws.com"]` |
| `read_files` | Whether it reads local files the person points it at |
| `commands` | Other programs it runs, e.g. `["aws"]` |
| `notes` | Anything else worth knowing, e.g. "a read-only IAM policy is enough" |

Declare everything the plugin does. KhonsuGraph shows these to the person; it
does not sandbox the plugin.

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

## Stable IDs and merging

Every node and relationship carries an `id` that must stay the same from one
sync to the next for the same real thing: an ARN, a database key, a path
relative to the configured root. Relationship `source` and `target` are node
IDs from the same plugin.

The host stores each element under `khonsu_id = "<source id>/<your id>"`,
together with `khonsu_source`, `khonsu_plugin` and `khonsu_synced_at`, so two
sources that use the same plugin never collide. On a sync:

- a node with a known ID is updated in place; a new ID adds a node;
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
source and install again to update. `khonsu plugin list` shows what is
installed and whether it is approved; `khonsu plugin uninstall folders` removes
it.

For a Rust plugin, depend on `crates/khonsu-plugin-protocol`: it has the
message types, `read_request()` and an `Emitter` that writes the lines.

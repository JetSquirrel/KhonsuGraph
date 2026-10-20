# khonsu CLI reference

Every command prints exactly one JSON object on stdout (`--pretty` indents it). Progress of a sync goes to stderr as JSON lines (`--quiet` drops them). On failure the object is `{"error": {"kind", "message", "detail"?}}` and the exit status is 1, or 2 for a usage or spec mistake.

## Graph

```bash
khonsu init DIR                                   # a new, empty graph
khonsu schema --db DIR                            # labels, types, properties, tables
khonsu query  --db DIR --cypher "MATCH (n:Host) RETURN n LIMIT 20"
khonsu query  --db DIR --sql "SELECT * FROM assets LIMIT 20"
khonsu query  --db DIR --file query.cypher        # .sql files run as SQL; - is stdin
khonsu query  --db DIR --cypher "MATCH (n:Host) WHERE n.name = \$name RETURN n" -p name=web-1
khonsu query  --db DIR --write --cypher "CREATE (:Note {text: 'checked'})"
khonsu explain --db DIR --cypher "MATCH (n:Host) RETURN n"
```

`--db` defaults to `$KHONSU_DB`, then the graph last opened in the app. Graphs open read-only; `--write` opens for writing and takes KhonsuGraph's writer lock. `-p name=value` parameters are parsed as JSON when they parse (`-p n=3` is a number, `-p ids='["a","b"]'` a list), else text; SQL parameters are positional (`-p 1=web` for `$1`).

### query output

```json
{
  "columns": ["h", "r", "s"],
  "rows": [[{"_id": 1, "labels": ["Host"], "name": "web-1"}, {"_id": 7, "type": "RUNS", "source_id": 1, "target_id": 6}, {"_id": 6, "labels": ["Service"], "name": "storefront"}]],
  "row_count": 14,
  "truncated": false,
  "elapsed_ms": 3,
  "graph": {"nodes": [{"id": 1, "labels": ["Host"], "properties": {"name": "web-1"}}], "edges": [{"id": 7, "source": 1, "target": 6, "rel_type": "RUNS", "properties": {}}]}
}
```

`--max-rows N` (default 200) keeps the first N rows; `row_count` is what the graph returned and `truncated` says whether rows were dropped. `graph` is present when any cell holds a node or relationship. Errors carry `detail.line` and `detail.column` when HawDB names a position. `"via_window": true` means the window, which holds the graph, ran the query.

### schema output

`{node_count, relationship_count, labels: [{name, count, properties}], relationship_types: [{name, count, properties, patterns: [[source, target]]}], tables: [{name, columns}]}`. Counts are exact; properties come from HawDB's statistics.

## Data sources

```bash
khonsu plugin list                                # built-in and installed, with permissions
khonsu plugin describe csv                        # config fields, permissions, output types
khonsu plugin install ./my-plugin --yes           # copy into the user plugin folder; --yes approves its permissions
khonsu source add --db DIR --plugin csv --name "Lab" \
  -c node_file=hosts.csv -c node_label=Host -c node_id_column=id \
  -c edge_file=deps.csv -c edge_type=DEPENDS_ON -c edge_source_column=source -c edge_target_column=target --test
khonsu source list
khonsu plugin sync --source lab --dry-run         # counts and samples, writes nothing
khonsu plugin sync --source lab                   # full sync; --incremental when the plugin supports it
khonsu plugin sync --source lab --db DIR --quiet  # --db must be the source's graph; no progress lines
khonsu source remove lab
```

One CSV file of mixed labels or types needs no JSON: `-c node_label_column=kind` or `-c edge_type_column=type` take each row's label or type from that column. The CSV plugin also takes `nodes` and `edges` as JSON lists of tables, for several files; `label_column`, `type_column` and the rest go inside each table, not as `-c` fields: `-c 'nodes=[{"path":"assets.csv","label_column":"kind","id_column":"id"}]' -c 'edges=[{"path":"links.csv","type_column":"type","source_column":"source","target_column":"target","id_column":"id"}]'`. Other columns become properties; numbers and true/false are typed (`-c infer_types=false` keeps text). A sync report: `{source, mode, received_nodes, received_edges, merge: {nodes_written, edges_written, unchanged_edges, duplicate_nodes, duplicate_edges, skipped_edges, stale_nodes_removed, stale_edges_removed, warnings}, elapsed_ms}`. Every synced element carries `khonsu_id`, `khonsu_source`, `khonsu_plugin` and `khonsu_synced_at`.

The AWS plugin reads an account through the `aws` CLI with read-only calls: `-c profile=readonly -c 'regions=["us-east-1","eu-west-1"]'` (or keys stored as secrets: `access_key_id`, `secret_access_key`, `session_token`). Labels: Account, Region, Vpc, Subnet, SecurityGroup, Cidr, Instance, InternetGateway, LoadBalancer, TargetGroup, Role. Relationships: `(Cidr)-[:ALLOWS {protocol, ports}]->(SecurityGroup)`, `(SecurityGroup)-[:ALLOWS]->(SecurityGroup)`, `(Instance)-[:PROTECTED_BY]->(SecurityGroup)`, `(Instance)-[:IN_SUBNET]->(Subnet)-[:IN_VPC]->(Vpc)-[:IN_REGION]->(Region)`, `(Instance)-[:ASSUMES]->(Role)`, `(Cidr)-[:ROUTES_TO]->(LoadBalancer)-[:FORWARDS_TO]->(TargetGroup)-[:ROUTES_TO {health}]->(Instance)`, `(Account)-[:OWNS]->(...)`. The internet is the Cidr node with `cidr = '0.0.0.0/0'`. Ask the person before syncing a real account; `-c fixture_dir=...` reads saved `aws --output json` responses instead.

## Dashboards and the window

```bash
khonsu check dash.dash --db DIR [--summary]
khonsu comments dash.dash [--all]
khonsu comments dash.dash --reply c1 --text "Sorted by count" [--resolve]
khonsu comments dash.dash --add callers --text "Is this the whole chain?"
khonsu comments dash.dash --resolve c1 | --reopen c1
khonsu presence dash.dash --doing editing|reading|idle [--plot a,b] [--agent NAME]
khonsu open dash.dash | DIR [--no-launch]
khonsu open --cypher "MATCH (n) RETURN n LIMIT 50" --run
khonsu open --state [dash.dash] [--active]
```

`check` success: `{file, ok, warnings: [...], queries: [{name, language, line, rows, columns: [{name, type}], graph?: {nodes, relationships, sampled_rows?}}], filters: [{name, plot, column, match, queries}], plots: [...]}`. Comments live in `FILE.comments.json` beside the spec. `presence` and `comments` tell a running window what you are doing (`{"delivered": false}` when none runs; not an error). `open --state` reports the window's graph and its tabs: each query tab's text and result summary (columns, counts, first 20 rows, or the error), each dashboard's plots, failures and picks.

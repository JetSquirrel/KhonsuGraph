# khonsu CLI reference

Every command prints exactly one JSON object on stdout (`--pretty` indents it). Progress of a sync goes to stderr as JSON lines. On failure the object is `{"error": {"kind", "message", "detail"?}}` and the exit status is 1, or 2 for a usage or spec mistake.

## Graph

```bash
khonsu init DIR                                   # a new, empty graph
khonsu rebuild --db DIR [--quiet]                 # an old-format graph (kind old_format), again from its data sources
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

## Graph analysis

HawDB's Cypher has no shortest paths, centrality or communities; `khonsu analyze` computes them in memory, after networkx's definitions (results match networkx on the same graph).

```bash
khonsu analyze summary     --db DIR                        # size, density, components, clustering, max core, diameter
khonsu analyze degree      --db DIR --top 20
khonsu analyze pagerank    --db DIR --type TRUSTS          # --alpha 0.85, --weight PROP
khonsu analyze betweenness --db DIR --label Service        # --sample K (default: all ≤ 5,000 nodes, else 500)
khonsu analyze closeness   --db DIR --type NEXT_TO --undirected
khonsu analyze components  --db DIR [--strong]
khonsu analyze communities --db DIR                        # Louvain; --resolution 1.0, --seed 42
khonsu analyze clustering  --db DIR
khonsu analyze kcore       --db DIR
khonsu analyze path        --db DIR --from 8 --to 11       # node IDs: RETURN id(n), or _id in a result
khonsu analyze pagerank    --db DIR --cypher "MATCH (a:Process)-[r:SPAWNED]->(b:Process) RETURN a, r, b"
khonsu analyze communities --db DIR --write community      # store each node's result as a property
```

Which graph: the whole graph by default; `--label` keeps nodes with those labels and the relationships between them; `--type` keeps those relationships and (without `--label`) only the nodes they join; `--cypher` analyzes exactly the nodes and relationships a query returns, whole. Relationships are directed unless `--undirected`; clustering, cores and communities always read them both ways. `--weight PROP` reads a numeric relationship property as weight (PageRank) or length (paths, betweenness, closeness); missing is 1.

`--write PROP` stores every analyzed node's result as node property `PROP` — the community or component number, the PageRank/betweenness/closeness/clustering score, the degree or the core number — so a dashboard can `ORDER BY n.pagerank`, filter `WHERE n.community = 3`, or draw a graph plot with `group = community`. It opens the graph for writing (the window must not hold it), writes in kernel batches (thousands of nodes in well under a second), and adds `written: {property, nodes}`. A node outside the analyzed graph keeps its old value; rerun after a sync.

### analyze output

`{algorithm, graph: {nodes, relationships, directed, weighted}, load_ms, elapsed_ms, rows, ...}`. Node rows are `{id, labels, name, <score>}`, best first, `--top` of them (`name` is the first of `name`, `title`, `label`, `external_id`, `id`). `degree` adds `in`, `out`, `centrality`; `pagerank` adds `converged`; `betweenness` adds `sampled_sources` (null when exact); `clustering` rows carry `triangles` and `clustering`, plus `average_clustering`. `components` and `communities` rows are groups `{component|community, size, members}` (first 10 members), with `count`, `singletons`, and `modularity` for communities. `path` gives `found`, `hops`, `length` and the path's nodes as `rows`. `summary` gives `summary: {nodes, relationships, density, self_loops, weak_components, largest_weak_component, strong_components, isolated_nodes, average_degree, average_clustering, max_core, diameter, diameter_estimated}`.

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
khonsu source remove lab
```

The CSV plugin also takes `nodes` and `edges` as JSON lists of tables: `-c 'nodes=[{"path":"assets.csv","label_column":"kind","id_column":"id"}]' -c 'edges=[{"path":"links.csv","type_column":"type","source_column":"source","target_column":"target","id_column":"id"}]'`. Other columns become properties, or only a table's `"columns": [...]`; numbers and true/false are typed (`-c infer_types=false` keeps text), and cells in `null_values` (default `NA`, `N/A`, `NULL`, `null`, `NaN`; `-c 'null_values=[]'` keeps them) leave the property out. An `id_column`, `source_column` or `target_column` can be a list joined with `:` into a composite key, and a node table may repeat IDs, so one fact table yields its entities: `{"path":"player_play.csv","label":"Play","id_column":["game_id","play_id"],"columns":["quarter","down"]}` plus the edge `{"path":"player_play.csv","rel_type":"ON_PLAY","source_column":"nfl_id","target_column":["game_id","play_id"]}`. A sync report: `{source, mode, received_nodes, received_edges, merge: {nodes_written, edges_written, unchanged_edges, duplicate_nodes, duplicate_edges, skipped_edges, stale_nodes_removed, stale_edges_removed, warnings}, elapsed_ms}`. Every synced element carries `khonsu_id`, `khonsu_source`, `khonsu_plugin` and `khonsu_synced_at`; relationships also `khonsu_hash`.

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

`check` success: `{file, ok, queries: [{name, language, line, rows, columns: [{name, type}], graph?}], filters: [{name, plot, column, match, queries}], plots: [...]}`. Comments live in `FILE.comments.json` beside the spec. `presence` and `comments` tell a running window what you are doing (`{"delivered": false}` when none runs; not an error). `open --state` reports the window's graph and its tabs: each query tab's text and result summary (columns, counts, first 20 rows, or the error), each dashboard's plots, failures and picks.

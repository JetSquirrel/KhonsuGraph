# Neo4j plugin

Copies the nodes and relationships of a Neo4j database into a graph, so you can query,
draw and compare them here, with Neo4j switched off afterwards if you like. It only sends
read queries, in read access mode, and the server refuses a write made in that mode.

Its main use is [Cartography](https://github.com/cartography-cncf/cartography), which
pulls AWS, GCP, Azure, Kubernetes, GitHub, Okta and more into Neo4j. The plugin knows
nothing about Cartography's labels, so any Neo4j database works.

**What this was tested on:** Neo4j 5.26 Community, with a graph built to follow
Cartography's AWS model: its labels, relationship types, bookkeeping properties and way
of writing. That graph was not made by running Cartography, so expect differences on real
output.

## Try it

```sh
khonsu init graphs/infra
khonsu source add --db graphs/infra --plugin neo4j --name infra \
  -c url=http://127.0.0.1:7474 -c password=... --test
khonsu plugin sync --source infra
khonsu query --db graphs/infra --cypher \
  "MATCH (i:AWSEC2Instance {exposed_internet: true}) RETURN i.instanceid, i.publicdnsname"
```

The address is Neo4j's HTTP one, 7474 by default, not `bolt://`.

| Field | Default | Meaning |
|---|---|---|
| `url` | `http://127.0.0.1:7474` | Neo4j's HTTP address. Use `https` away from this machine: the password goes with every request |
| `database` | `neo4j` | |
| `username`, `password` | `neo4j`, none | The password is kept in the system keychain. Leave it out if the server has no authentication. A reader role is enough |
| `id_property` | `id` | What identifies a node from one sync to the next |
| `exclude_properties` | `lastupdated`, `_module_version` | Properties rewritten on every run of whatever fills the database. Kept, they would make every relationship read as changed |
| `label_priority` | none | Labels to prefer when a node has several, first one first |

Sync when Cartography isn't running. Both reads share one transaction, but Neo4j's
transactions aren't snapshots, so a write landing midway can show up in part.

## How the graph comes across

- **Each node gets one label.** HawDB has one label per node. A node takes the first of its
  labels on `label_priority`, otherwise its rarest, since a shared label such as
  `AWSPrincipal` covers at least the nodes of a narrower one such as `AWSRole`. On a tie it
  takes the one Neo4j lists first.
- **All its labels stay in `neo4j_labels`.**
- **A node's ID is `<label>:<id_property>`,** because Neo4j only keeps an ID unique within a
  label, if at all. Without that property, or when two nodes share both, it uses Neo4j's
  `elementId`, which changes if the database is rebuilt, and logs a warning.
- **If a node's label changes,** it is replaced and its relationships follow it.
- **Names with spaces or punctuation** have them replaced by `_`. `neo4j_labels` keeps the
  originals.
- **A database with no nodes changes nothing.** The run is recorded as partial, rather than
  emptying the graph because Neo4j restarted without its volume.
- **Types with no equivalent:** a date, time or duration becomes its text, and a point its
  GeoJSON text.

## Running queries written for Neo4j

Two differences give a wrong answer with no error, so check them first.

**A label that a node doesn't go by here finds nothing.**

```cypher
// In Neo4j. Here, only the nodes that took AWSPrincipal as their one label
MATCH (p:AWSPrincipal) RETURN p.id

// Here
MATCH (p) WHERE 'AWSPrincipal' IN p.neo4j_labels RETURN p.id
```

**`=~` finds a match anywhere in the text.** In Neo4j it has to match the whole text, so
`r.name =~ 'r1'` also finds `r10` here. Write `r.name =~ '^r1$'`.

The rest fail with an error you can see. These are limits of HawDB's Cypher as of 0.24:

| In Neo4j | What to do here |
|---|---|
| `(a)--(b)` | `(a)-[]-(b)` |
| `any(x IN xs WHERE x = 'y')` | `'y' IN xs` |
| `any(x IN xs WHERE x CONTAINS 'y')` | `size([x IN xs WHERE x CONTAINS 'y']) > 0` |
| `exists((a)-[:R]->())` | `MATCH (a)-[:R]->() RETURN DISTINCT a` |
| `labels(n)` | `n.neo4j_labels` |
| `shortestPath` with an end not yet bound | Bind both ends first, in `MATCH` clauses with inline properties |
| `UNION`, `UNION ALL` | Run the parts one at a time |
| `toLower(x) = 'y'` | `x =~ '(?i)^y$'`. `=~` doesn't work inside a list expression, so there spell out the cases: `x CONTAINS 'create' OR x CONTAINS 'Create'` |
| `keys(n)`, `properties(n)` | No equivalent |

We ran 55 queries on both engines and compared the rows. 34 matched as written, and 15
matched once rewritten as above, 4 of those having first given wrong rows with no error. The
other 6 stayed apart: 3 used `UNION`, 1 used `keys()`, and 2 used `toLower` with a rewrite
that didn't hold, which the forms above replace. We wrote these queries for the comparison,
several from Cartography's samples, so they show what to look out for and don't measure a
real workload.

`cartography-rules` connects to Neo4j itself, so it doesn't run against the copy.

## How long it takes

Measured on one Windows desktop with Neo4j in Docker on the same machine, against the graph
described above at 157,003 nodes and 247,502 relationships:

| Step | Time | Graph on disk |
|---|---|---|
| First sync | 132 s | 919 MB |
| Sync with nothing changed | 32 s | 919 MB |
| Sync with one node changed | 32–35 s | 919 MB |
| Sync after deleting one node | 35 s | 919 MB |
| A three-hop query across the graph | 0.3 s | |

After each step the node and relationship counts matched Neo4j. Your graph's shape will
change these numbers, and we haven't measured anything larger.

## Limits

- **No incremental sync.** Every sync reads the whole database, so a sync takes about the
  same time however little changed. Only what changed is written.
- **Nothing is sandboxed.** It's built in and needs no approval, but like every plugin it
  runs as you. See [plugin security](../../docs/plugin-security.md).
- **Tested only as above.** Neo4j Aura, clusters and older Neo4j versions haven't been tried.

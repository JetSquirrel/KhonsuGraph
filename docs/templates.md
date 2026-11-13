# Investigation templates

A template is a question asked the same way each time: what it needs from a
graph, the parameters it takes, and a list of bounded steps. Running one gives
findings that each point at the records behind them, with the caps that applied
and anything that stopped the answer from being whole.

```sh
khonsu template list
khonsu template check examples/sre/blast-radius.template.json --db graphs/infra
khonsu template run examples/sre/blast-radius.template.json --db graphs/infra \
  -p service=infra/svc-db --report db.report.json
```

Two templates ship, and both run offline on the example data:

| Template | File | Asks |
|---|---|---|
| `blast-radius` | `examples/sre/blast-radius.template.json` | Which services depend on a service, what runs it, and which alerts fired on it. Runs on the CSV demo in `examples/csv` |
| `aws.exposure` | `plugins/aws/templates/exposure.template.json` | What the AWS plugin's exposure rules say about each instance, and which security group rules allow the whole internet. Runs on `plugins/aws/tests/fixture` |

`template run` and `template check` take a file, or an ID that `template list`
shows. Templates are looked for in `templates/` in your settings folder, and in
each plugin's `templates/` folder; `--dir` adds a folder. A plugin's templates
are files that are read, never programs that are run.

## What a run may do

These hold whatever a template file says:

- **It only reads.** Queries run in read transactions, so a statement that
  writes is refused by the engine (kind `read_only`), not by anything that reads
  its text. Nothing in a template can change a graph or a source. To change
  either, a person runs `khonsu query --write` or `khonsu plugin sync` themselves.
- **Every step has a cap.** A walk is bounded as `khonsu explore` is (16 hops,
  2,000 nodes, 5,000 relationships at most), a query by `max_rows` (5,000 at
  most), an analysis by the plugin time limit, and the run by `--max-findings`.
  When a cap is reached the step says so in `limits`, and the run's `complete`
  is false.
- **It stops on request.** `--timeout` (300 seconds by default) stops further
  steps from starting. A query already running finishes first, since HawDB
  cannot cancel one.
- **The wrong graph is refused.** A graph without the labels or relationship
  types under `requires` fails with kind `unsupported_source` before any step
  runs. An empty answer from the wrong graph would read as "nothing found".
- **A plugin must be approved.** An analysis step runs a plugin only when it is
  approved as it is now, as everywhere else.

## Reading a run

Read `complete` first. When it is false, `gaps` says why in words, and a finding
that isn't there means nothing.

| Field | Meaning |
|---|---|
| `complete`, `gaps` | False when a step failed or was cancelled, a cap was reached, findings were cut, or a source's last sync wasn't complete |
| `failed` | Steps that could not run. The command exits 1 when there are any |
| `steps` | Each step's `status` (`ok`, `failed`, `cancelled`), `caps`, `limits`, `findings`, `error` and `warnings` |
| `fingerprint`, `sources` | What the graph held, and how much of each source its last sync covered |
| `parameters` | The parameters as used, with defaults filled in and secrets masked |
| `findings` | See below. `truncated` counts those `--max-findings` left out |
| `evidence` | Every record a finding rests on, as `khonsu evidence` gives it |

A finding:

| Field | Meaning |
|---|---|
| `finding_id` | The same template, parameters and records give the same ID |
| `step`, `rule_id`, `verdict` | Where it came from and what it is filed under. `verdict` is unknown when the step names none |
| `class` | `observation`: a row of what the graph holds. `derived`: worked out by a walk or a rule |
| `resource`, `resource_name` | The evidence ID of what it is about, and what people call it. Unknown when a row names none |
| `summary` | The finding in a line |
| `values` | A query row by column, with secrets masked and whole records named by their evidence IDs |
| `path` | For a walk or a rule: `hops` in words, `supporting` evidence IDs, `confidence`, and `refs`, which `khonsu investigation save --path-refs` takes |
| `conditions`, `unknowns`, `assumptions` | A plugin's conditions as the host checked them, what could not be read, and what is assumed |
| `evidence_ids` | Every record it rests on. Each is in the run's `evidence` |

Two runs of the same template, with the same parameters, on graphs holding the
same records give the same findings in the same order. Finding IDs come from
stable IDs, so this holds across two graphs built from the same source, and
after a rebuild. Timings (`started_at`, `elapsed_ms`) are the only fields that
differ.

A path is a connection in the data. It is not proof that anything can reach
anything, and no path is not proof of the opposite. See
[the gates](investigation-gates.md#known-unsupported-semantics).

## Writing a template

```json
{
  "format": "khonsu-template",
  "version": 1,
  "id": "blast-radius",
  "revision": "1",
  "name": "Blast radius of a service",
  "question": "If this service fails, which services depend on it?",
  "claim": "What the graph records as depending on the service. Not a measurement of impact.",
  "requires": { "node_labels": ["Service"], "relationship_types": ["DEPENDS_ON"] },
  "parameters": [
    { "name": "service", "kind": "node", "description": "The service that fails: its khonsu_id." },
    { "name": "hops", "kind": "integer", "default": 4 }
  ],
  "steps": [
    { "id": "dependents", "kind": "walk", "from": ["$service"], "direction": "in",
      "types": ["DEPENDS_ON"], "max_hops": "$hops", "labels": ["Service"], "verdict": "depends_on_it" }
  ]
}
```

`question` and `claim` are required: a template has to say what it asks and what
its findings do and don't show. Change `revision` when what it asks changes; a
run records the template's digest, so two different templates under one revision
still tell apart.

A parameter's `kind` is `node` (a `khonsu_id`, which must be in the graph),
`string`, `integer` or `boolean`. Without a `default` it is required.

| Step `kind` | Fields | Gives |
|---|---|---|
| `walk` | `from` (node parameters), `direction` (`out`, `in`, `both`), `types`, `max_hops`, `max_nodes`, `max_edges`, `labels` (only nodes with one of these are findings) | One finding for each node reached, nearest first, with the fewest-hops path the walk took to it |
| `query` | `cypher`, `max_rows`, `resource_column` (the column holding the node, or `khonsu_id`, a row is about) | One finding for each row. Parameters are bound as `$name`; a node parameter binds its `khonsu_id` |
| `analysis` | `plugin`, `analysis`, `resource` (a node parameter), `only_verdict` | The plugin's findings, with its rules, verdicts, conditions and unknowns, checked by the host |

Every step also takes `title`, `assumptions`, and (except `analysis`, which
keeps the plugin's) `rule_id` and `verdict`. A cap is a number or `$name` of an
integer parameter.

Return whole nodes and relationships from a query (`RETURN h AS host, r AS
runs`) when a finding should cite them: each becomes evidence. A value that is
not a record is kept as it is, masked when its column name or its shape is a
secret's.

`khonsu template check FILE` says what is wrong with a file, and with `--db`
whether a graph holds what it needs and whether its plugins may run.

## What is not there

- No loops, conditions or steps that read another step's findings. A template is
  a list.
- No SQL steps, and no `khonsu analyze` algorithms as steps.
- No window for templates: they run from the command line, and a finding's path
  opens in the window through [a report](reports.md).
- KhonsuGraph runs no model and calls no AI service. An agent runs these
  commands as a person would; which model it is, and how it is configured, is
  the agent's business.

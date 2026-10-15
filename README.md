# KhonsuGraph

A local-first desktop workbench for [HawDB](https://hawdb.ai) graphs: open a
graph, query it in Cypher or PostgreSQL SQL, see nodes and relationships on a
canvas, bring data in through plugins, and build `.dash` dashboards an AI
agent can write and a person can review. Built on HawDB; not an official
HawDB product.

## Layout

| Path | What |
| --- | --- |
| `app/` | The desktop app: GPUI with gpui-kit (DuckLocal's shell) and [gpui-graph](https://github.com/JetSquirrel/gpui-graph) for the canvas |
| `crates/khonsu-core` | HawDB access, writer lock, results, schema, plugin host, merge writer, settings |
| `crates/khonsu-dash` | `.dash` dashboards: parse, validate, filters, `check`, comments, presence, the window link |
| `crates/khonsu-cli` | `khonsu`, the windowless JSON CLI for agents and scripts |
| `crates/khonsu-plugin-protocol` | The data-source plugin protocol (JSON lines over stdio) |
| `plugins/csv` | The built-in CSV plugin |
| `plugins/aws` | The official AWS plugin: VPCs, subnets, security groups, instances, load balancers and IAM roles, read through the `aws` CLI |
| `skills/khonsugraph` | The agent skill: how to query, author dashboards and review them with a person |
| `examples/` | Five user-story dashboards on public datasets, and the scripts that build their graphs |

## Build

```sh
cargo build --workspace            # app, CLI, CSV and AWS plugins
cargo test --workspace
```

The app expects gpui-graph checked out beside this repository
(`../gpui-graph`). On Windows, HawDB 0.8 cannot create a graph under `C:\`
(it flushes every parent directory up to the drive root); tests use
`scratch/tests`, set in `.cargo/config.toml`.

## HawDB 0.8 notes

What KhonsuGraph works around, found while building it:

- One process per graph directory, readers included. While the app has a
  graph open, `khonsu query` and `khonsu check` run through the app.
- Cypher has no `sum()`, `OPTIONAL MATCH`, path variables, `labels()` or
  `keys()`; variable-length patterns need a type and no variable;
  `WHERE true` does not parse, so dashboard filters remove an unpicked
  condition instead.
- `SET` on a relationship property does nothing; a changed relationship is
  deleted and created again.
- Creating a relationship costs time proportional to the relationships of its
  type already stored, so large imports are slow (35k ratings in one type
  take about 13 minutes). Nodes go in kernel batches and are fast.

## License

Apache-2.0.

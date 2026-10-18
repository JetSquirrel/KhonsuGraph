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
| `crates/khonsu-plugin-protocol` | The data-source plugin protocol (JSON lines over stdio); see [docs/plugins.md](docs/plugins.md) to write a plugin in any language |
| `plugins/csv` | The built-in CSV plugin |
| `plugins/aws` | The official AWS plugin: VPCs, subnets, security groups, instances, load balancers and IAM roles, read through the `aws` CLI |
| `skills/khonsugraph` | The agent skill: how to query, author dashboards and review them with a person |
| `examples/` | Five user-story dashboards on public datasets, the scripts that build their graphs, an AWS exposure dashboard and a Python plugin |

## Build

```sh
cargo build --workspace            # app, CLI, CSV and AWS plugins
cargo test --workspace
```

On Windows, HawDB 0.8 could not create a graph under `C:\` (it flushed
every parent directory up to the drive root; not yet checked on 0.10); tests
use `scratch/tests`, set in `.cargo/config.toml`.

## Release builds

| Script | Makes |
| --- | --- |
| `scripts/bundle.sh` | An unsigned `KhonsuGraph.app` to try on a Mac |
| `scripts/package-macos.sh` | The signed, notarized `.dmg` (Developer ID and an App Store Connect key required) |
| `scripts/package-windows.ps1` | A zip of the Windows build (not code-signed) |

Every package holds the app, the `khonsu` CLI and the official plugins under
`plugins/<id>/` beside them, which is where both look for built-in plugins.
The robot button in the title bar (Command line & AI) puts `khonsu` on your
`PATH` and adds the agent skill to Claude Code.
Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds both and
publishes a GitHub release.

## HawDB 0.10 notes

What KhonsuGraph works around, found while building it:

- One process per graph directory, readers included. While the app has a
  graph open, `khonsu query` and `khonsu check` run through the app.
- Cypher has no `sum()`, path variables, `shortestPath`, `labels()` or
  `keys()`; variable-length patterns need a type and no variable;
  `WHERE true` does not parse, so dashboard filters remove an unpicked
  condition instead. `CREATE` takes one pattern per statement.
- A changed relationship is deleted and created again, in one batch per
  pattern, rather than matched one by one to `SET`.

Fixed since 0.8: `OPTIONAL MATCH` works, `SET` on a relationship property
sticks, and creating relationships no longer slows with the number already
stored (the audit story's 35k ratings import in about 2 seconds, down from
13 minutes). Graphs written by 0.8 open as they are.

## License

Apache-2.0.

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
| `crates/khonsu-cli` | `khonsu`, the windowless JSON CLI for agents and scripts; the app runs it when called as `khonsu` or with a subcommand |
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

Tests use `scratch/tests`, set in `.cargo/config.toml`,
to avoid a Windows drive-root flush issue observed with HawDB 0.8. The
upgrade validation is performed on macOS; Windows still needs verification.

## Release builds

| Script | Makes |
| --- | --- |
| `scripts/bundle.sh` | An unsigned `KhonsuGraph.app` to try on a Mac |
| `scripts/package-macos.sh` | The signed, notarized `.dmg` (Developer ID and an App Store Connect key required) |
| `scripts/package-windows.ps1` | A zip of the Windows build (not code-signed) |

One executable is both the app and the `khonsu` CLI: with no arguments it
opens the window; called as `khonsu`, or with a subcommand or a flag
(`khonsugraph query …`, `khonsugraph --help`), it runs the command and exits.
So `cargo run -p khonsugraph -- query …` works from a checkout. Every package
holds it and the official plugins under `plugins/<id>/` beside it, which is
where it looks for built-in plugins; the Windows zip adds a `khonsu.exe` copy,
since Windows finds a command by file name. The robot button in the title bar
(Command line & AI) puts `khonsu` on your `PATH` and adds the agent skill to
Claude Code.
Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds the
packages and publishes a GitHub release.

## HawDB 0.12.0 notes

KhonsuGraph pins HawDB to 0.12.0. See [upgrade validation](docs/hawdb-0.12-upgrade.md)
for compatibility checks and their scope, and the [0.11 validation](docs/hawdb-0.11-upgrade.md)
for earlier database compatibility checks.

- One process per graph directory, readers included. While the app has a
  graph open, `khonsu query` and `khonsu check` run through the app.
- `OPTIONAL MATCH`, scalar `IN` expressions and `json_value()` on JSON text
  are supported. Relationship properties can now be updated with `SET`;
  the importer retains its existing delete/recreate strategy.
- Cypher `sum()`, `labels()` and `WHERE true` still fail to parse in the
  checked queries. Dashboard filters remove an unpicked condition instead.
- KhonsuGraph explicitly limits reads to 100,000 result rows, preserving
  its previous behavior after HawDB removed the default row limit in 0.10.
  The result display limit is separate and does not lower execution cost.
- Vector values returned by SQL and Cypher are exposed as JSON number arrays,
  including vector properties inside graph elements.
- The earlier import measurement (35k relationships in about 13 minutes)
  was made on 0.8; it is not a performance estimate for 0.12.0.

## License

Apache-2.0.

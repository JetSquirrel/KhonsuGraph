# HawDB 0.22.0 upgrade

Validated on Windows 11 on 2026-10-31.

## Changes

- Pin HawDB to 0.22.0 (from 0.19.0). The public API KhonsuGraph uses is
  unchanged; `HAWDB_VERSION` follows the pin.
- **Read-only opens ask for strict WAL replay.** Since 0.20 HawDB repairs a
  torn WAL tail on open by default (`RecoveryMode::AutoRepairTornTail`), which
  needs a writer: every read-only open failed with `automatic WAL tail repair
  requires a writable database open`, including `khonsu query` on a graph no
  one else has open. `Graph::open` now passes `RecoveryMode::Strict` for
  `AccessMode::ReadOnly`, as HawDB's own CLI does. Writable opens keep the
  default.
- The skill, the agent setup text and the README describe the Cypher that now
  works.

## Graphs written by earlier versions

Graphs written by 0.19 open in 0.22 without a rebuild. A copy of the fly
medulla connectome graph (1,781 nodes, 9,735 relationships, synced by the 0.19
CLI) read back the same counts and schema, accepted a sync and a `SET`, and
read back again after reopening. 0.8–0.15 graphs still report `old_format`.

## Validation

The workspace suite passed 204 tests; the normally ignored integration tests
passed, including the CSV source add-and-sync CLI test.

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored
```

## Cypher

Checked through `khonsu query` on 0.22.0:

| Works | Still fails |
|---|---|
| `sum()`, also over expressions (`sum(r.weight * 2)`) | `labels()`, `keys()`, `properties()` |
| `collect`, `size`, `head`, `tail`, `last`, `reverse`, `range` | `reduce`, `any`, `all`, `toString` |
| indexing `xs[0]`, `xs[-1]`, slices `xs[1..3]` | comprehension projections with arithmetic (`[x IN xs \| x * 10]`) |
| `[x IN xs WHERE x > 5]`, `[m IN ns \| m.name]` | `id(a) < id(b)` (a function call on both sides) |
| `IN [...]`, `CASE WHEN`, `coalesce` | string concatenation (`a.name + '!'`) |
| columns in `RETURN` order | path variables, `shortestPath`, `WHERE true` |
| | a read that does not start with `MATCH` (`RETURN 1`) |
| | variable-length patterns with a variable or without a type |
| | `OPTIONAL MATCH` as the first clause, `UNWIND` without a write |

Variable-length patterns now match trails (no relationship twice, 0.21). The
only shipped query using one, `examples/sre/blast_radius.dash`, returns
endpoints, which trails do not change.

HawDB 0.20 says it preserves Cypher column order (#1323). KhonsuGraph's own
reordering (`cypher_columns.rs`) still runs and is harmless; whether it can
go was not checked against raw HawDB output.

Creating a graph under `C:\` still fails with `os error 5`: 0.21's Windows
parent-directory fix is in the Python bindings only.

Interactive desktop-window behavior and macOS were not verified.

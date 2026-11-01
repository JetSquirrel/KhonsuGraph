# HawDB 0.24.0 upgrade

Validated on Windows 11 on 2026-11-02.

## Changes

- Pin HawDB to 0.24.0 (from 0.22.0); `HAWDB_VERSION` follows the pin.
- **Temporal values become ISO 8601 strings in JSON.** 0.24 adds `Value::Date`,
  `LocalTime`, `LocalDateTime`, `DateTime` and `Duration`. `value_to_json` writes
  them as HawDB prints them (`2026-10-30`, `2026-10-30T12:34:56+01:00`,
  `P1Y2M3DT5H`). The schema command and `khonsu check` see them as text.
- **`LOAD CSV` stays off.** It is new in 0.24 and only runs when the host sets
  `csv_import_root`. KhonsuGraph does not set it, so read-only queries and
  dashboards cannot read files from disk. A test holds this in place.
- The skill, the agent setup text, the README and the dashboard filter wording
  describe the Cypher that now works. Unpicked dashboard filters are still
  removed from the query, because HawDB's SQL still refuses `WHERE true`.

## Graphs written by earlier versions

| Written by | Result on 0.24 |
|---|---|
| 0.22: fly medulla copy (1,782 nodes, 9,735 relationships, last written by 0.22) | Same counts and schema; accepted a `SET` and read back |
| 0.22: CSV source (12 nodes, 14 relationships) | Same counts; a re-sync left the counts unchanged and rewrote no relationships |
| 0.8–0.15 | Still report `old_format` |

Going back to 0.22 depends on the data. A plain graph last written by 0.24
opened in 0.22, but a graph with temporal properties failed there with `WAL
corruption at byte offset 28: WAL value message is empty`. The graph was not
damaged: 0.24 read it normally afterwards. This was seen on one small graph, and
only a read-only open was tried.

## Validation

The workspace suite passed 208 tests; the normally ignored integration tests
passed, including the CSV source add-and-sync tests.

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored
```

Five shipped dashboards were checked on 0.24 against graphs synced from their
data sources into new directories. The `infra` and `aws` examples were not
checked, because no synced graph for them was at hand.

| Dashboard | Nodes | Relationships | Sync (debug build) | `khonsu check` |
|---|---:|---:|---:|---|
| `audit/trust_network` | 5,881 | 35,592 | 4.4 s | ok |
| `investigation/offshore_network` | 5,577 | 6,515 | 1.3 s | ok |
| `renting/near_mtr` | 5,730 | 17,119 | 2.1 s | ok |
| `security/attack_chain` | 1,838 | 20,591 | 4.3 s | ok |
| `sre/blast_radius` | 12 | 16 | 0.1 s | ok |

The sync times were not compared with 0.22 on the same machine, so they say
nothing about what the upgrade changed.

## Cypher

Checked through `khonsu query` on 0.24.0:

| Now works | Still fails |
|---|---|
| `shortestPath`, `allShortestPaths`, `length(p)`, `nodes(p)`, `relationships(p)` with bound endpoints | `shortestPath` with an unbound endpoint |
| `date`, `datetime`, `duration`, comparison, `+` and `.year`; stored as properties | `p = (a)-[:T]->(b)` (a path variable without `shortestPath`) |
| `toString`, `toInteger`, `toFloat`, `toBoolean` | `labels()`, `keys()`, `properties()`, `toLower`, `toUpper`, `exists` |
| `[x IN xs \| x * 10]` | `reduce`, `any`, `all`, `none` |
| `UNWIND` as a read (`UNWIND [1,2] AS x RETURN x`) | `id(a) < id(b)`, string concatenation |
| `WHERE true` in Cypher (not in SQL) | `RETURN 1`, `OPTIONAL MATCH` as the first clause |
| comparison chains (`1 <= a.cpu <= 4`) | variable-length patterns with a variable or without a type |
| `ORDER BY` over an expression | `CREATE (a:N), (b:N)` |

`RETURN p` nests the path's nodes and relationships in a cell, and
`GraphCollector` already picks those up, so `result.graph` holds them with no
new code. A test covers this.

Creating a graph under `C:\` still fails with `os error 5`.

The desktop window, including how a path or a date looks in it, and macOS were
not verified.

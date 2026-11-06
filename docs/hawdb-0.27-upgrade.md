# HawDB 0.27.0 upgrade

Validated on macOS 26.6 (Apple M5) on 2026-11-06.

## Changes

- Pin HawDB to 0.27.0 (from 0.24.0); `HAWDB_VERSION` follows the pin.
- **No graph written before 0.25 opens.** 0.25 changed the relational durable
  codec from version 2 to 3 (in the Decimal and keyless-table changes,
  hawdb#1429 and #1474) and reads no version 2 graph, which every KhonsuGraph
  build up to v0.2.0 wrote; HawDB offers no migration and its release notes do
  not mention it. Such a graph reports `old_format`, now worded "before 0.25",
  and `khonsu rebuild` makes it again from its data sources. With no users
  yet, KhonsuGraph does not carry a migration. One was built and checked
  (HawDB 0.24 linked beside 0.27, every record copied and compared; the
  `audit` story and two real graphs came across with no difference), then left
  out: it cost 19 MB in the executable for graphs nobody has.
- **SQL decimals.** 0.25 made SQL numeric literals, `DECIMAL` columns and their
  `sum` exact decimals. `value_to_json` writes one as a JSON number when a
  double holds it exactly (`19.99`, `25.00`), and as its exact text otherwise,
  so dashboards still chart them and no digit is lost without a word.
- **SQL error positions.** 0.26 parses SQL with a PEG parser, whose syntax
  errors say `at bytes A..B` instead of a line and column. `Graph::run` works
  the line and column out from that span in the text as written.
- Cypher did not change for what KhonsuGraph uses: 29 statements, from the
  0.24 table below and more, succeed or fail on 0.27 exactly as on 0.24, with
  the same rows.

## Validation

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored
```

`graph-hawdb-0.27.0.zip` replaces the 0.24 fixture, which 0.27 cannot read,
so the next upgrade is tested against this version.
`examples/aws/golden.json` did not change: the offline investigation gives
the same answers on 0.27.

## Performance

The same release build of each version, on the same machine, five runs each
(median); every graph built by the version under test. Answers were compared
and are the same.

| | 0.24 | 0.27 | |
|---|---:|---:|---|
| `WHERE r.rating = -10` on 35,592 relationships | 2,750 ms | 74 ms | 37× faster (hawdb#1547) |
| `audit/trust_network` dashboard, all queries | 134 ms | 92 ms | −31% |
| CSV import of the audit story | 2,084 ms | 2,248 ms | within noise |
| Unchanged re-sync, point lookup, two hops, aggregates | | | within ±5% |
| Loading the graph for `analyze` | 550 ms | 550 ms | the same, run interleaved |

## Seen, not changed

- An unconstrained `NUMERIC` column stores `0.1` as `0`: HawDB gives it scale
  0, where PostgreSQL keeps the value's own.
- SQL still refuses `WHERE true`, so dashboard filters still take an unpicked
  condition out of the query.

Windows was not verified.

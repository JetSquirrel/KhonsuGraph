# HawDB 0.11.0 upgrade validation

Validated on macOS on 2026-10-20, upgrading the pinned engine from 0.8.0
to [0.11.0](https://github.com/nowledge-co/hawdb/releases/tag/v0.11.0).

## Application changes

- Updated the workspace dependency, lockfile and displayed engine version.
- Explicitly configured `max_read_result_rows: Some(100_000)`. HawDB 0.10
  removed the default row limit; KhonsuGraph retains its existing ceiling.
  `QueryOptions::max_rows` only truncates the displayed result after execution.
- Added regression tests for scalar `IN` with a list parameter, typed
  `json_value()` results after reopening, and a 100,001-row query failing
  with `read_budget` even when the display limit is two rows.
- Updated the README and agent skill for supported `OPTIONAL MATCH` and
  relationship-property `SET`. The importer keeps its existing strategy.

## Existing database compatibility

The original audit graph was not modified. A copy of its 0.8 database
(6,192 nodes, 35,899 relationships) was queried with both the saved 0.8 CLI
and the newly built 0.11 CLI, each using an isolated settings directory.

| Check | Result |
| --- | --- |
| Schema, counts, properties and relationship patterns | Identical |
| Account count | 5,882 in both versions |
| Flagged account query | Same 60 rows |
| Flagged-account trust subgraph | Same 125 rows, nodes and edges |
| Monthly negative ratings | Same 58 rows in the same order |
| `check examples/audit/trust_network.dash` | Identical validation output |
| Write a marker into the copy, then reopen in a fresh process | Marker persisted; Account count remained 5,882 |
| SQL `SELECT version()` | Reports HawDB 0.11.0 |

Comparisons exclude elapsed time. Unordered query results are compared as
multisets; monthly results are additionally compared in order.

## Automated validation

```sh
cargo test --offline --locked --workspace
cargo test --offline --locked --workspace -- --ignored
cargo build --offline --locked --workspace
```

The regular suite passed 184 tests. Both normally ignored CSV integration
tests also passed, covering CLI source setup/import and core sync updates,
stale-record removal and idempotence. App tests account for 121 of the regular
tests; AWS plugin tests use saved fixtures rather than a live AWS account.

In this checkout the `target` symlink points to an unavailable directory.
Validation used `CARGO_TARGET_DIR=/private/tmp/khonsugraph-review-target`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and
`CARGO_INCREMENTAL=0`. `CARGO_NET_OFFLINE=true` also applied to nested plugin
builds in the ignored tests.

## Confirmed behavior and limits

- `OPTIONAL MATCH` returns matching peers and a null peer when none exist.
- `SET r.value = 2` persists a relationship-property update after reopening.
- The checked Cypher `sum()`, `labels()` and `WHERE true` queries still fail
  to parse; standalone read-only `UNWIND` is rejected.
- A SQL self-cross-join over 320 rows without `LIMIT` reports
  `overlay contains 13 entries, exceeding limit 12` in both 0.8 and 0.11.
  The same query with `LIMIT 5` succeeds in both. This is an existing engine
  limitation, not an upgrade regression.
- Windows and live desktop-window interaction were not verified in this
  upgrade. Compilation and app unit tests do not establish visual acceptance.
- This validates forward compatibility for the copied audit database;
  reopening a database written by 0.11 in 0.8 was not tested.

# HawDB 0.15.0 upgrade

Validated on Windows 11 on 2026-10-26.

## Changes

- Pin HawDB to 0.15.0 (from 0.12.0) and update the engine version shown in
  About, crash reports and the `old_format` message. No code changes were
  needed: the public API KhonsuGraph uses is unchanged.

## Validation

The workspace suite passed 177 tests, and both normally ignored CSV
integration tests passed. The complete workspace builds without warnings.

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored
```

## Graphs written by earlier versions

- **0.12 graphs open as they are.** A graph created by KhonsuGraph on 0.12
  (nodes, a relationship with a property, an index) was read and written by
  0.15 without a rebuild. 0.12 could still read it after 0.15 had written to
  it, so a downgrade within 0.12–0.15 also works.
- **0.8–0.11 graphs** are still rejected (`HAWDB_BRANCH_PROJECT_V1` header)
  and still report `old_format`; `khonsu rebuild` applies as described in
  the [0.12 upgrade](hawdb-0.12-upgrade.md).

## Cypher

These gaps were checked again and are unchanged in 0.15: `sum()`,
`labels()`, `keys()`, `WHERE true` and path variables (`p = (...)`) still
fail to parse or bind.

Interactive desktop-window behavior and macOS were not verified.

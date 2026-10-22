# HawDB 0.12.0 and gpui-kit 0.8.0 upgrade

Validated on macOS on 2026-10-22.

## Changes

- Pin HawDB to 0.12.0 and update the engine version shown in About and crash reports.
- Upgrade gpui-kit to 0.8.0, preserving SQL syntax highlighting and test support.
- Upgrade gpui-graph to 0.1.2, which uses gpui-base 0.8.0. Both UI libraries
  share gpui-pre 0.3.9 and the same base theme state.
- Convert HawDB's new `Value::Vector` to a JSON number array. JSON array
  parameters retain their existing list semantics.

## Validation

The workspace suite passed 189 tests, including SQL and Cypher vector results
after reopening a database, vectors nested in graph properties, and graph
colors following both light and dark component themes. Existing persistence,
query-budget, CLI, dashboard, and plugin tests also passed.

Both normally ignored CSV integration tests passed, covering CLI setup/import
and core sync updates, stale-record removal, and idempotence.
The complete workspace also builds successfully.

```sh
cargo test --offline --locked --workspace
cargo test --offline --locked --workspace -- --ignored
cargo build --offline --locked --workspace
```

Validation used `CARGO_TARGET_DIR=/private/tmp/khonsugraph-review-target`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, and
`CARGO_INCREMENTAL=0`. `CARGO_NET_OFFLINE=true` also applied to nested plugin
builds in the ignored tests.

gpui-graph 0.1.2 separately passed its 15 tests, workspace Clippy with warnings
denied, format checks, and package creation before release.

Windows and interactive desktop-window behavior were not verified. The
[0.11 upgrade report](hawdb-0.11-upgrade.md) records earlier checks against a
copy of the audit database; that dataset comparison was not repeated here.

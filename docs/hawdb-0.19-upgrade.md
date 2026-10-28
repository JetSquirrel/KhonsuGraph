# HawDB 0.19.0 and gpui-kit 0.8.1 upgrade

Validated on Windows 11 on 2026-10-29.

## Changes

- Pin HawDB to 0.19.0 (from 0.15.0). The public API KhonsuGraph uses is
  unchanged; the engine version in About, crash reports and the
  `old_format` message follows the pin.
- Upgrade gpui-kit to 0.8.1 and gpui-graph to 0.1.6. Both use gpui-base
  0.8.1 and gpui-pre 0.3.10.
- gpui-pre 0.3.10 resolves `secondary-` (⌘ on macOS, Ctrl elsewhere) only
  when it knows the host OS. `KeyBinding::new` with `secondary-` now panics,
  so those bindings go through `KeyBinding::load` with the app context, and
  the toolbar's ⌘↵ hint uses `Keystroke::parse_with_os`.
- `ScrollWheelEvent` and `on_scroll_wheel` are renamed to `ScrollEvent` and
  `on_scroll` upstream.

## Graphs written by earlier versions

**Graphs written by 0.12–0.15 (the first KhonsuGraph v0.1.0 build) cannot be read.** HawDB
0.19 writes relational storage with codec version 2 and refuses version 1:
`unsupported relational durable codec version 1; this reader supports
version 2`. There is no migration. The graph still opens; the refusal comes
with the first statement, so opening a graph now runs `SELECT 1` and maps
`HawDBError::UnsupportedStorageFormat` to `old_format`, the same kind as
0.8–0.11 graphs. `khonsu rebuild` accepts both.

Opening a 0.15 graph with 0.19, including a refused write, left it readable
by 0.15. A graph made and synced by the 0.1.0 CLI (CSV source, 12 nodes,
14 relationships) was rebuilt by 0.19 with the same counts and schema, and
the old directory was kept as `graph.hawdb-old`.

## Validation

The workspace suite passed 189 tests; both normally ignored CSV integration
tests passed. The app started on Windows and bound its keys without a panic.

```sh
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored
```

## Cypher

Unchanged in 0.19: `sum()`, `labels()`, `keys()`, `WHERE true` and path
variables still fail to parse or bind, and `OPTIONAL MATCH` cannot start a
query. `OPTIONAL MATCH` after `MATCH`, scalar `IN` and relationship `SET`
work. Creating a graph under `C:\` still fails with `os error 5`.

Interactive desktop-window behavior beyond startup and macOS were not
verified.

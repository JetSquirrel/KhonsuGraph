# HawDB 0.39.0 upgrade

Validated on Windows Server 2025 on 2026-11-25.

## Changes

- Pin HawDB to 0.39.0 (from 0.34.0); `HAWDB_VERSION` follows the pin.
- **Graphs written by 0.34 open unchanged.** The 0.34 fixture gives the same
  answers, schema, exposure and investigation fingerprint on 0.39, and takes
  a write; no rebuild is needed.
- **Going back to a 0.34 build** keeps working for graphs 0.34 created: after
  0.39 read one, and after it wrote to one, the 0.34 build still opened it and
  saw the new data. A graph 0.39 *created* does not open on 0.34, which
  reports it as written "by an older HawDB (before 0.31)": the wording is
  wrong, since the graph is newer, and nothing distinguishes the two cases.
- HawDB's temporal values moved from chrono to the `time` crate
  (`hawdb::temporal::{Date, Time, PlainDateTime, OffsetDateTime,
  SignedDuration}`), and `temporal::Duration::new` now returns `Option`.
  KhonsuGraph only formats these values through `Display`, so just the test
  that builds them changed; it now uses HawDB's `parse_*` functions.
- Shipped together with gpui-kit 0.11 and gpui-graph 0.3.0, which do not touch
  the database.

## Validation

```sh
cargo build --workspace --all-targets --locked
cargo test --workspace --locked --no-fail-fast
```

Every test passes on 0.39. `examples/aws/golden.json` did not change: the
offline investigation gives the same answers.

`graph-hawdb-0.39.0.zip` joins the 0.34 fixture rather than replacing it,
because 0.39 still reads 0.34 graphs. Both run through
`graphs_written_by_earlier_hawdb_versions_keep_their_meaning`.

The app was also run on Windows against the 0.39 fixture: the graph view,
the `examples/aws/exposure.dash` dashboard, and the Cypher and SQL editors
all work. Next to a `main` build (gpui-kit 0.9) the dashboard's spacing and
menus are pixel-identical; embedded graphs now refit when their panel
resizes (gpui-graph 0.2.2+), so they fill it better.

## Seen, not changed

- The `old_format` message says "older" for a graph a newer HawDB wrote (see
  above).
- HawDB 0.39 rejects a path variable (`MATCH p = (a)-[r]->(b) RETURN p`:
  "unknown variable 'p'"); whether 0.34 accepted it was not checked.

HawDB's release notes, including their `Persistent format changed` line, were
not checked; the 0.34 fixture opening and answering unchanged is the evidence
that the format still reads. Performance was not measured. macOS and Linux
were not verified locally (CI runs macOS and Windows).

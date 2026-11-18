# HawDB 0.34.0 upgrade

Validated on Windows Server 2025 on 2026-11-17.

## Changes

- Pin HawDB to 0.34.0 (from 0.29.0); `HAWDB_VERSION` follows the pin.
- **No graph written by KhonsuGraph v0.3.0 or earlier opens.** HawDB 0.31
  introduced a persistent format version (2) and reads no graph 0.29 wrote;
  0.31–0.34 share that format. HawDB offers no in-place upgrade, and its
  logical dump (export with the old library, import with the new) only exists
  from 0.31, so a 0.29 graph cannot be exported either. Such a graph reports
  `old_format`, now worded "before 0.31", and `khonsu rebuild` makes it again
  from its data sources; anything written by hand rather than synced is lost.
  Whether 0.30 reads 0.29 graphs was not checked: KhonsuGraph never pinned it.
- No API change touched KhonsuGraph: the workspace builds against 0.34 as it
  did against 0.29.

## Validation

```sh
cargo build --workspace --all-targets --locked
cargo test --workspace --locked --no-fail-fast
```

Every test passes on 0.34. `examples/aws/golden.json` did not change: the
offline investigation gives the same answers.

`graph-hawdb-0.34.0.zip` replaces the 0.27 and 0.29 fixtures, which 0.34
cannot read (both open as `old_format`), so the next upgrade is tested against
this version.

## Next time

From 0.31 on, a format change can be carried across: export each graph with
`Database::export_logical_dump` on the old pin and import it with
`Database::import_logical_dump` on the new one (HawDB's
`docs/LOGICAL_DUMP.md`). Each HawDB release note says
`Persistent format changed: yes|no`; check it before bumping the pin.

## Seen, not changed

- The Cargo released 2026-11-16 builds test binaries outside `target/`
  (`~/.cargo/targets/<project>-<hash>/…/out`), so a test no longer finds the
  CSV plugin it built in `target/debug` and
  `csv_source_syncs_counts_match_rows_and_resync_is_idempotent` fails. It is
  not a HawDB change; the suite above ran with
  `KHONSU_PLUGIN_PATH=target/debug`.

Performance was not measured. macOS and Linux were not verified.

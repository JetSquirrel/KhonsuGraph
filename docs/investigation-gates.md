# Investigation gates

The offline AWS example is tested from saved responses to the answers an
investigator reads. It needs no AWS access, and CI runs it on macOS and Windows
as part of `cargo test --workspace`. To run it alone:

```sh
cargo test --locked --workspace --test investigation
```

## What is checked

| Area | How |
|---|---|
| Sync, schema, analysis and the dashboard | `examples/aws/golden.json`, by stable IDs and relationship types |
| Candidate paths, direction, hop cap and type filters | Named queries in the golden file; the path answers are also written out in the test |
| Determinism | Two separate syncs give the same answers and internal IDs; a re-sync writes no relationships |
| Limits | `--max-rows` reports `truncated`; the 100,000-row ceiling is in `crates/khonsu-core/tests/engine.rs` |
| Path Explorer | `crates/khonsu-core/tests/explore.rs` covers direction, cycles, zero matches, each cap and cancellation; the golden file holds two `khonsu explore` walks |
| Cancellation | A sync cancelled partway reports it, and the graph keeps its answers |
| Corrupt input | A malformed response, a missing folder, a bad dashboard and a damaged graph each fail with a reason |
| Cycles | A shortest path and a variable-length pattern both end on a cycle |
| Evidence | `khonsu evidence` and `explore --to` give stable IDs, state what is unknown, and mask secrets; the golden file holds a node and a relationship |
| Exposure rules | `plugins/aws/tests/fixture-exposure` has a positive, a negative and an undecided case; the golden file holds its whole report, and rule IDs and verdicts for the shop fixture. See [the rules](../plugins/aws/README.md) |
| Plugin analyses | An example analyzer with nothing to do with AWS gives findings on a graph of its own; a forged reference, a missing end, a failure, an endless line and output that isn't the protocol are each refused; one that hangs is ended; a changed plugin doesn't run; a plugin with no analyses goes on syncing |
| Saved investigations | Reopening restores a walk and path on unchanged data, and flags a changed source, a missing record, a missing graph and a bad file; the golden file holds a reopened walk and the fixture's fingerprint. See [saved investigations](investigations.md) |
| Sync coverage | A scripted plugin fails midway, stops without `done`, reports gaps, sends duplicates and missing endpoints, and moves its cursor; none of it removes records or moves the cursor unless the sync was complete |
| Snapshots and diff | Two kept snapshots give a fixed diff; the AWS and CSV examples report what changed, a no-op re-sync gives none, and incomplete coverage is never called a removal. See [snapshots](snapshots.md) |
| Plugin safety | A changed program, file or manifest needs approval again; entries and IDs can't leave the plugin folder; a bad reinstall keeps the old plugin; oversized and invalid output end a sync with records intact. See [plugin security](plugin-security.md) |
| HawDB upgrades | `app/tests/fixtures/graph-hawdb-*.zip` must still give the golden answers and fingerprint, and accept a write |

Timings, sync times, paths, line numbers and plugin versions are left out of the
comparison.
Floats are rounded to nine decimal places.

## After a change

If an answer changes on purpose, regenerate the golden file and review its diff:

```sh
KHONSU_BLESS=1 cargo test --locked --workspace --test investigation the_offline
```

This also writes a snapshot for the pinned HawDB version. Bumping the pin fails
the gate until that snapshot exists, so each upgrade is tested against a graph
the previous version wrote. Keep the older snapshots. Delete one only when HawDB
drops that format on purpose, and say so in the upgrade notes.

## Known unsupported semantics

These are written down so that nobody reads an empty result as proof:

- **A found path is a candidate.** The graph holds configuration. It does not
  show that traffic can flow, or that a privilege can be used.
- **No path found does not mean no exposure.** The security group chain to
  `db-1` exists as rules, while the directed path query finds nothing, because
  `PROTECTED_BY` points from the instance to its group. The two queries ask
  different questions.
- **A partial sync removes nothing, so its records can be out of date.** A saved
  response that is missing makes the sync `partial`. What the graph already held
  stays until a complete sync. Evidence, exposure findings and reopened
  investigations say which sources this applies to.
- **A real account's sync is complete or failed, never partial.** The AWS plugin
  stops at the first call that fails. Records from before that are kept.
- **Runs aren't recorded for older syncs.** A source last synced before runs were
  kept has unknown coverage, and isn't treated as incomplete.
- **HawDB's variable-length patterns take one relationship type.**
  `[:A|B*..6]` does not parse. Use explicit hops for a typed chain, or an untyped
  `shortestPath`.
- **`type(r)` fails inside a comprehension over `relationships(p)`.** Use
  `[r IN relationships(p) | r.type]`.
- **A running query cannot be cancelled.** `Graph::run` has no way to stop one.
  Syncs can be cancelled, and a walk stops at its next query, so one slow step
  still finishes first.

- **`record_written_at` is when a record last changed.** A sync does not rewrite
  an unchanged node or relationship, so this can be older than the sync that last
  saw it.
  `source_last_successful_sync` is the source's last complete sync, and
  `source_coverage` says how its latest one ended.
- **Evidence shows what is stored, not the raw response.** The AWS plugin stores
  chosen fields, not the API call each came from.

- **An exposure finding is not reachability.** It says what configuration allows.
  [The rules](exposure-rules.md) list what they do not read.


## Baselines

Measured on 2026-11-02 with `examples/datasets/baseline.ps1`, on HawDB 0.24.0.

- **Hardware:** AMD Ryzen 5 6600H (6 cores, 12 threads), 31 GB RAM, NVMe SSD,
  Windows 11.
- **Build:** debug (`dev` profile, dependencies at `opt-level = 1`), so release
  builds should differ.
- **Figures:** median of 3 runs, whole process including opening the graph. Peak
  memory is the `khonsu` process alone, sampled every 5 ms or so, and leaves out
  any plugin it starts.

| Graph | Operation | Time | Peak memory |
|---|---|---:|---:|
| AWS fixture (16 nodes, 24 relationships) | `analyze summary` | 324 ms | 23 MB |
| | `check exposure.dash` | 370 ms | 27 MB |
| | `shortestPath` query | 319 ms | 27 MB |
| Audit story (5,881 nodes, 35,592 relationships) | Sync into a new graph | 14.1 s (11.3–16.2 s) | 214 MB |
| | `count(n)` query | 3.3 s | 133 MB |
| | Two-hop `count` query | 20.4 s | 139 MB |
| | `analyze pagerank` | 7.1 s | 177 MB |
| | `check trust_network.dash` | 3.8 s | 145 MB |

On the audit graph, about 3 s of each command is spent outside the query: `count(n)`
reported 0.5–0.7 s of its own time in a whole-process run of 3.7–3.9 s. Why was
not investigated. The two-hop query reported about 19 s for 1,948,728 paths.

These figures describe one machine and one build. They are not targets or
performance claims.

### Release, Windows

Measured on 2026-11-03 at commit `10d29e2` on the same machine and HawDB version,
with a `--release` build. Figures are the median of 3 runs, measured the same way,
except the sync, which ran once. Disk caches weren't cleared, so these aren't
cold-start figures.

| Graph | Operation | Time | Peak memory |
|---|---|---:|---:|
| AWS exposure fixture (27 nodes, 45 relationships) | `analyze summary` | 175 ms | 22 MiB |
| | `check exposure.dash` | 179 ms | 25 MiB |
| | `shortestPath` query | 185 ms | 22 MiB |
| | `plugin analyze aws exposure` | 502 ms | 24 MiB |
| | `explore`, both directions, 2 hops, 2 types | 202 ms | 24 MiB |
| | `evidence` on one node | 203 ms | 22 MiB |
| Audit story (5,881 nodes, 35,592 relationships) | Sync into a new graph, one run | 4.2 s | 213 MiB |
| | `count(n)` query | 1.2 s | 131 MiB |
| | Two-hop `count` query | 5.1 s | 133 MiB |
| | `analyze summary` | 2.6 s | 178 MiB |
| | `analyze pagerank` | 2.2 s | 170 MiB |
| | `check trust_network.dash` | 1.6 s | 139 MiB |

Don't read these as a speed-up from the debug table: the AWS graph is a different
fixture, and the two-hop query counted 2,301,858 paths here against 1,948,728 there.

**Not measured:** macOS, UI responsiveness (input to visible change, frame rate, a
busy window) and how long a cancel takes under load. The window has no scripted way
to measure responsiveness yet.

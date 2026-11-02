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
| Cancellation | A sync cancelled partway reports it, and the graph keeps its answers |
| Corrupt input | A malformed response, a missing folder, a bad dashboard and a damaged graph each fail with a reason |
| Cycles | A shortest path and a variable-length pattern both end on a cycle |
| HawDB upgrades | `app/tests/fixtures/graph-hawdb-*.zip` must still give the golden answers and accept a write |

Timings, sync times, paths and line numbers are left out of the comparison.
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
- **A missing saved response reads as an empty one (#38).** A partial export
  syncs without a warning, and a re-sync removes what the missing file used to
  describe. A test records this behaviour, so fixing it has to change the test.
- **HawDB's variable-length patterns take one relationship type.**
  `[:A|B*..6]` does not parse. Use explicit hops for a typed chain, or an untyped
  `shortestPath`.
- **`type(r)` fails inside a comprehension over `relationships(p)`.** Use
  `[r IN relationships(p) | r.type]`.
- **A running query cannot be cancelled.** `Graph::run` has no way to stop one;
  only syncs can be cancelled, and only they are tested.

Evidence provenance and rule names are not in the golden file yet, because they
arrive with #33 and #35.

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

**Not measured:** release builds, macOS, and UI responsiveness. The window has no
scripted way to measure responsiveness yet.

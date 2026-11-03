# Snapshots and diff

A snapshot keeps a graph's records in a file, by stable IDs, so you can later ask
what changed. HawDB has no time travel, and this doesn't add any: a snapshot exists
only if you took it.

```sh
khonsu snapshot save monday.snapshot.json --db graphs/aws
khonsu plugin sync --source shop
khonsu snapshot save tuesday.snapshot.json --db graphs/aws
khonsu snapshot diff monday.snapshot.json tuesday.snapshot.json --pretty
khonsu snapshot check monday.snapshot.json --db graphs/aws
```

## What a diff says

Nodes and relationships each list:

| List | Meaning |
|---|---|
| `added` | In the later snapshot only |
| `removed` | Gone, and the source's last sync was `complete` in the later snapshot |
| `not_seen` | Missing from the later snapshot, and nothing shows the source was read completely. They may still exist |
| `changed` | The same stable ID with different properties, labels or ends |

A record is only `removed` when the later snapshot can back that up. After a
partial, failed or cancelled sync, or with a source that has no record of its runs
or no records left, it stays in `not_seen`, and `warnings` says why.

`node_counts` and `relationship_counts` count everything. The lists hold at most
`--max-changes` in all, 1,000 by default, and `truncated` counts the rest.

The same two files always give the same diff.

## What a snapshot holds

- Each record's stable ID, labels or type, ends and own properties.
- Each source's last run and plugin version at that moment.
- A digest of the records, which also gives the `snapshot_id`. The same records give
  the same ID.

It doesn't hold the graph's path, source configuration or credentials. Anything that
looks secret is masked as evidence masks it, so a changed secret doesn't show as a
change. KhonsuGraph's own bookkeeping isn't kept, so a re-sync that changes nothing
gives an empty diff.

The file does hold your data's other properties, such as names and addresses, so
treat it as you would the graph.

## Limits

- **A graph over 200,000 nodes or relationships is refused** with kind `too_large`.
  A snapshot cut short would read as removals later.
- **Records with no stable ID aren't compared.** They're counted, and `warnings` says
  how many. A record written by hand usually has none.
- **A diff lists records, not meanings.** It doesn't say which exposure findings or
  paths changed. To see that, run the analysis on the graph, such as `khonsu plugin analyze aws exposure`, or compare the
  relationships it lists.
- **Snapshots are taken by hand.** Nothing takes one when a sync runs.
- **The window has no snapshot or diff view yet.** It does warn when a reopened
  investigation's graph has moved on.

## Staleness

`khonsu snapshot check` compares a snapshot with the graph and gives `graph_state`, as
reopening an investigation does.

A saved investigation pins the source runs it was evaluated against, along with the
graph's digest. Reopening one on a changed graph names the run it was saved at and
the run now, in `differences`, and the window shows them as warnings.

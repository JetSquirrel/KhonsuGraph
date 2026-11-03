# Saved investigations

A saved investigation keeps what you were looking at in a file of its own: a walk's
starting points and filters, the path you chose, a query and a dashboard. Saving
writes nothing to the graph.

```sh
khonsu investigation save web.investigation --db graphs/aws \
  --from shop/cidr:internet --to shop/123456789012:us-east-1:i-web1 \
  --type ROUTES_TO,FORWARDS_TO --max-hops 4
khonsu investigation reopen web.investigation
```

To keep a finding's path exactly, pass its `path_refs`. A walk finds the fewest hops,
which isn't always the path a finding describes, so these are looked up one by one:

```sh
khonsu investigation save db.investigation --db graphs/aws \
  --path-refs '[{"edge": "rel:...", "along": true}]'
```

Reopening then checks that each of those records is still there and that they still
connect. If not, `path_same` is false, and the walk runs instead. Saving from the
window does this for you after **Show path on canvas**.

## What reopening tells you

Reopening runs the walk again on the graph as it is now and says how that differs
from when it was saved. Read `graph_state` first.

| `graph_state` | Meaning |
|---|---|
| `unchanged` | The graph holds what it did when saved |
| `changed` | Something differs. `differences` gives what it can, and is not a full list |
| `not_compared` | The graph holds over 200,000 nodes or relationships, so it was not hashed, and the counts agree |

A changed answer doesn't replace the saved one silently:

- `path_same` says whether the path rests on the same records. It compares which
  records the path crosses, so a changed value on one of them gives `graph_state`
  `changed` with `path_same` true.
- `missing_seeds` and `target_missing` name saved records that have gone. Nothing
  stands in for them, and the walk runs from the seeds that remain.
- `unsure` lists records found by an internal ID on a changed graph. A record
  written by hand has no stable ID, so the same number may now mean something else.

A saved query is returned, not run.

## What counts as a change

The fingerprint hashes every node and relationship: labels, types, ends and
properties. Internal IDs, sync times and plugin versions are left out, so a re-sync
that changes nothing stays `unchanged`, and a graph rebuilt from the same data
matches. It covers the whole graph, not just the records you saved, so an unrelated
change also reads as `changed`.

The hash is for noticing change, not for security: someone who wants to can
produce a matching one. It also has no record of what the source held when saved.
Comparing source snapshots is #37.

## The file

It is JSON with a `format` and a `version`, so it can be read and compared.

- **Holds:** the graph's path, data source IDs, stable record IDs and filters.
- **Never holds:** source configuration, or credentials.
- **Saved as typed:** the name and any query. Leave anything secret out of them. The
  graph's path can include a user name, so check it before sharing a file.

Fields a release doesn't know are ignored, so a later one can add some. A file from
a later version fails with kind `too_new` rather than being read wrongly. A file
that can't be used fails with kind `invalid`, and a graph that has moved fails with
`graph_missing`; pass `-d` to use another one.

## In the window

Select a node on a graph canvas, then choose **Save investigation…** in Path
Explorer. It keeps the current filters, the last walk's starting points, and the
path to the selected node if that walk reached it, along with the tab's query.

**File > Open investigation…** or `khonsu open web.investigation` reopens one in a
new tab, and opens its graph first if another one is open. The query goes in the
editor without running. The canvas shows the saved path, with **Back** leading to
the whole walk, and Path Explorer shows the filters. Anything that differs from
when it was saved appears as a warning on the canvas and in Path Explorer.

A new query in that tab clears the warnings along with the walk. Reopening a file
again adds another tab.

## Limits

- `khonsu investigation` needs the graph free. It fails with kind `locked` while
  the window has it open, so use the window for a graph that's open there.
- The window saves no dashboard or layout. A saved file's `dashboard` is not opened
  on reopening, and node positions start over.
- A file isn't reopened by itself when the app restarts.

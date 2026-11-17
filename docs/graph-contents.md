# What goes in a graph

Two rules decide where KhonsuGraph keeps something. New features follow them;
a feature that needs to break one says so and why.

## 1. A graph is what its sources and files make it

A graph holds the records its data sources wrote, and anything that can be
worked out again from those sources and from files KhonsuGraph keeps. Nothing
else.

HawDB has changed its storage format without a migration three times so far
(0.12, 0.19, 0.25), and each time the graph was made again from its sources with
`khonsu graph rebuild`. Whatever was only in the graph was lost. So what a person or
an agent decides about the data lives in a file, not in the graph:

| What | Where |
|---|---|
| Records from a data source | The graph, owned by that source (`khonsu_source`, `khonsu_keys`; see `crates/khonsu-core/src/merge.rs`) |
| A walk and path someone is following | A saved investigation (`.investigation`) |
| A template run, its findings, hypotheses, conclusions and reviews | A report (`.report.json`) |
| What the graph held at a moment | A snapshot |
| Definitions of what records mean, and decisions such as "these two accounts are one" | Files (planned), applied to the graph by a step that runs after each sync, so a rebuild applies them again |

A statement run with `khonsu query --write` is the exception the CLI allows: it
is in no source and no file, so a rebuild doesn't carry it over. Each one is
logged in `writes.jsonl` in the config folder (when, which graph, the statement
with key material masked, the agent that ran it when one is detected, and
whether it went through the window), so what was written by hand can be found
and written again.

## 2. No time travel

A sync makes the graph look like its source now: records the source no longer
has are removed, and properties it stopped sending are taken off. Unchanged
records are not rewritten, because each rewrite costs HawDB a new version of
the record. KhonsuGraph keeps no history inside the graph.

"What changed?" is answered by [snapshots](snapshots.md) and their diff, and
"what did we conclude then?" by reports, which keep the run they were made from.
Times that are part of the data (when something was valid, first or last seen)
are ordinary properties, and can be queried like any other.

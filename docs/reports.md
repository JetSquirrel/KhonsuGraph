# Investigation reports

A report keeps one [template](templates.md) run in a file, with what a reviewer
needs to check it and run it again. A screenshot or a summary can't be
re-run. This can.

```sh
khonsu template run blast-radius --db graphs/infra -p service=infra/svc-db \
  --report db.report.json --snapshot db.snapshot.json
khonsu report verify db.report.json
khonsu report export db.report.json --markdown db.md
khonsu report replay db.report.json
```

## What is in the file

| Part | Holds |
|---|---|
| `result_id` | The same template, parameters and graph contents give the same ID, whenever and wherever the run was made |
| `template` | The template as it ran, whole, so the run can be made again without its file |
| `run` | The run: parameters, the graph's fingerprint, each source's last sync, steps with their caps and errors, findings, evidence and `gaps` |
| `snapshot` | With `--snapshot`: the ID and file of a snapshot taken with the run, for [saying what changed](snapshots.md) later |
| `actions` | What was done, in order: the run, each step with its caps and outcome, and every note and review after it |
| `notes`, `reviews` | What people and agents added |
| `digest` | SHA-256 of everything else in the file |

The schema is `khonsu-report` version 1. A later release may add fields; a file
from a later version is refused with kind `too_new`.

## Four things kept apart

A report that blurs these cannot be checked:

| What | Where | Who says it |
|---|---|---|
| Observations | `run.evidence`, and findings of class `observation` | A data source |
| Derived findings | Findings of class `derived` | A walk or a rule, from the observations |
| Hypotheses | `notes` of kind `hypothesis` | Whoever suspects it, an agent included |
| Human conclusions | `reviews`, and `notes` of kind `conclusion` | A person |

```sh
khonsu report note db.report.json --hypothesis "redis may hide this" --by agent --finding finding:d5d2c110aaa171e6
khonsu report review db.report.json --finding finding:d5d2c110aaa171e6 --state confirmed --by jet --note "no fallback"
khonsu report note db.report.json --conclusion "postgres failing takes the storefront down" --by jet
```

A review's `--state` is `confirmed`, `rejected` or `needs-more`. A later review
of the same finding replaces the earlier one, and both stay in `actions`. The
Markdown export has a section for each of the four.

An agent may record a hypothesis. It should not record a review or a conclusion
unless a person told it what they decided: those are the person's.

## Checking it

`khonsu report verify FILE` recomputes the digest, and checks that the template
in the file is the one the run names, that the result ID matches the run, and
that every record a finding cites is in the file. It exits 1 when any of that
fails, and `problems` says what. `note` and `review` refuse to build on a file
that doesn't verify.

The digest catches accidents and careless edits. It is not a signature: whoever
can write the file can write a new digest. To hold someone to a report, keep the
digest somewhere they can't change.

## Running it again

`khonsu report replay FILE` makes the run again, with the saved template and
parameters, and compares finding IDs. `--db` uses another graph; `-p` changes a
parameter, or gives again one the report masked.

| Field | Meaning |
|---|---|
| `graph_state` | `unchanged`, `changed`, or `not_compared` when the graph is too large to hash and the counts agree |
| `differences` | What changed in the graph, in words. Not exhaustive |
| `same` | True when the result ID is the saved one: every finding the same, nothing else about the answer different |
| `unchanged`, `added`, `removed` | Findings in both, only there now, and only in the report |
| `run` | The run just made |

On a graph that holds what it did, `same` is true. When `graph_state` is
`changed`, `added` and `removed` say what that did to the answer, and
`khonsu snapshot check` or `snapshot diff` against the report's snapshot says
what changed in the data. Replay does not change the report: keep the new run
with `template run --report`.

## Showing a finding in the window

```sh
khonsu report export db.report.json --investigation web.investigation --finding finding:d5d2c110aaa171e6
khonsu open web.investigation
```

This writes the finding's path as a [saved investigation](investigations.md),
which the window opens on the canvas and in Path Explorer. It works for a
finding with a path, which a walk's or a rule's has and a query row hasn't. No
AI service is involved in any of this.

## What is left out

- **Source configuration and credentials.** The file names sources by ID and
  holds their last sync's ID and status, nothing of how they connect.
- **Secrets in records.** Properties are masked as in `khonsu evidence`, by name
  (`password`, `token`, `api_key` and the like) and by shape (AWS access key
  IDs, PEM blocks), and the names are listed in `redacted`. A query row's values
  are masked the same way.
- **Key material in free text.** A note, a reviewer's name and an error message
  have AWS access key IDs and PEM blocks masked. That is a net for accidents,
  not a reason to paste a secret into a note.
- **Raw payloads.** Only records a finding rests on are kept, not everything a
  step read.

A parameter whose name is a secret's is masked in the file, so `replay` asks
for it again.

## What a report does not do

- It is not a case management system: no assignment, no status beyond a review
  per finding, no ticket links.
- It does not prove a path can be used. See
  [the gates](investigation-gates.md#known-unsupported-semantics).
- The Markdown is a rendering. The JSON file is what is checked and replayed.

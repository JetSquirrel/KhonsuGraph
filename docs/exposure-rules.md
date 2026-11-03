# AWS exposure rules

`khonsu plugin analyze aws exposure` says which instances an account's
configuration leaves potentially open to the internet, and shows how it decided.
It reads the graph only. It sends no traffic and needs no AWS access once the
graph is synced.

These rules belong to the AWS plugin, in `plugins/aws/src/exposure.rs`. KhonsuGraph
itself knows nothing about AWS: it sends the plugin records from the graph, checks
the findings that come back against them, and looks up their values and paths. See
[offering an analysis](plugins.md#offering-an-analysis).

A finding is **potential exposure worked out from configuration**. It does not
show that anything is reachable, and no finding does not show that a resource is
safe. No verdict says "reachable".

## Verdicts

Each rule is a list of conditions, each `met`, `not_met` or `unknown`. There is
no score.

| Verdict | When | How to read it |
|---|---|---|
| `potential_exposure` | Every condition is met | Check it: the configuration allows this path |
| `insufficient_data` | None is unmet and at least one is unknown, or a source behind it was not read completely | `unknowns` says what could not be read; get that data |
| `not_indicated` | At least one is unmet, and every source behind it was read completely as far as is known | This rule does not point here. Another route may exist |

Findings are ordered by verdict, then rule ID, then resource ID, so the same
graph always gives the same order.

## Rules

### `aws.exposure.direct-instance`

Every instance gets one finding.

| Condition | Field | Unknown when |
|---|---|---|
| `instance-running` | `describe-instances`: `State.Name` | No state was supplied |
| `public-address` | `describe-instances`: `PublicIpAddress`, `Ipv6Address` | Never: AWS omits the field when there is no address |
| `open-inbound-rule` | `describe-security-groups`: `IpPermissions[]` from `0.0.0.0/0` or `::/0` | No security groups are recorded, since every instance has one |
| `internet-route` | `describe-route-tables`: `Routes[]`, `Associations[]` | No subnet or route table is recorded, or only a narrower route leads to a gateway |

A subnet with no route table of its own uses its VPC's main table. A route counts
when it is active, goes to an internet gateway, and matches the family of the
instance's address.

### `aws.exposure.public-load-balancer`

Each target gets one finding per load balancer and target group that sends
traffic to it.

| Condition | Field | Unknown when |
|---|---|---|
| `instance-running` | `describe-instances`: `State.Name` | No state was supplied |
| `internet-facing` | `describe-load-balancers`: `Scheme` | No scheme was supplied |
| `balancer-open-rule` | `describe-security-groups`, on the balancer's groups | The balancer has no groups recorded, as a network load balancer can |
| `target-healthy` | `describe-target-health`: `TargetHealth.State` | Not supplied, or anything but `healthy` |
| `target-allows-balancer` | `describe-target-groups`: `Port`, `Protocol`; the target's groups and their rules | No port, protocol or target groups are recorded, only a CIDR rule covers the port, or no rule lets it in and the balancer has no groups recorded |

A `TCP_UDP` or `TCP_QUIC` target group carries both, so a rule for either protocol
counts. `UDP`, `QUIC` and `GENEVE` need a UDP rule, and anything else a TCP one.

### `aws.exposure.security-group-lateral`

One finding for each instance whose security group allows the
group of an instance with a `potential_exposure` or `insufficient_data` finding.
It follows one step. `depends_on` names the finding it rests on, and its verdict
can be no stronger than that one's.

## What the rules do not read

Each finding lists its assumptions, and they include these:

- Network ACLs, host firewalls, and anything outside the account.
- Whether anything listens on an allowed port.
- Load balancer listeners, so an open group is not shown to lead to a given target group.
- A target's own registered port. The target group's port is used.
- Address family on rules: `0.0.0.0/0` and `::/0` are one Internet record.
- IAM. The rules say nothing about what an instance's role can do.

Only `0.0.0.0/0` and `::/0` count as the internet. Rules that together cover the
address space, such as `0.0.0.0/1` and `128.0.0.0/1`, are not recognized.

## A missing response is not a clear result

A record that should exist and doesn't makes a condition `unknown`, not `not_met`.
A graph synced before route tables were read gives `insufficient_data` on the direct
rule until it is synced again.

A source's last sync can also be incomplete, because a saved response was missing,
the plugin reported a gap, or the sync failed or was cancelled. Then:

- Each finding that rests on it names it in `incomplete_sources` and in `unknowns`.
- A finding that would be `not_indicated` becomes `insufficient_data`, because what
  the sync missed could be what would have met a condition.
- A `potential_exposure` stays, because what the source did report still stands.
- The report's `complete` is false, and `sources` gives each source's last run.

A security group that is one of several on an instance and is missing from the
responses can't be detected this way, because the instance's other groups are
still there.

`complete` is also false when the graph held more than 50,000 records of one type. A
missing finding then means nothing.

## Walkthrough

This uses the saved responses in `plugins/aws/tests/fixture-exposure`, with no AWS
access. It holds one case for each verdict. Run it from the repository root. In
Git Bash on Windows, use `$(pwd -W)` in place of `$PWD`.

```sh
khonsu init graphs/exposure
khonsu source add --db graphs/exposure --plugin aws --name shop \
  -c "fixture_dir=\"$PWD/plugins/aws/tests/fixture-exposure\"" -c regions=us-east-1
khonsu plugin sync --source shop
khonsu plugin analyze aws exposure --db graphs/exposure --pretty
```

Read `summary` first: three potential exposures, one undecided, three not indicated.

**1. A potential exposure.** Look at one instance:

```sh
khonsu plugin analyze aws exposure --db graphs/exposure --pretty --resource shop/123456789012:us-east-1:i-web1
```

`web-1` meets all four conditions. Each condition's `fields` give the record, the
stored field and, in `source_field`, the AWS field it came from. `path.hops` shows the candidate path,
`path.rule_id` the rule, and `assumptions` what was not checked.

**2. Open it in Path Explorer.** A finding's `walk` holds the arguments:

```sh
khonsu explore --db graphs/exposure --from shop/cidr:internet --to shop/123456789012:us-east-1:i-web1 \
  --direction both --type ALLOWS,PROTECTED_BY --max-hops 2
```

To see more around it, raise `--max-hops` or drop `--type`.

**3. Check a record.** Every ID in `contributing` can be looked up. Remove the
`node:` or `rel:` prefix:

```sh
khonsu evidence --db graphs/exposure --node shop/123456789012:us-east-1:i-web1
```

**4. A security group rule that is not exposure.** `batch-1` has port 22 open to
`0.0.0.0/0`. It has no public address, and its subnet's default route goes to a NAT
gateway, so it gets `not_indicated`. The conditions show why, and the open rule
still deserves a look:

```sh
khonsu plugin analyze aws exposure --db graphs/exposure --pretty --resource shop/123456789012:us-east-1:i-batch1
```

**5. An undecided case.** `legacy-1` has a public address and an open rule, but the
saved responses hold no route tables for its VPC. `unknowns` says so:

```sh
khonsu plugin analyze aws exposure --db graphs/exposure --pretty --verdict insufficient-data
```

**6. One step further in.** `db-1` has no public address, and its group allows
`web-1`'s group on 5432. Its `aws.exposure.security-group-lateral` finding follows
from `web-1`'s through `depends_on`:

```sh
khonsu plugin analyze aws exposure --db graphs/exposure --pretty --resource shop/123456789012:us-east-1:i-db1
```

The commands above need the graph free. They fail with kind `locked` while the
window has it open.

## In the window

Select an instance on a graph canvas, then choose **Run analysis** under Exposure in
the inspector. The window offers it because the AWS plugin says it can speak about
instances, and every word it shows comes from the plugin. It evaluates the whole
graph, not just what is on the canvas, and the result lasts until the canvas gets a
new result. After a sync, choose **Check again**. **Stop** ends a run that is taking
too long.

The window holds the graph only while reading it, not while the plugin works, and
gives the same findings as `khonsu plugin analyze aws exposure`.

Each finding shows its verdict, rule, conditions with their AWS fields, candidate
path and what was not checked.

- **Show path on canvas** narrows the canvas to the path. **Back** returns to the
  earlier view, and selecting a node on the path shows its evidence.
- **Copy finding as JSON** copies the same record as the command.

In the Chinese interface, verdicts, rule titles and condition statements are
translated, by the plugin. Each condition's detail and the list of what was not
checked stay in English.

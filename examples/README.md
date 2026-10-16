# Examples: five user stories on public data

Each folder holds a `.dash` dashboard for one user story in the PRD. The data
comes from public datasets, downloaded and converted by `datasets/`:

| Story | Dashboard | Data | Graph |
| --- | --- | --- | --- |
| Security engineer finds the attack chain in noisy logs | `security/attack_chain.dash` | MITRE ATT&CK; OTRF Security-Datasets Sysmon recordings (Empire Mimikatz, Covenant PS remoting) | 1,838 nodes, 20,591 relationships |
| SRE finds a failing service's blast radius | `sre/blast_radius.dash` | Google Online Boutique manifests | 12 services, 16 call paths |
| Investigator maps who stands behind a set of companies | `investigation/offshore_network.dash` | ICIJ Offshore Leaks (Panama Papers, Hong Kong, 2012–2015) | 5,577 nodes, 6,515 relationships |
| Auditor looks for accounts others warn about | `audit/trust_network.dash` | SNAP Bitcoin OTC trust network | 5,881 accounts, 35,592 ratings |
| A renter looks for a place near the MTR | `renting/near_mtr.dash` | OpenStreetMap MTR lines and stations; Inside Airbnb Hong Kong | 5,730 nodes, 17,119 relationships |

`infra/blast_radius.dash` runs on the small CSV demo in `csv/`.

`aws/exposure.dash` shows what the internet can reach in an AWS account. Try
it on the saved responses that ship with the AWS plugin:

```sh
khonsu init graphs/aws
khonsu source add --db graphs/aws --plugin aws --name "shop account"   -c "fixture_dir=\"$PWD/plugins/aws/tests/fixture\"" -c 'regions=["us-east-1"]'
khonsu plugin sync --source shop-account
khonsu check examples/aws/exposure.dash --db graphs/aws
```

For a real account, drop `fixture_dir` and set `profile` to a read-only
profile; the plugin only calls `describe`, `list` and `get` operations.

## Build a story graph

```sh
sh examples/datasets/fetch.sh scratch/datasets/raw
node examples/datasets/build.mjs security scratch/datasets/raw scratch/datasets/security

khonsu init graphs/security
khonsu source add --db graphs/security --plugin csv --name "security data" \
  -c 'nodes=[{"path":"scratch/datasets/security/nodes.csv","label_column":"label","id_column":"id"}]' \
  -c 'edges=[{"path":"scratch/datasets/security/edges.csv","type_column":"type","source_column":"source","target_column":"target","id_column":"id"}]'
khonsu plugin sync --source security-data
khonsu check examples/security/attack_chain.dash --db graphs/security
khonsu open graphs/security && khonsu open examples/security/attack_chain.dash
```

Swap `security` for `sre`, `investigation`, `audit` or `renting`. Paths in
`source add` are stored as given, so use absolute ones if you will sync from
another folder. Every imported graph's node and relationship counts equal the
rows of its CSVs.

## Working with an agent

The dashboards are written for the review loop the `khonsugraph` skill
describes: an agent edits the spec and runs `khonsu check`, the person
comments on a plot in the window, the agent reads `khonsu comments`, changes
the spec and replies. While the window has the graph open, the agent's
`query` and `check` run through the window.

## Licenses

ATT&CK® is MITRE's (attack.mitre.org terms of use); OTRF Security-Datasets
are MIT; Online Boutique is Apache-2.0; the Offshore Leaks database is ICIJ's
under ODbL, with ICIJ's caveat that listing a person or company does not imply
wrongdoing; Bitcoin OTC is from SNAP (Kumar et al., 2016); MTR data is
© OpenStreetMap contributors under ODbL; Inside Airbnb data is CC BY 4.0. None
of the data is in this repository.

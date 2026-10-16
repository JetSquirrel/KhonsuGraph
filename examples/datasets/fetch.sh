#!/usr/bin/env sh
# Downloads the public datasets the user-story examples are built from into
# $1 (default scratch/datasets/raw). Then, for each story:
#   node examples/datasets/build.mjs <story> scratch/datasets/raw scratch/datasets/<story>
set -e
RAW=${1:-scratch/datasets/raw}
mkdir -p "$RAW" "$RAW/otrf" "$RAW/icij"
UA="KhonsuGraph-dataset-fetch/0.1 (github.com/JetSquirrel/KhonsuGraph)"

# security: MITRE ATT&CK (CC BY 4.0-style terms, see attack.mitre.org) and two
# OTRF Security-Datasets recordings (MIT): credential dumping, lateral movement.
curl -sL -o "$RAW/enterprise-attack.json" https://raw.githubusercontent.com/mitre/cti/master/enterprise-attack/enterprise-attack.json
for p in credential_access/host/empire_mimikatz_logonpasswords.zip lateral_movement/host/covenant_psremoting_command.zip; do
  curl -sL -o "$RAW/otrf_$(basename "$p")" "https://raw.githubusercontent.com/OTRF/Security-Datasets/master/datasets/atomic/windows/$p"
  unzip -o -q "$RAW/otrf_$(basename "$p")" -d "$RAW/otrf"
done

# sre: Google Online Boutique (Apache-2.0) manifests.
curl -sL -o "$RAW/boutique-k8s.yaml" https://raw.githubusercontent.com/GoogleCloudPlatform/microservices-demo/main/release/kubernetes-manifests.yaml

# investigation: ICIJ Offshore Leaks database (ODbL; cite ICIJ).
curl -sL -C - -o "$RAW/offshoreleaks.zip" https://offshoreleaks-data.icij.org/offshoreleaks/csv/full-oldb.LATEST.zip
unzip -o -q "$RAW/offshoreleaks.zip" -d "$RAW/icij"

# audit: SNAP Bitcoin OTC trust network.
curl -sL -o "$RAW/soc-sign-bitcoinotc.csv.gz" https://snap.stanford.edu/data/soc-sign-bitcoinotc.csv.gz

# renting: Inside Airbnb Hong Kong (CC BY 4.0) and MTR routes from
# OpenStreetMap (ODbL) through the Overpass API.
curl -sL -o "$RAW/hk-listings.csv" https://data.insideairbnb.com/china/hk/hong-kong/2026-09-20/visualisations/listings.csv
curl -s -X POST https://overpass-api.de/api/interpreter -H "User-Agent: $UA" \
  --data-urlencode 'data=[out:json][timeout:90][bbox:22.15,113.82,22.57,114.45];rel["route"="subway"];out body;node(r);out body;' \
  -o "$RAW/hk-mtr.json"

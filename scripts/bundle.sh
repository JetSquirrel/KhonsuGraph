#!/bin/bash
# Build KhonsuGraph.app, unsigned, for trying a build locally on a Mac.
# scripts/package-macos.sh makes the signed, notarized dmg that ships.
#
# The bundle holds the app, which is the `khonsu` command too, and the
# official plugins in Contents/MacOS: it finds built-in plugins next to its
# own executable, also when run through the `khonsu` link.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --release --locked -p khonsugraph -p khonsu-plugin-csv -p khonsu-plugin-aws

APP="target/release/KhonsuGraph.app"
scripts/assemble-app.sh target/release "$APP"
echo "Built $APP"

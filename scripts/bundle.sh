#!/bin/bash
# Build KhonsuGraph.app, unsigned, for trying a build locally on a Mac.
# scripts/package-macos.sh makes the signed, notarized dmg that ships.
#
# The bundle holds the app, the `khonsu` CLI and the official plugins side
# by side in Contents/MacOS: the app and the CLI find built-in plugins next
# to their own executable, and `khonsu open` finds the app the same way.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --release --locked -p khonsugraph -p khonsu-cli -p khonsu-plugin-csv -p khonsu-plugin-aws

APP="target/release/KhonsuGraph.app"
scripts/assemble-app.sh target/release "$APP"
echo "Built $APP"

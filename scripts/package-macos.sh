#!/usr/bin/env bash
#
# Package release binaries as KhonsuGraph.app inside a .dmg, signed with a
# Developer ID and notarized by Apple. Adapted from DuckLocal's script.
#
# Signing and notarization are required, not optional: since macOS 15 an
# unnotarized app cannot reasonably be opened, so this fails rather than
# ship one by accident.
#
# Usage: scripts/package-macos.sh <dir with the release binaries> <output.dmg> <version>
# Run from the repository root. Needs dmgbuild (pipx install dmgbuild).
#
# Required environment:
#   NOTARY_KEY        Path to the App Store Connect API key (.p8)
#   NOTARY_KEY_ID     That key's Key ID
#   NOTARY_ISSUER     The issuer UUID the key belongs to
# Optional:
#   MACOS_SIGN_IDENTITY  Defaults to the one "Developer ID Application"
#                        identity in the keychain.
set -euo pipefail

BIN="$1"
DMG="$2"
VERSION="$3"
APP="KhonsuGraph.app"

die() {
  echo "package-macos: $*" >&2
  exit 1
}

# Credentials first: a configuration mistake should not cost a build.
if [ -z "${MACOS_SIGN_IDENTITY:-}" ]; then
  IDENTITIES=()
  while IFS= read -r line; do
    IDENTITIES+=("$line")
  done < <(
    security find-identity -v -p codesigning |
      sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p'
  )
  case ${#IDENTITIES[@]} in
    1) MACOS_SIGN_IDENTITY="${IDENTITIES[0]}" ;;
    0) die "no 'Developer ID Application' identity in the keychain; set MACOS_SIGN_IDENTITY or install one." ;;
    *) die "${#IDENTITIES[@]} Developer ID Application identities found; set MACOS_SIGN_IDENTITY:
$(printf '  %s\n' "${IDENTITIES[@]}")" ;;
  esac
fi
for var in NOTARY_KEY NOTARY_KEY_ID NOTARY_ISSUER; do
  [ -n "${!var:-}" ] || die "$var is not set (App Store Connect API key for notarization)."
done
[ -f "$NOTARY_KEY" ] || die "NOTARY_KEY points at no file: $NOTARY_KEY"
command -v dmgbuild >/dev/null || die "dmgbuild is not installed: pipx install dmgbuild"

echo "package-macos: signing as $MACOS_SIGN_IDENTITY"

rm -f "$DMG"
scripts/assemble-app.sh "$BIN" "$APP" "$VERSION"

# Inside out, every executable on its own, no --deep. The hardened runtime
# and a timestamp are preconditions for notarization. Nothing needs an
# entitlement: HawDB is linked in, and plugins are separate signed programs.
sign() {
  codesign --force --timestamp --options runtime --sign "$MACOS_SIGN_IDENTITY" "$1"
}
for plugin in "$APP"/Contents/MacOS/plugins/*/khonsu-plugin-*; do
  sign "$plugin"
done
sign "$APP/Contents/MacOS/khonsu"
sign "$APP/Contents/MacOS/khonsugraph"
sign "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

dmgbuild -s scripts/dmg-settings.py -D app="$APP" KhonsuGraph "$DMG"
codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$DMG"

# One submission: the notary scans the dmg's contents, so the app inside is
# covered too. New teams can wait hours in the queue, hence the budget.
echo "package-macos: notarizing $DMG"
SUBMIT_LOG=$(mktemp -t notarytool)
if ! xcrun notarytool submit "$DMG" \
    --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" \
    --wait --timeout 300m | tee "$SUBMIT_LOG"; then
  SUBMISSION_ID=$(sed -n 's/^  id: //p' "$SUBMIT_LOG" | head -1)
  if [ -n "$SUBMISSION_ID" ]; then
    xcrun notarytool log "$SUBMISSION_ID" \
      --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" || true
  fi
  die "notarization did not complete cleanly (see log above)"
fi
xcrun stapler staple "$DMG"

xcrun stapler validate "$DMG"
spctl --assess --type execute --verbose=2 "$APP"
spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
echo "package-macos: $DMG is signed, notarized and stapled"

#!/bin/bash
# Install the newest Heron build from GitHub Actions on your iPhone.
#
#   scripts/heron/install-on-iphone.sh [run id]
#
# Downloads the `heron-ios-archive` artifact (an unsigned Release archive),
# signs it for development with your Apple team (Xcode's automatic signing:
# the Mac's Xcode must be signed in to that team), and installs it with
# devicectl on the first paired iPhone (USB or Wi-Fi, Developer Mode on).
#
# Env: HERON_REPO (kanishkaverma/heron), HERON_TEAM (Q49ZKW8324),
#      HERON_DEVICE (a devicectl device id; default: the first iPhone).
set -euo pipefail

REPO="${HERON_REPO:-kanishkaverma/heron}"
TEAM="${HERON_TEAM:-Q49ZKW8324}"
RUN="${1:-}"
WORK="$(mktemp -d /tmp/heron-install.XXXXXX)"

if [ -z "$RUN" ]; then
  RUN="$(gh run list -R "$REPO" -w "Heron iOS" -s success -L 20 --json databaseId,headBranch -q '[.[] | select(.headBranch == "main")][0].databaseId')"
fi
[ -n "$RUN" ] || { echo "No successful Heron iOS run on main yet." >&2; exit 1; }
echo "== Run $RUN"
gh run download "$RUN" -R "$REPO" -n heron-ios-archive -D "$WORK"
ditto -x -k "$WORK/Heron.xcarchive.zip" "$WORK"
ARCHIVE="$WORK/Heron.xcarchive"

echo "== Signing for team $TEAM"
OPTIONS="$WORK/ExportOptions.plist"
plutil -create xml1 "$OPTIONS"
plutil -insert method -string debugging "$OPTIONS"
plutil -insert signingStyle -string automatic "$OPTIONS"
plutil -insert teamID -string "$TEAM" "$OPTIONS"
plutil -insert compileBitcode -bool false "$OPTIONS"
plutil -insert thinning -string "<none>" "$OPTIONS"
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$WORK/export" \
  -exportOptionsPlist "$OPTIONS" -allowProvisioningUpdates > "$WORK/export.log" 2>&1 || {
  tail -40 "$WORK/export.log"; exit 1; }
IPA="$(ls "$WORK"/export/*.ipa | head -1)"
echo "signed: $IPA"

DEVICE="${HERON_DEVICE:-$(xcrun devicectl list devices 2>/dev/null | awk '/iPhone/ && /available|connected/ && /physical/ {for (i=1;i<=NF;i++) if ($i ~ /^[0-9A-F-]{36}$/) {print $i; exit}}')}"
[ -n "$DEVICE" ] || { echo "No paired iPhone found (xcrun devicectl list devices)." >&2; exit 1; }
echo "== Installing on $DEVICE"
xcrun devicectl device install app --device "$DEVICE" "$IPA"
xcrun devicectl device process launch --device "$DEVICE" com.kanishkaverma.heron || true
echo "Heron is on your iPhone."

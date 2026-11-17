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

if [ -n "${HERON_ARCHIVE:-}" ]; then
  ditto "$HERON_ARCHIVE" "$WORK/Heron.xcarchive"
  RUN=local
elif [ -z "$RUN" ]; then
  RUN="$(gh run list -R "$REPO" -w "Heron iOS" -s success -L 20 --json databaseId,headBranch -q '[.[] | select(.headBranch == "main")][0].databaseId')"
fi
[ -n "$RUN" ] || { echo "No successful Heron iOS run on main yet." >&2; exit 1; }
echo "== Run $RUN"
if [ "$RUN" != local ]; then
  gh run download "$RUN" -R "$REPO" -n heron-ios-archive -D "$WORK"
  ditto -x -k "$WORK/Heron.xcarchive.zip" "$WORK"
fi
ARCHIVE="$WORK/Heron.xcarchive"

echo "== Signing for team $TEAM"
# Signed here with codesign: this Mac's Apple Development certificate and the
# team profiles for Heron's two bundle ids (Xcode's export wants cloud-managed
# certificates, which not every team has). The profiles are made once per Mac
# by scripts/heron/make-profiles.sh.
APP="$WORK/Payload/$(basename "$ARCHIVE"/Products/Applications/*.app)"
mkdir -p "$WORK/Payload"
ditto "$ARCHIVE"/Products/Applications/*.app "$APP"
profile_for() {
  local id="$1" f
  for f in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"/* "$HOME/Library/MobileDevice/Provisioning Profiles"/*; do
    [ -f "$f" ] || continue
    security cms -D -i "$f" > "$WORK/p.plist" 2>/dev/null || continue
    [ "$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$WORK/p.plist" 2>/dev/null)" = "$TEAM.$id" ] || continue
    [ "$(date -j -f '%a %b %d %T %Z %Y' "$(/usr/libexec/PlistBuddy -c 'Print :ExpirationDate' "$WORK/p.plist")" +%s 2>/dev/null || echo 0)" -gt "$(date +%s)" ] || continue
    echo "$f"; return 0
  done
  echo "No profile for $id: run scripts/heron/make-profiles.sh" >&2; return 1
}
IDENTITY="$(security find-identity -v -p codesigning | awk -v team="$TEAM" '/Apple Development/ {print $2; exit}')"
sign() { # sign <bundle> <bundle id>
  local bundle="$1" id="$2" profile ent
  profile="$(profile_for "$id")"
  cp "$profile" "$bundle/embedded.mobileprovision"
  ent="$WORK/$id.entitlements"
  security cms -D -i "$profile" > "$WORK/p.plist"
  /usr/libexec/PlistBuddy -x -c 'Print :Entitlements' "$WORK/p.plist" > "$ent"
  codesign --force --sign "$IDENTITY" --entitlements "$ent" --timestamp=none --generate-entitlement-der "$bundle"
}
find "$APP" -depth \( -name "*.framework" -o -name "*.dylib" \) | while read -r lib; do
  codesign --force --sign "$IDENTITY" --timestamp=none "$lib"
done
for ext in "$APP"/PlugIns/*.appex; do
  [ -d "$ext" ] && sign "$ext" "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$ext/Info.plist")"
done
sign "$APP" "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Info.plist")"
codesign --verify --deep --strict "$APP"
echo "signed: $APP"

DEVICE="${HERON_DEVICE:-$(xcrun devicectl list devices 2>/dev/null | awk '/iPhone/ && /available|connected/ && /physical/ {for (i=1;i<=NF;i++) if ($i ~ /^[0-9A-F-]{36}$/) {print $i; exit}}')}"
[ -n "$DEVICE" ] || { echo "No paired iPhone found (xcrun devicectl list devices)." >&2; exit 1; }
echo "== Installing on $DEVICE"
xcrun devicectl device install app --device "$DEVICE" "$APP"
xcrun devicectl device process launch --device "$DEVICE" com.kanishkaverma.heron || true
echo "Heron is on your iPhone."

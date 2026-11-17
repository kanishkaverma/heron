#!/bin/bash
# The fast loop: build Heron for the iOS Simulator on this Mac, (re)install,
# launch, screenshot. A Swift-only change takes well under a minute. The app
# in the simulator is a real Zeron peer: signed in, it talks to the real Heron
# chat on air, so no phone is needed until the final install.
#
#   scripts/heron/sim.sh [launch args…]     e.g. sim.sh -heron-computer nishka-server
#   scripts/heron/sim.sh -demo -fast        the offline demo workspace
#
# Screenshot: /tmp/heron-sim.png. Needs the simulator core once:
# target/ios-core/iphonesimulator (see the "Build the Rust core for the
# simulator" step in .github/workflows/heron-ios.yml).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NAME="${HERON_SIM:-Heron iPhone}"
RUNTIME="${HERON_SIM_RUNTIME:-com.apple.CoreSimulator.SimRuntime.iOS-26-0}"
TYPE="${HERON_SIM_TYPE:-com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro}"
DERIVED="$HOME/Library/Developer/Xcode/DerivedData/HeronSim"

UDID="$(xcrun simctl list devices -j | python3 -c "import json,sys; n=sys.argv[1]; print(next((d['udid'] for r in json.load(sys.stdin)['devices'].values() for d in r if d['name']==n and d['isAvailable']), ''))" "$NAME")"
if [ -z "$UDID" ]; then
  UDID="$(xcrun simctl create "$NAME" "$TYPE" "$RUNTIME" 2>/dev/null || xcrun simctl create "$NAME" com.apple.CoreSimulator.SimDeviceType.iPhone-16e "$RUNTIME")"
fi
xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true

t0=$(date +%s)
cd "$ROOT/apps/ios"
if ! ZERON_SKIP_CORE=1 xcodebuild -project Zeron.xcodeproj -scheme Zeron -configuration Debug \
  -destination "id=$UDID" -derivedDataPath "$DERIVED" CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM= PROVISIONING_PROFILE_SPECIFIER= build > /tmp/heron-sim-build.log 2>&1; then
  grep -E "error:" /tmp/heron-sim-build.log | sort -u | head -20; exit 1
fi
APP="$DERIVED/Build/Products/Debug-iphonesimulator/Zeron.app"
xcrun simctl install "$UDID" "$APP"
xcrun simctl terminate "$UDID" com.kanishkaverma.heron >/dev/null 2>&1 || true
xcrun simctl launch "$UDID" com.kanishkaverma.heron "$@" >/dev/null
echo "built + launched in $(( $(date +%s) - t0 ))s on $NAME ($UDID)"
sleep "${HERON_SHOT_DELAY:-4}"
xcrun simctl io "$UDID" screenshot /tmp/heron-sim.png >/dev/null 2>&1 && echo "screenshot: /tmp/heron-sim.png"

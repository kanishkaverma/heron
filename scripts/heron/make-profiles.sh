#!/bin/bash
# Make (or refresh) the development profiles for Heron's two bundle ids, once
# per Mac: a signed Debug build with automatic signing, which registers the
# app ids and writes "iOS Team Provisioning Profile: com.kanishkaverma.heron"
# (and .LiveActivity) for every device on the team. Xcode must be signed in
# to the team. The Rust core isn't rebuilt (ZERON_SKIP_CORE) if one is there.
set -euo pipefail
cd "$(dirname "$0")/../../apps/ios"
ZERON_SKIP_CORE=1 xcodebuild -project Zeron.xcodeproj -scheme Zeron -configuration Debug \
  -destination 'generic/platform=iOS' -allowProvisioningUpdates \
  DEVELOPMENT_TEAM="${HERON_TEAM:-Q49ZKW8324}" build | grep -E "Provisioning Profile|error:|\*\* BUILD"

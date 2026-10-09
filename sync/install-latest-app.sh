#!/bin/bash
# Installs the latest Heron (kanishkaverma/heron) release as /Applications/Heron.app, beside
# Zeron, via a one-shot LaunchAgent (survives Heron quitting), with rollback if the new build
# won't stay up. Heron has its own bundle id (sh.heron.app) and data (~/.heron).
set -euo pipefail
REPO=kanishkaverma/heron
D=$HOME/.local/share/heron-update; mkdir -p "$D"; cd "$D"
TAG=$(gh release view -R "$REPO" --json tagName -q .tagName)
VER=${TAG#*-}
rm -f ./*.tar.gz manifest.json
gh release download "$TAG" -R "$REPO" -p '*macos-arm64-app.tar.gz' -p manifest.json
TAR=zeron-$VER-macos-arm64-app.tar.gz
[ "$(shasum -a 256 "$TAR" | cut -d' ' -f1)" = "$(jq -r '.files[].sha256' manifest.json 2>/dev/null || python3 -c 'import json;print(list(json.load(open("manifest.json"))["files"].values())[0]["sha256"])')" ] || { echo "sha mismatch"; exit 1; }
tar tzf "$TAR" | grep -q '^Zeron.app/Contents/Info.plist$' || { echo "$TAR has no Zeron.app"; exit 1; }
echo "verified $TAR"

# A Zeron.app from the old fork feed would update itself into a second Heron. Send it back to
# the official Zeron feed; it keeps running what it has until Zeron's next release.
Z=/Applications/Zeron.app
if [[ "$(plutil -extract LSEnvironment.ZERON_RELEASES_URL raw "$Z/Contents/Info.plist" 2>/dev/null)" == *kanishkaverma* ]]; then
  plutil -remove LSEnvironment.ZERON_RELEASES_URL "$Z/Contents/Info.plist"
  codesign --force --deep --sign - "$Z" 2>/dev/null
  echo "$Z now follows official Zeron releases"
fi

cat >"$D/update-app.sh" <<SCRIPT
#!/bin/bash
APP=/Applications/Heron.app; BIN=\$APP/Contents/MacOS/zeron; BACKUP=$D/Heron-previous.app
exec >>"$D/update-app.log" 2>&1
log() { echo "\$(date '+%F %T') \$*"; }
note() { osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Heron update"' -e 'end run' "\$1"; }
pids() { ps -axo pid=,command= | awk -v b="\$BIN" '{pid=\$1; sub(/^ *[0-9]+ /,""); if (\$0==b) print pid}'; }
running() { [ -n "\$(pids)" ]; }
# Delete the plist first: bootout kills this script, so nothing after it runs.
finish() { rm -f "\$HOME/Library/LaunchAgents/com.heron-update.plist"; launchctl bootout "gui/\$(id -u)/com.heron-update" 2>/dev/null; exit "\$1"; }
log start; sleep 60
old=\$(plutil -extract CFBundleShortVersionString raw "\$APP/Contents/Info.plist" 2>/dev/null || echo none)
rm -rf "\$HOME/.heron/updates"/*
if running; then
  osascript -e 'tell application id "sh.heron.app" to quit' &
  for i in \$(seq 1 30); do running || break; sleep 1; done
  running && { kill -TERM \$(pids); for i in \$(seq 1 15); do running || break; sleep 1; done; }
  running && { kill -KILL \$(pids); sleep 2; }
  running && { log "could not stop Heron"; note "Update aborted: Heron would not quit."; finish 1; }
fi
rm -rf "\$BACKUP" "$D/unpack"; [ -d "\$APP" ] && mv "\$APP" "\$BACKUP"
mkdir -p "$D/unpack" && tar xzf "$D/$TAR" -C "$D/unpack" && mv "$D/unpack/Zeron.app" "\$APP" && xattr -dr com.apple.quarantine "\$APP"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "\$APP"
new=\$(plutil -extract CFBundleShortVersionString raw "\$APP/Contents/Info.plist" 2>/dev/null)
open -g "\$APP"; for i in \$(seq 1 45); do running && break; sleep 1; done; sleep 10
if running && [ "\$new" = "$VER" ]; then log "ok: \$old -> \$new"; note "Heron \$new is running."; finish 0; fi
log "failed; restoring \$old"; kill -KILL \$(pids) 2>/dev/null; rm -rf "\$APP"
[ -d "\$BACKUP" ] && mv "\$BACKUP" "\$APP" && open -g "\$APP"
note "Heron \$new failed to start; kept \$old."; finish 1
SCRIPT
chmod +x "$D/update-app.sh"; bash -n "$D/update-app.sh"
P=$HOME/Library/LaunchAgents/com.heron-update.plist
cat >"$P" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.heron-update</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$D/update-app.sh</string></array>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
<key>RunAtLoad</key><true/><key>AbandonProcessGroup</key><true/>
</dict></plist>
PLIST
plutil -lint "$P" >/dev/null
launchctl bootout "gui/$(id -u)/com.heron-update" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$P"
echo "scheduled: Heron $VER in ~60s at /Applications/Heron.app; log $D/update-app.log"

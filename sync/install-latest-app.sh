#!/bin/bash
# Installs the latest kanishkaverma/zeron fork release over /Applications/Zeron.app via a
# one-shot LaunchAgent (survives Zeron quitting), with rollback if the new build won't stay up.
set -euo pipefail
D=$HOME/.local/share/zeron-fork-update; mkdir -p "$D"; cd "$D"
TAG=$(gh release view -R kanishkaverma/zeron --json tagName -q .tagName)
VER=${TAG#fork-}
rm -f ./*.tar.gz manifest.json
gh release download "$TAG" -R kanishkaverma/zeron -p '*macos-arm64-app.tar.gz' -p manifest.json
TAR=zeron-$VER-macos-arm64-app.tar.gz
[ "$(shasum -a 256 "$TAR" | cut -d' ' -f1)" = "$(jq -r '.files[].sha256' manifest.json 2>/dev/null || python3 -c 'import json;print(list(json.load(open("manifest.json"))["files"].values())[0]["sha256"])')" ] || { echo "sha mismatch"; exit 1; }
echo "verified $TAR"
cat >"$D/update-app.sh" <<SCRIPT
#!/bin/bash
APP=/Applications/Zeron.app; BIN=\$APP/Contents/MacOS/zeron; BACKUP=$D/Zeron-previous.app
exec >>"$D/update-app.log" 2>&1
log() { echo "\$(date '+%F %T') \$*"; }
note() { osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Zeron update"' -e 'end run' "\$1"; }
pids() { ps -axo pid=,command= | awk -v b="\$BIN" '{pid=\$1; sub(/^ *[0-9]+ /,""); if (\$0==b) print pid}'; }
running() { [ -n "\$(pids)" ]; }
# Delete the plist first: bootout kills this script, so nothing after it runs.
finish() { rm -f "\$HOME/Library/LaunchAgents/com.zeron-fork-update.plist"; launchctl bootout "gui/\$(id -u)/com.zeron-fork-update" 2>/dev/null; exit "\$1"; }
log start; sleep 60
old=\$(plutil -extract CFBundleShortVersionString raw "\$APP/Contents/Info.plist")
rm -rf "\$HOME/.zeron/updates"/*
osascript -e 'tell application id "sh.zeron.app" to quit' &
for i in \$(seq 1 30); do running || break; sleep 1; done
running && { kill -TERM \$(pids); for i in \$(seq 1 15); do running || break; sleep 1; done; }
running && { kill -KILL \$(pids); sleep 2; }
running && { log "could not stop Zeron"; note "Update aborted: Zeron would not quit."; finish 1; }
rm -rf "\$BACKUP" && mv "\$APP" "\$BACKUP" && tar xzf "$D/$TAR" -C /Applications && xattr -dr com.apple.quarantine "\$APP"
new=\$(plutil -extract CFBundleShortVersionString raw "\$APP/Contents/Info.plist" 2>/dev/null)
open -g "\$APP"; for i in \$(seq 1 45); do running && break; sleep 1; done; sleep 10
if running && [ "\$new" = "$VER" ]; then log "ok: \$old -> \$new"; note "Zeron \$old → \$new (fork) is running."; finish 0; fi
log "rollback to \$old"; kill -KILL \$(pids) 2>/dev/null; rm -rf "\$APP"; mv "\$BACKUP" "\$APP"; open -g "\$APP"
note "Zeron fork build failed to start; rolled back to \$old."; finish 1
SCRIPT
chmod +x "$D/update-app.sh"; bash -n "$D/update-app.sh"
P=$HOME/Library/LaunchAgents/com.zeron-fork-update.plist
cat >"$P" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.zeron-fork-update</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$D/update-app.sh</string></array>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
<key>RunAtLoad</key><true/><key>AbandonProcessGroup</key><true/>
</dict></plist>
PLIST
plutil -lint "$P" >/dev/null
launchctl bootstrap "gui/$(id -u)" "$P"
echo "scheduled: Zeron -> $VER in ~60s; log $D/update-app.log"

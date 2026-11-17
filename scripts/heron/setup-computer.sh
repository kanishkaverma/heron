#!/bin/bash
# Set up the computer Heron lives on (macOS or Linux, with Zeron and Pi
# installed and signed in). Run from a checkout of this repo, on that computer:
#
#   scripts/heron/setup-computer.sh
#
# - installs OptMem (Heron's memory) at ~/.optmem, or updates it
# - puts Heron's AGENTS.md and Pi extension in ~/heron (the folder the
#   Heron chat runs in; the iPhone app makes it a Zeron project)
# - lets Zeron keep an idle chat's agent up for 12 h instead of 30 min, so a
#   background thread's report can wake the Heron chat hours later
#   (ZERON_SESSION_IDLE_MS on the Zeron engine; restarts the engine)
set -euo pipefail

HERE="$(cd "$(dirname "$0")/../.." && pwd)"
SRC="$HERE/heron/agent"
DEST="${HERON_HOME:-$HOME/heron}"
IDLE_MS="${HERON_IDLE_MS:-43200000}"

echo "== OptMem"
curl -fsSL https://raw.githubusercontent.com/VictorTaelin/OptMem/main/install.sh | sh >/dev/null
"$HOME/.optmem/memo" config | head -3

echo "== $DEST"
mkdir -p "$DEST/.pi/extensions/heron"
if [ -f "$DEST/AGENTS.md" ] && ! cmp -s "$SRC/AGENTS.md" "$DEST/AGENTS.md"; then
  cp "$DEST/AGENTS.md" "$DEST/AGENTS.md.bak-$(date +%Y%m%d%H%M%S)"
fi
cp "$SRC/AGENTS.md" "$DEST/AGENTS.md"
cp "$SRC/.pi/extensions/heron/index.ts" "$SRC/.pi/extensions/heron/runner.mjs" "$DEST/.pi/extensions/heron/"
mkdir -p "$HOME/.heron/threads"
ls -la "$DEST" "$DEST/.pi/extensions/heron"

echo "== Zeron idle window: ${IDLE_MS} ms"
case "$(uname)" in
  Darwin)
    plist="$HOME/Library/LaunchAgents/sh.zeron.app.plist"
    if [ -f "$plist" ]; then
      /usr/libexec/PlistBuddy -c "Add :EnvironmentVariables dict" "$plist" 2>/dev/null || true
      /usr/libexec/PlistBuddy -c "Delete :EnvironmentVariables:ZERON_SESSION_IDLE_MS" "$plist" 2>/dev/null || true
      /usr/libexec/PlistBuddy -c "Add :EnvironmentVariables:ZERON_SESSION_IDLE_MS string $IDLE_MS" "$plist"
      launchctl bootout "gui/$(id -u)/sh.zeron.app" 2>/dev/null || launchctl unload "$plist" 2>/dev/null || true
      sleep 2
      launchctl bootstrap "gui/$(id -u)" "$plist" 2>/dev/null || launchctl load "$plist"
      echo "restarted the Zeron engine (launchd)"
    else
      echo "No headless Zeron (sh.zeron.app) here: set ZERON_SESSION_IDLE_MS=$IDLE_MS in Zeron's environment yourself."
    fi
    ;;
  Linux)
    unit="$(systemctl --user list-unit-files 2>/dev/null | awk '/zeron/ {print $1; exit}')"
    if [ -n "$unit" ]; then
      mkdir -p "$HOME/.config/systemd/user/$unit.d"
      printf '[Service]\nEnvironment=ZERON_SESSION_IDLE_MS=%s\n' "$IDLE_MS" > "$HOME/.config/systemd/user/$unit.d/heron.conf"
      systemctl --user daemon-reload
      systemctl --user restart "$unit"
      echo "restarted $unit"
    else
      echo "No Zeron systemd user unit found: set ZERON_SESSION_IDLE_MS=$IDLE_MS in Zeron's environment yourself."
    fi
    ;;
esac
echo "Heron is set up in $DEST."

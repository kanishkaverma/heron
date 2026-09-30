#!/usr/bin/env bash
# Installs the hourly fork sync as a LaunchAgent on this Mac (needs your GPG key and gh login).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
label=com.kanishkaverma.zeron-fork-sync
plist=$HOME/Library/LaunchAgents/$label.plist
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/zeron-fork-sync"
install -m 755 "$here/zeron-fork-sync.sh" "$HOME/.local/bin/zeron-fork-sync"
sed "s|HOME_DIR|$HOME|g" "$here/$label.plist" >"$plist"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
echo "installed; log: ~/.local/share/zeron-fork-sync/sync.log"

#!/bin/bash
# Set up the computer Heron lives on (macOS or Linux, with Zeron and Pi
# installed and signed in). Run from a checkout of this repo, on that computer:
#
#   scripts/heron/setup-computer.sh
#
# - installs OptMem (Heron's memory) at ~/.optmem, or updates it
# - puts Heron's AGENTS.md and Pi extension in ~/heron (the folder the
#   Heron chat runs in; the iPhone app makes it a Zeron project)
# - puts memo-fix (corrects or removes one memory) in ~/.heron
# - installs bun, which runs hist: the history_search / history_read tools
#   over every computer's past sessions (heron-keeper's pi_sync shares the
#   tool and each computer's index)
# - lets Zeron keep an idle chat's agent up for 12 h instead of 30 min, so a
#   background thread's report can wake the Heron chat hours later
#   (ZERON_SESSION_IDLE_MS on the Zeron engine; restarts the engine)
# - picks the model Heron runs on here (the first that answers, best first)
#   into ~/heron/.pi/settings.json
# - installs heron-keeper (launchd / systemd user service): takes the Heron
#   chat over when the computer it's on goes offline
set -euo pipefail

HERE="$(cd "$(dirname "$0")/../.." && pwd)"
SRC="$HERE/heron/agent"
DEST="${HERON_HOME:-$HOME/heron}"
IDLE_MS="${HERON_IDLE_MS:-43200000}"

echo "== OptMem"
curl -fsSL https://raw.githubusercontent.com/VictorTaelin/OptMem/main/install.sh | sh >/dev/null
"$HOME/.optmem/memo" config | head -3

echo "== bun (runs hist, the session-history search)"
if ! command -v bun >/dev/null && [ ! -x "$HOME/.bun/bin/bun" ]; then
  curl -fsSL https://bun.sh/install | bash >/dev/null
fi
mkdir -p "$HOME/.local/bin"
[ -x "$HOME/.bun/bin/bun" ] && ln -sf "$HOME/.bun/bin/bun" "$HOME/.local/bin/bun"

echo "== $DEST"
mkdir -p "$DEST/.pi/extensions/heron"
if [ -f "$DEST/AGENTS.md" ] && ! cmp -s "$SRC/AGENTS.md" "$DEST/AGENTS.md"; then
  cp "$DEST/AGENTS.md" "$DEST/AGENTS.md.bak-$(date +%Y%m%d%H%M%S)"
fi
cp "$SRC/AGENTS.md" "$DEST/AGENTS.md"
cp "$SRC/.pi/extensions/heron/index.ts" "$SRC/.pi/extensions/heron/runner.mjs" "$DEST/.pi/extensions/heron/"
mkdir -p "$HOME/.heron/threads"
cp "$SRC/memo-fix" "$HOME/.heron/memo-fix" && chmod +x "$HOME/.heron/memo-fix"
# Pi loads a folder's .pi/extensions only once the folder is trusted.
python3 - "$DEST" <<'PY'
import json, os, sys
dest = os.path.realpath(sys.argv[1])
path = os.path.join(os.environ.get("PI_CODING_AGENT_DIR") or os.path.expanduser("~/.pi/agent"), "trust.json")
try:
    trust = json.load(open(path))
except Exception:
    trust = {}
if not any(v and (dest == k or dest.startswith(k.rstrip("/") + "/")) for k, v in trust.items()):
    trust[dest] = True
    os.makedirs(os.path.dirname(path), exist_ok=True)
    json.dump(trust, open(path, "w"), indent=2)
    print("trusted", dest, "for Pi")
PY
ls -la "$DEST" "$DEST/.pi/extensions/heron"

echo "== Heron's model here"
PI="$(command -v pi || echo "$HOME/.local/bin/pi")"
# Every model that answers here, best first: ~/.heron/models.json (Heron and
# its tasks fall back down the list on a limit or a login error).
working=()
for model in ${HERON_MODELS:-claude-bridge/claude-opus-5-5 anthropic/claude-opus-5-5 openai-codex/gpt-6.1-sol openai/gpt-6.1-sol}; do
  out="$(cd /tmp && timeout 90 "$PI" -p --no-session --model "$model" "Reply with just: ok" 2>/dev/null || true)"
  if grep -qi '^ok' <<<"$out"; then working+=("$model"); fi
done
if [ "${#working[@]}" -gt 0 ]; then
  printf '%s\n' "${working[@]}" | python3 -c 'import json,sys; json.dump([l.strip() for l in sys.stdin if l.strip()], open(sys.argv[1],"w"), indent=1)' "$HOME/.heron/models.json"
  # Keeps what else is in the file. Compaction: Heron compacts at about 400k
  # tokens (reserve = context window - 400k), or with ~20% left on a smaller window.
  python3 - "$DEST/.pi/settings.json" "${working[0]}" <<'PY'
import json, sys
path, first = sys.argv[1:]
try:
    s = json.load(open(path))
except Exception:
    s = {}
s["defaultProvider"], s["defaultModel"] = first.split("/", 1)
overrides = s.setdefault("compaction", {}).setdefault("modelOverrides", {})
for model, reserve in {
    "claude-bridge/claude-opus-5-5": 600000,   # 1M
    "anthropic/claude-opus-5-5": 600000,       # 1M
    "openai-codex/gpt-6.1-sol": 100000,        # 500K
    "openai-codex/gpt-6.2-sol": 100000,        # 500K
    "openai-codex/gpt-6.2-astra": 55000,       # 272K: compacts at ~217K
}.items():
    overrides.setdefault(model, {"reserveTokens": reserve})
json.dump(s, open(path, "w"), indent=2)
PY
fi
cat "$HOME/.heron/models.json" 2>/dev/null
cat "$DEST/.pi/settings.json" 2>/dev/null || echo "No working model found: Heron will use Pi's default here."

echo "== heron-keeper"
cp "$HERE/heron/keeper/heron-keeper.py" "$HERE/heron/keeper/pi_sync.py" "$HOME/.heron/"
NODE_DIR="$(dirname "$(command -v node || echo /opt/homebrew/bin/node)")"
case "$(uname)" in
  Darwin)
    kplist="$HOME/Library/LaunchAgents/com.kanishkaverma.heron-keeper.plist"
    cat > "$kplist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.kanishkaverma.heron-keeper</string>
  <key>ProgramArguments</key><array><string>/usr/bin/python3</string><string>$HOME/.heron/heron-keeper.py</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>$NODE_DIR:/opt/homebrew/bin:$HOME/.local/bin:/usr/bin:/bin</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$HOME/.heron/keeper.log</string>
  <key>StandardErrorPath</key><string>$HOME/.heron/keeper.log</string>
</dict></plist>
PLIST
    launchctl bootout "gui/$(id -u)/com.kanishkaverma.heron-keeper" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$kplist"
    ;;
  Linux)
    mkdir -p "$HOME/.config/systemd/user"
    cat > "$HOME/.config/systemd/user/heron-keeper.service" <<UNIT
[Unit]
Description=Heron keeper: keeps the Heron chat on an online computer
After=network-online.target

[Service]
Environment=PATH=$NODE_DIR:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=/usr/bin/python3 $HOME/.heron/heron-keeper.py
Restart=always
RestartSec=10
StandardOutput=append:$HOME/.heron/keeper.log
StandardError=append:$HOME/.heron/keeper.log

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable --now heron-keeper.service
    systemctl --user restart heron-keeper.service
    ;;
esac
sleep 2; tail -2 "$HOME/.heron/keeper.log" 2>/dev/null || true

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
    unit="$( (systemctl --user list-unit-files 2>/dev/null || true) | awk '/^zeron/ {print $1}' | head -1 || true)"
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

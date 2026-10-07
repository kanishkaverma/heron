#!/usr/bin/env bash
# E2E: a Pi extension command that shares a skill's name (pi-pstack's /bro and
# /poteto-mode, cursor-team-kit's /thermo-nuclear-code-quality-review) must show
# as one slash-menu row, and picking it must run the extension's wrapper.
# Real headless engine + real pi, hermetic HOME and PI_CODING_AGENT_DIR, driven
# only through IPC with crates/rpc/examples/rpc_probe.rs.
#
# Ways this can go wrong, each a check below:
#   1  the "/" menu lists the wrapper command and its skill as two rows
#      (file-backed skill and package-only harness-skill:pi: skill alike)
#   2  the fix drops the skill, so the "$" skill menu loses it
#   3  the single row runs /skill:<name>, skipping the wrapper (Poteto Mode
#      would stop being sticky)
#   4  a skill named like a Pi builtin Zeron adds (compact) gets bound to the
#      builtin, so picking the skill compacts the session instead
#   5  a skill with no same-named command loses its /skill:<name> binding
#
#   scripts/e2e-pi-skill-wrapper.sh     # exits non-zero on any FAIL
set -uo pipefail
cd "$(dirname "$0")/.."

ART=/tmp/zeron-pi-skill-wrapper-e2e/$(date +%Y%m%d-%H%M%S)
T=$ART/env
PORT=27941
mkdir -p "$T"/{home,data,project,pkgskills/pkgwrap} "$T"/agent/{extensions,skills/{wrap,solo,compact}}
SUMMARY=$ART/summary.txt
: >"$SUMMARY"
PID=
trap 'kill $PID 2>/dev/null' EXIT

echo "▸ building zeron and rpc_probe"
cargo build -q --locked -p zeron >"$ART/build.log" 2>&1 || { tail -40 "$ART/build.log"; exit 1; }
cargo build -q --locked -p zeron-rpc --example rpc_probe >>"$ART/build.log" 2>&1 || { tail -40 "$ART/build.log"; exit 1; }
TARGET=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
ZERON=$TARGET/debug/zeron PROBE=$TARGET/debug/examples/rpc_probe

skill() { printf -- '---\nname: %s\ndescription: %s\n---\nReply with the word %s.\n' "$2" "$3" "$2" >"$1/SKILL.md"; }
skill "$T/agent/skills/wrap" wrap "File-backed skill with a wrapper command"
skill "$T/pkgskills/pkgwrap" pkgwrap "Package-only skill with a wrapper command"
skill "$T/agent/skills/solo" solo "Skill without a command"
skill "$T/agent/skills/compact" compact "Skill named like Pi's compact builtin"
# Package-style skill location Zeron does not scan: it reaches Zeron only as /skill:pkgwrap.
echo "{\"skills\":[\"$T/pkgskills\"]}" >"$T/agent/settings.json"
cat >"$T/agent/extensions/wrappers.ts" <<EOF
import { appendFileSync } from "node:fs";
export default function (pi: any) {
  for (const name of ["wrap", "pkgwrap"]) {
    pi.registerCommand(name, {
      description: "Wrapper command for the " + name + " skill",
      handler: async (args: string) => appendFileSync("$T/marker", name + " wrapper ran [" + args.trim() + "]\n"),
    });
  }
}
EOF

PI=$(python3 -c 'import os,shutil; print(os.path.realpath(shutil.which("pi")))')
NODE_DIR=$(dirname "$(python3 -c 'import os,shutil; print(os.path.realpath(shutil.which("node")))')")
env -i HOME="$T/home" PATH="$NODE_DIR:/usr/bin:/bin:/usr/sbin:/sbin" PI_EXECUTABLE="$PI" \
  PI_CODING_AGENT_DIR="$T/agent" ZERON_DATA_DIR="$T/data" ZERON_IPC_PORT=$PORT \
  NO_COLOR=1 RUST_LOG=warn "$ZERON" headless >"$ART/daemon.log" 2>&1 &
PID=$!
for _ in $(seq 1 120); do
  (exec 3<>/dev/tcp/127.0.0.1/$PORT) 2>/dev/null && { exec 3>&-; break; }
  sleep 0.25
done
"$PROBE" "ws://127.0.0.1:$PORT" EngineReady '{}' >/dev/null

rpc() { # method params
  local reply
  reply=$("$PROBE" "ws://127.0.0.1:$PORT" "$1" "$2")
  echo "{\"method\":\"$1\",\"params\":$2,\"reply\":$reply}" >>"$ART/rpc.jsonl"
  echo "$reply"
}
ok=true
record() { # name pass?(0/1) evidence
  local verdict=FAIL
  [[ "$2" == 0 ]] && verdict=PASS || ok=false
  echo "$verdict  $1 | $3" | tee -a "$SUMMARY"
}

DEV=$(rpc LocalDevice '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["deviceId"])')
SPACE=$(uuidgen | tr 'A-Z' 'a-z')
rpc Mutate "{\"op\":\"createSpace\",\"spaceId\":\"$SPACE\",\"deviceId\":\"$DEV\",\"path\":\"$T/project\"}" >/dev/null
CATALOG="{\"harness\":\"pi\",\"spaceId\":\"$SPACE\"}"
rpc ListCommands "$CATALOG" >"$ART/commands.json"
rpc ListSkills "$CATALOG" >"$ART/skills.json"

# The "/" menu, by the rule in crates/ui/src/composer.rs invocation_candidates:
# commands a skill is bound to are hidden; enabled skills follow.
python3 - "$ART" >"$ART/menu.json" <<'EOF'
import json, sys
art = sys.argv[1]
commands = json.load(open(f"{art}/commands.json"))
skills = json.load(open(f"{art}/skills.json")) or []
bound = {s["command"]["name"] for s in skills if s.get("command")}
rows = [{"kind": "command", "name": c["name"]} for c in commands if c["name"] not in bound]
rows += [{"kind": "skill", "name": s["name"], "command": (s.get("command") or {}).get("name"), "path": s["path"]}
         for s in skills if s["enabled"]]
print(json.dumps(rows, indent=1))
EOF
q() { python3 -c "import json,sys; rows=json.load(open('$ART/menu.json')); skills={s['name']:s for s in json.load(open('$ART/skills.json')) or []}; print($1)"; }

# Pi itself also advertises /skill:<name>; that must not resurface as a third row.
rows=$(q '[(r["kind"], r["name"]) for r in rows if r["name"].removeprefix("skill:") in ("wrap","pkgwrap")]')
record "1 one / row per wrapper+skill pair" "$(q 'all(sum(r["name"].removeprefix("skill:")==n for r in rows)==1 for n in ("wrap","pkgwrap"))' | grep -qx True; echo $?)" \
  "rows: $rows"

record "2 \$ menu still lists both skills" "$(q '"wrap" in skills and "pkgwrap" in skills' | grep -qx True; echo $?)" \
  "wrap: $(q 'skills.get("wrap")'); pkgwrap: $(q 'skills.get("pkgwrap")')"

record "4 compact skill stays on /skill:compact" "$(q '(skills.get("compact") or {}).get("command",{}).get("name")=="skill:compact"' | grep -qx True; echo $?)" \
  "compact: $(q 'skills.get("compact")')"

record "5 solo skill keeps /skill:solo" "$(q '(skills.get("solo") or {}).get("command",{}).get("name")=="skill:solo"' | grep -qx True; echo $?)" \
  "solo: $(q 'skills.get("solo")')"

# 3: pick the wrap and pkgwrap skill rows in a real Pi chat, as the composer sends them.
for name in wrap pkgwrap; do
  CHAT=$(uuidgen | tr 'A-Z' 'a-z')
  rpc Mutate "{\"op\":\"createChat\",\"chatId\":\"$CHAT\",\"spaceId\":\"$SPACE\",\"config\":{\"harness\":\"pi\",\"model\":null,\"reasoning\":null,\"sandbox\":\"workspace-write\"}}" >/dev/null
  # Invocation::link(): "[$name](zeron-invoke:<hex of the serialized invocation>)".
  PROMPT=$(python3 - "$ART/skills.json" "$name" <<'EOF'
import json, sys
s = next(s for s in json.load(open(sys.argv[1])) if s["name"] == sys.argv[2])
inv = {"kind": "skill", "name": s["name"], "path": s["path"]}
if s.get("command"):
    # Field order matters: the engine only accepts the canonical serialization.
    inv["command"] = {"name": s["command"]["name"], "harness": s["command"]["harness"]}
hexed = json.dumps(inv, separators=(",", ":")).encode().hex()
print(json.dumps(f"[${s['name']}](zeron-invoke:{hexed}) now"))
EOF
)
  rpc QueueCommand "{\"chatId\":\"$CHAT\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen | tr 'A-Z' 'a-z')\",\"request\":{\"prompt\":$PROMPT,\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"$T/project\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}}}" >/dev/null
done
for _ in $(seq 1 120); do
  [[ $(grep -c "wrapper ran" "$T/marker" 2>/dev/null) == 2 ]] && break
  sleep 0.5
done
record "3 picking the row runs the wrapper" \
  "$(grep -qx 'wrap wrapper ran \[now\]' "$T/marker" 2>/dev/null && grep -qx 'pkgwrap wrapper ran \[now\]' "$T/marker" && echo 0 || echo 1)" \
  "marker: $(tr '\n' ';' <"$T/marker" 2>/dev/null || echo none)"

echo "artifacts: $ART"
$ok

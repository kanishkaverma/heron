#!/usr/bin/env python3
"""Call one Zeron MCP tool through `zeron mcp` (stdio JSON-RPC) and print the result.

  zmcp.py <tool> '<json args>'      e.g. zmcp.py list_chats '{"limit":5}'

Used to drive and check the Heron chat from the computer it runs on.
"""
import json, os, shutil, subprocess, sys

APP = "/Applications/Zeron.app/Contents/MacOS/zeron"
exe = os.environ.get("ZERON") or (APP if os.path.exists(APP) else shutil.which("zeron"))
tool = sys.argv[1]
args = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
env = {k: v for k, v in os.environ.items() if not k.startswith("ZERON_CHAT")}
p = subprocess.Popen([exe, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)

def call(i, method, params):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n")
    p.stdin.flush()
    while True:
        line = p.stdout.readline()
        if not line:
            raise SystemExit("zeron mcp closed")
        msg = json.loads(line)
        if msg.get("id") == i:
            return msg

call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "heron-zmcp", "version": "1"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
p.stdin.flush()
res = call(2, "tools/call", {"name": tool, "arguments": args})
p.terminate()
if "error" in res:
    print(json.dumps(res["error"], indent=1))
    sys.exit(1)
for c in res["result"].get("content", []):
    if c.get("type") == "text":
        try:
            print(json.dumps(json.loads(c["text"]), indent=1))
        except Exception:
            print(c["text"])

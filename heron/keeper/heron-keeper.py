#!/usr/bin/env python3
"""heron-keeper: keeps the one Heron chat running on an available computer.

Runs on every Heron computer (launchd / systemd user service), next to Zeron.
Every few seconds it finds the Heron chat (the top-level chat whose working
folder is `~/heron`, oldest first) and:

- if this computer hosts it: makes sure the chat uses this computer's Heron
  model (`~/heron/.pi/settings.json`), because a bare model id reported by Pi
  can resolve to a provider whose login has lapsed here;
- if its computer has been offline for OFFLINE_SECS and this is the
  first computer in PRIORITY that is online: takes the chat over (Zeron's
  `setChatHost`). Messages waiting for the dead computer run here, and the
  Heron Pi extension catches the new session up from the chat's transcript.

Config (optional): ~/.heron/keeper.json
  {"priority": ["nishka-server", "zeph", "Kanishka’s MacBook Pro"],
   "offline_secs": 120, "interval_secs": 15}
Talks to the local engine only: `zeron mcp` for reads, the engine's IPC for
the two writes. Logs to stdout (one line per decision).
"""
import datetime
import json
import os
import shutil
import subprocess
import sys
import time

HOME = os.path.expanduser("~")
CONFIG = os.path.join(HOME, ".heron", "keeper.json")
DEFAULTS = {
    "priority": ["nishka-server", "zeph", "Kanishka\u2019s MacBook Pro"],
    "offline_secs": 120,
    "interval_secs": 15,
}
HERON_CWDS = {"~/heron", "~/heron/"}


def log(*parts):
    print(datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"), *parts, flush=True)


def config():
    cfg = dict(DEFAULTS)
    try:
        cfg.update(json.load(open(CONFIG)))
    except Exception:
        pass
    return cfg


def zeron_exe():
    app = "/Applications/Zeron.app/Contents/MacOS/zeron"
    if os.path.exists(app):
        return app
    return shutil.which("zeron") or os.path.join(HOME, ".local", "bin", "zeron")


# ---------------------------------------------------------------- MCP reads

class Mcp:
    def __init__(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("ZERON_CHAT")}
        self.p = subprocess.Popen([zeron_exe(), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, env=env)
        self.n = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "heron-keeper", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("zeron mcp closed")
            msg = json.loads(line)
            if msg.get("id") == self.n:
                if "error" in msg:
                    raise RuntimeError(json.dumps(msg["error"]))
                return msg["result"]

    def tool(self, name, args=None):
        res = self.rpc("tools/call", {"name": name, "arguments": args or {}})
        text = "".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        if res.get("isError"):
            raise RuntimeError(text)
        return json.loads(text)

    def close(self):
        try:
            self.p.terminate()
        except Exception:
            pass


# ---------------------------------------------------------------- IPC writes

def ipc(method, params):
    """One call on the engine's IPC WebSocket (node is on every Heron computer)."""
    token_file = os.path.join(HOME, ".zeron", "ipc-token")
    script = r"""
const fs = require("fs");
const [method, params, tokenFile, port] = process.argv.slice(1);
const token = fs.readFileSync(tokenFile, "utf8").trim();
const ws = new WebSocket(`ws://127.0.0.1:${port}`, { headers: { Authorization: `Bearer ${token}` } });
ws.onopen = () => ws.send(JSON.stringify({ id: 1, method, params: JSON.parse(params) }));
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id === 1) { console.log(JSON.stringify(d)); process.exit(0); } };
ws.onerror = (e) => { console.error(String(e.message || e)); process.exit(1); };
setTimeout(() => process.exit(2), 15000);
"""
    node = shutil.which("node") or "/opt/homebrew/bin/node"
    out = subprocess.run([node, "-e", script, method, json.dumps(params), token_file,
                          os.environ.get("ZERON_IPC_PORT", "27654")],
                         capture_output=True, text=True, timeout=30)
    if out.returncode != 0:
        raise RuntimeError(f"ipc {method}: {out.stderr.strip() or out.returncode}")
    reply = json.loads(out.stdout.strip().splitlines()[-1])
    if "error" in reply or "err" in reply:
        raise RuntimeError(f"ipc {method}: {reply}")
    return reply


# ---------------------------------------------------------------- decisions

def heron_model():
    """This computer's Heron model: ~/heron/.pi/settings.json, provider/id."""
    try:
        s = json.load(open(os.path.join(HOME, "heron", ".pi", "settings.json")))
        if s.get("defaultProvider") and s.get("defaultModel"):
            return f"{s['defaultProvider']}/{s['defaultModel']}"
    except Exception:
        pass
    return None


def iso_age(ts):
    if not ts:
        return 1e12
    t = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
    return (datetime.datetime.now(datetime.timezone.utc) - t).total_seconds()


def find_heron_chat(mcp):
    """The Heron chat: top-level, working in ~/heron, live before archived,
    the oldest first. An archived one is brought back (never two Herons)."""
    chats = mcp.tool("list_chats", {"limit": 500, "include_archived": True})["chats"]
    mine = [c for c in chats if not c.get("parentChatId") and (c.get("cwd") or "") in HERON_CWDS
            and c.get("title") == "Heron"]
    mine.sort(key=lambda c: (bool(c.get("archived")), c.get("createdAt") or ""))
    if not mine:
        return None
    chat = mine[0]
    if chat.get("archived"):
        mcp.tool("archive_chat", {"chat": chat["id"], "archived": False})
        log(f"brought {chat['id'][:8]} back from Archived")
    return chat


def tick(cfg, state):
    mcp = Mcp()
    try:
        devices = mcp.tool("list_devices")["devices"]
        me = next(d for d in devices if d.get("local"))
        chat = find_heron_chat(mcp)
        offline = cfg["offline_secs"]
        online = lambda d: d.get("local") or iso_age(d.get("lastSeenAt")) < offline
        if not chat:
            # No Heron chat yet (first run, or it was archived): the first
            # online computer in priority order makes one here, in ~/heron.
            first = next((d for name in cfg["priority"] for d in devices if d["name"] == name and online(d)), None)
            if first and first["id"] == me["id"]:
                made = mcp.tool("create_chat", {"kind": "chat", "device": me["id"], "harness": "pi",
                                                "cwd": "~/heron", "title": "Heron"})
                log(f"made the Heron chat {made.get('chatId', '')[:8]} here")
            return
        host = next((d for d in devices if d["id"] == chat["deviceId"]), None)
        if chat["deviceId"] == me["id"]:
            want = heron_model()
            if want and chat.get("model") != want:
                full = mcp.tool("get_chat", {"chat": chat["id"]})
                cfg_now = {"harness": full.get("harness") or "pi", "model": want,
                           "reasoning": full.get("reasoning"), "modelOptions": {},
                           "sandbox": "danger-full-access"}
                ipc("Mutate", {"op": "setChatConfig", "chatId": chat["id"], "config": cfg_now})
                log(f"model {chat.get('model')} -> {want} on {chat['id'][:8]}")
            return
        if host and online(host):
            return
        # The host is gone: the first online computer in priority order takes over.
        names = {d["name"]: d for d in devices}
        for name in cfg["priority"]:
            d = names.get(name)
            if not d or d["id"] == chat["deviceId"] or not online(d):
                continue
            if d["id"] != me["id"]:
                return  # someone ahead of us will take it
            ipc("Mutate", {"op": "setChatHost", "chatId": chat["id"], "deviceId": me["id"]})
            log(f"took over {chat['id'][:8]} from {host['name'] if host else chat['deviceId']} "
                f"(last seen {int(iso_age(host.get('lastSeenAt')) if host else -1)}s ago)")
            return
    finally:
        mcp.close()


def main():
    once = "--once" in sys.argv
    state = {}
    log("heron-keeper started", zeron_exe(), "model", heron_model())
    while True:
        cfg = config()
        try:
            tick(cfg, state)
        except Exception as e:
            log("error:", str(e)[:300])
        if once:
            break
        time.sleep(cfg["interval_secs"])


if __name__ == "__main__":
    main()

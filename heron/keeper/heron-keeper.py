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
    "interval_secs": 10,
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


# ---------------------------------------------------------------- tasks
#
# Background tasks live in ~/.heron/threads, the same on every computer
# (Syncthing). The leader (first online computer in priority order) leases
# each task that needs running to the least busy online computer, and moves
# a task whose computer has gone offline to another one (attempt + 1). Every
# keeper runs the tasks leased to its own computer (runner.mjs) and stops
# local runs of tasks leased elsewhere. Who writes which file: runner.mjs.

THREADS = os.path.join(HOME, ".heron", "threads")
RUNS = os.path.join(HOME, ".heron", "runs")  # this computer only: pid per task
FINAL = {"done", "failed", "stopped"}
MAX_RESTARTS = 2


def read_json(path, default=None):
    try:
        return json.load(open(path))
    except Exception:
        return default


def write_json(path, value):
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w") as f:
        json.dump(value, f, indent=2)
    os.replace(tmp, path)


def tasks():
    try:
        names = sorted(os.listdir(THREADS))
    except FileNotFoundError:
        return []
    out = []
    for name in names:
        d = os.path.join(THREADS, name)
        request = read_json(os.path.join(d, "request.json"))
        if not request or not os.path.exists(os.path.join(d, "spec.json")):
            continue
        out.append({
            "id": name, "dir": d, "request": request,
            "lease": read_json(os.path.join(d, "lease.json"), {}),
            "status": read_json(os.path.join(d, "status.json"), {}),
            "stop": read_json(os.path.join(d, "stop.json"), {}),
        })
    return out


def needs_run(t):
    req, st = t["request"]["req"], t["status"]
    if t["stop"].get("req") == req:
        return False
    return not (st.get("req") == req and st.get("state") in FINAL)


def pid_alive(pid):
    if not pid:
        return False
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def local_run(tid):
    run = read_json(os.path.join(RUNS, f"{tid}.json"), {})
    return run if pid_alive(run.get("pid")) else None


def kill_run(tid, run, sig):
    import signal
    try:
        os.killpg(run["pid"], getattr(signal, sig))
    except OSError:
        try:
            os.kill(run["pid"], getattr(signal, sig))
        except OSError:
            pass


def start_run(t, attempt, resume):
    os.makedirs(RUNS, exist_ok=True)
    runner = os.path.join(HOME, "heron", ".pi", "extensions", "heron", "runner.mjs")
    node = shutil.which("node") or "/opt/homebrew/bin/node"
    pi = shutil.which("pi") or os.path.join(HOME, ".local", "bin", "pi")
    req = t["request"]["req"]
    args = [node, runner, t["dir"], str(req), str(attempt)] + (["--resume"] if resume else [])
    env = {k: v for k, v in os.environ.items() if not k.startswith("ZERON_")}
    env["HERON_PI"] = pi
    p = subprocess.Popen(args, cwd=t["dir"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
    write_json(os.path.join(RUNS, f"{t['id']}.json"), {"pid": p.pid, "req": req, "attempt": attempt, "at": time.time()})
    log(f"task {t['id']}: started request {req} attempt {attempt}{' (resuming)' if resume else ''}")


def schedule(devices, me, online, cfg):
    """`devices`/`online` are None when Zeron is unreachable: then only run
    what is already leased here."""
    all_tasks = tasks()
    if devices is not None:
        heron = [d for name in cfg["priority"] for d in devices if d["name"] == name]
        up = [d for d in heron if online(d)]
        if up and up[0]["id"] == me["id"]:
            lead(all_tasks, heron, up)
    for t in all_tasks:
        own_duties(t, me)


def lead(all_tasks, heron, up):
    by_id = {d["id"]: d for d in heron}
    load = {d["id"]: 0 for d in up}
    for t in all_tasks:
        lease = t["lease"]
        if needs_run(t) and lease.get("req") == t["request"]["req"] and lease.get("owner") in load:
            load[lease["owner"]] += 1

    def least(exclude=None):
        cands = [d for d in up if d["id"] != exclude]
        return min(cands, key=lambda d: load[d["id"]]) if cands else None

    for t in all_tasks:
        if not needs_run(t):
            continue
        req, lease = t["request"]["req"], t["lease"]
        if lease.get("req") != req:
            d = least()
            write_json(os.path.join(t["dir"], "lease.json"),
                       {"req": req, "attempt": 1, "owner": d["id"], "ownerName": d["name"], "at": time.time()})
            load[d["id"]] += 1
            log(f"task {t['id']}: request {req} -> {d['name']}")
        elif lease.get("owner") not in load:
            d = least(exclude=lease.get("owner"))
            if not d:
                continue
            gone = by_id.get(lease.get("owner"), {}).get("name", lease.get("ownerName"))
            write_json(os.path.join(t["dir"], "lease.json"),
                       {"req": req, "attempt": lease.get("attempt", 1) + 1, "owner": d["id"], "ownerName": d["name"],
                        "at": time.time(), "movedFrom": gone})
            load[d["id"]] += 1
            log(f"task {t['id']}: {gone} is offline, moved to {d['name']} (attempt {lease.get('attempt', 1) + 1})")


def own_duties(t, me):
    req, lease, st = t["request"]["req"], t["lease"], t["status"]
    run = local_run(t["id"])
    mine = lease.get("req") == req and lease.get("owner") == me["id"]
    if run and (not mine or run.get("req") != req or run.get("attempt") != lease.get("attempt")):
        # The task moved to another computer (or on to a newer request).
        kill_run(t["id"], run, "SIGKILL")
        log(f"task {t['id']}: stopped the local run (now leased to {lease.get('ownerName')})")
        return
    if not mine:
        return
    if t["stop"].get("req") == req:
        if run:
            kill_run(t["id"], run, "SIGTERM")
            log(f"task {t['id']}: stopped on request")
        return
    if not needs_run(t) or run:
        return
    attempt = lease.get("attempt", 1)
    same = st.get("req") == req and st.get("attempt") == attempt
    if same and st.get("state") == "running":
        # Ours, marked running, but no process: the run died (a restart?).
        if st.get("restarts", 0) >= MAX_RESTARTS:
            write_json(os.path.join(t["dir"], "status.json"),
                       {**st, "state": "failed", "error": "The task's run kept dying.", "endedAt": time.time() * 1000})
            log(f"task {t['id']}: gave up after {MAX_RESTARTS} restarts")
            return
        start_run(t, attempt, resume=True)
    elif not same:
        start_run(t, attempt, resume=attempt > 1)


def tick(cfg, state):
    try:
        mcp = Mcp()
    except Exception as e:
        log("zeron unreachable:", str(e)[:200])
        if state.get("me"):
            schedule(None, state["me"], None, cfg)
        return
    try:
        devices = mcp.tool("list_devices")["devices"]
        me = next(d for d in devices if d.get("local"))
        state["me"] = me
        offline = cfg["offline_secs"]
        online = lambda d: d.get("local") or iso_age(d.get("lastSeenAt")) < offline
        try:
            keep_chat(mcp, cfg, devices, me, online)
        finally:
            schedule(devices, me, online, cfg)
    except Exception as e:
        if state.get("me"):
            schedule(None, state["me"], None, cfg)
        raise e
    finally:
        mcp.close()


NUDGE = "\u27f3 Background results are ready."
NUDGE_AFTER_SECS = 45
nudged = {}


def nudge_for_reports(mcp, chat):
    """A task finished but no live Heron agent reported it (the chat moved
    here and nobody has written since): post a short note into the chat so
    its agent starts and delivers the waiting reports."""
    now = time.time()
    waiting = []
    for t in tasks():
        req, st = t["request"]["req"], t["status"]
        spec = read_json(os.path.join(t["dir"], "spec.json"), {})
        if spec.get("chat") not in (None, chat["id"]):
            continue
        if st.get("req") != req or st.get("state") not in FINAL:
            continue
        if os.path.exists(os.path.join(t["dir"], f"notified-{req}")):
            continue
        delivering = read_json(os.path.join(t["dir"], f"delivering-{req}"), None)
        if delivering and now - (delivering.get("at") or 0) / 1000 < 180:
            continue
        if now - (st.get("endedAt") or now * 1000) / 1000 < NUDGE_AFTER_SECS:
            continue
        if now - nudged.get((t["id"], req), 0) < 300:
            continue
        waiting.append((t["id"], req))
    if not waiting or (chat.get("sessionStatus") or "idle") in ("working", "awaitingInput"):
        return
    mcp.tool("send_message", {"chat": chat["id"], "text": NUDGE})
    for key in waiting:
        nudged[key] = now
    log(f"nudged {chat['id'][:8]}: {len(waiting)} report(s) waiting")


def keep_chat(mcp, cfg, devices, me, online):
        chat = find_heron_chat(mcp)
        offline = cfg["offline_secs"]
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
            nudge_for_reports(mcp, chat)
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


PI_SYNC_EVERY = 1800


def maybe_pi_sync(state):
    """Every 30 minutes, in the background: make this computer's Pi setup
    match the others (pi_sync.py: skills, extensions, packages, MCP, version)."""
    proc = state.get("pi_sync")
    if proc and proc.poll() is None:
        return
    if time.time() - state.get("pi_sync_at", time.time() - PI_SYNC_EVERY + 60) < PI_SYNC_EVERY:
        return
    state["pi_sync_at"] = time.time()
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pi_sync.py")
    if os.path.exists(script):
        state["pi_sync"] = subprocess.Popen([sys.executable, script], stdout=sys.stdout, stderr=sys.stdout,
                                            stdin=subprocess.DEVNULL, start_new_session=True)


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
        try:
            maybe_pi_sync(state)
        except Exception as e:
            log("pi-sync error:", str(e)[:200])
        time.sleep(cfg["interval_secs"])


if __name__ == "__main__":
    main()

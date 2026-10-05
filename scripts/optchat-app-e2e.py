#!/usr/bin/env python3
"""OptChat through a running Zeron engine, the way the app drives it.

Talks to the engine over `zeron mcp` (the engine's own IPC), so every turn goes
through the registry, the run controls and the transcript store, exactly as a
message typed in the app does. Two separate Zeron chats share OptChat's one
memory: the second chat must recall what the user told the first.

Usage: scripts/optchat-app-e2e.py [--port 49777] [--out target/optchat-app-e2e]
Needs: Zeron Dev running (scripts/run-macos-dev.sh) and OptChat signed in to
Claude (Settings > Accounts, or <data>/profiles/local/optchat/auth.json).
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class Mcp:
    def __init__(self, port):
        env = dict(os.environ, ZERON_IPC_PORT=str(port))
        for key in ("ZERON_CHAT_ID", "ZERON_DEVICE_ID"):
            env.pop(key, None)
        self.proc = subprocess.Popen(
            [str(ROOT / "target/debug/zeron"), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            env=env,
            text=True,
        )
        self.next_id = 0
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "optchat-app-e2e", "version": "1"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params):
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("zeron mcp exited")
            reply = json.loads(line)
            if reply.get("id") == self.next_id:
                if "error" in reply:
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply["result"]

    def call(self, tool, **args):
        result = self.request("tools/call", {"name": tool, "arguments": args})
        text = "".join(c.get("text", "") for c in result.get("content", []))
        if result.get("isError"):
            raise RuntimeError(f"{tool}: {text}")
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return text


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=49777)
    parser.add_argument("--out", default=str(ROOT / "target/optchat-app-e2e"))
    parser.add_argument("--model", default="claude-sonnet-5-5")
    args = parser.parse_args()
    out = Path(args.out)
    workspace = out / "workspace"
    workspace.mkdir(parents=True, exist_ok=True)
    checks = []

    def check(name, ok, detail):
        checks.append({"name": name, "pass": bool(ok), "detail": detail})
        print(f"{'PASS' if ok else 'FAIL'} {name}: {detail}", flush=True)

    mcp = Mcp(args.port)
    harnesses = mcp.call("list_harnesses")["harnesses"]
    optchat = next((h for h in harnesses if h.get("id") == "optchat"), None)
    check("engine lists OptChat", optchat is not None and optchat.get("available"), json.dumps(optchat)[:300])
    models = [m.get("id") for m in mcp.call("list_models", harness="optchat")["models"]]
    check("OptChat offers its models", args.model in models, models)

    tree = f"ginkgo-{int(time.time()) % 100000}"
    started = time.time()
    first = mcp.call(
        "create_chat",
        harness="optchat",
        model=args.model,
        cwd=str(workspace),
        kind="chat",
        title="OptChat app e2e: tell",
        prompt=f"My favorite tree is the {tree}. Just say ok.",
        wait=True,
        timeout_secs=300,
    )
    outcome = (first.get("turn") or {}).get("outcome")
    check("first chat answers", outcome == "completed", json.dumps(first.get("turn"))[:400])

    second = mcp.call(
        "create_chat",
        harness="optchat",
        model=args.model,
        cwd=str(workspace),
        kind="chat",
        title="OptChat app e2e: recall",
        prompt="What is my favorite tree? Reply with its exact name only.",
        wait=True,
        timeout_secs=300,
    )
    reply = json.dumps(second.get("turn"))
    check("a second Zeron chat recalls the first through the shared memory", tree in reply, reply[:400])

    memory = mcp.call("send_message", chat=second["chatId"], text="/memory", wait=True, timeout_secs=120)
    text = json.dumps(memory)
    found = re.search(r"(/[^\s\"]*memory\.html)", text)
    page = found.group(1) if found else None
    check("/memory writes the browsing page", page and Path(page).is_file(), page)

    transcript = mcp.call("read_chat", chat=second["chatId"], limit=10)
    report = {
        "elapsed_s": round(time.time() - started, 1),
        "checks": checks,
        "chats": {"tell": first, "recall": second},
        "memory_page": page,
        "transcript": transcript,
    }
    (out / "report.json").write_text(json.dumps(report, indent=2))
    print(f"report: {out / 'report.json'}")
    mcp.proc.stdin.close()
    failed = [c for c in checks if not c["pass"]]
    if failed:
        print(f"FAIL: {len(failed)} checks failed")
        sys.exit(1)
    print("PASS optchat-app-e2e")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Keep Pi's setup the same on every Heron computer, without taking anything away.

Run by heron-keeper every 30 minutes (and by hand: `python3 pi_sync.py`).
Everything travels through ~/.heron/shared, which Syncthing keeps the same on
every Heron computer:

  shared/skills/<name>/       every computer's skills (links resolved, no node_modules)
  shared/extensions/<name>    every computer's global Pi extensions
  shared/tools/<name>         every computer's Pi tools (~/.pi/agent/tools, e.g. hist)
  shared/history/<host>.db    each computer's session-history index (`hist publish`)
  shared/hosts/<host>.json    each computer's Pi version, packages and MCP servers

On each computer it:
  1. adds its own skills and extensions to the pool (a newer copy replaces an older one);
  2. links in every skill and extension it doesn't have (~/.agents/skills/<name>,
     ~/.pi/agent/extensions/<name> -> the pool); what it already has stays as is;
  3. installs the packages other computers have that it lacks (`pi install`), except
     one of a kind it already covers (two loop packages, two web-search packages)
     and local-folder packages that don't exist here;
  4. adds MCP servers it lacks (when their command exists here);
  5. updates Pi when another computer runs a newer version;
  6. publishes its session-history index, so `hist` (the history_search and
     history_read tools) on the other computers can read this one's sessions.
Nothing is ever removed.
"""
import datetime
import json
import os
import re
import shutil
import socket
import subprocess
import sys

HOME = os.path.expanduser("~")
AGENT = os.environ.get("PI_CODING_AGENT_DIR") or os.path.join(HOME, ".pi", "agent")
SHARED = os.path.join(HOME, ".heron", "shared")
SKILL_DIRS = [os.path.join(HOME, ".agents", "skills"), os.path.join(AGENT, "skills")]
EXT_DIR = os.path.join(AGENT, "extensions")
TOOLS_DIR = os.path.join(AGENT, "tools")
HOST = socket.gethostname().split(".")[0]
# Per-machine files a tool installs and rewrites itself.
EXT_SKIP = {"herdr-agent-state.ts", ".DS_Store"}
# Packages that do the same job: a computer keeps the one it has.
FAMILIES = [
    {"pi-loop", "pi-mono-loop"},
    {"pi-deepseek-search", "pi-web-access"},
]


def log(*parts):
    print(datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"), "pi-sync:", *parts, flush=True)


def pi_exe():
    return shutil.which("pi") or os.path.join(HOME, ".local", "bin", "pi")


def run(args, timeout=600):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout)


def in_pool(path):
    return os.path.realpath(path).startswith(os.path.realpath(SHARED) + os.sep)


def newest_mtime(path):
    newest = 0
    for root, dirs, files in os.walk(path, followlinks=True):
        dirs[:] = [d for d in dirs if d != "node_modules" and not d.startswith(".git")]
        for f in files:
            try:
                newest = max(newest, os.stat(os.path.join(root, f)).st_mtime)
            except OSError:
                pass
    if os.path.isfile(path):
        newest = os.stat(path).st_mtime
    return newest


def copy_into_pool(src, dest):
    """Resolved copy of src (file or folder) at dest, without node_modules or git."""
    tmp = dest + ".incoming"
    shutil.rmtree(tmp, ignore_errors=True)
    if os.path.isdir(src):
        shutil.copytree(src, tmp, symlinks=False,
                        ignore=shutil.ignore_patterns("node_modules", ".git", ".DS_Store", "*.sync-conflict-*"))
    else:
        shutil.copy2(src, tmp)
    if os.path.isdir(dest) and not os.path.islink(dest):
        shutil.rmtree(dest)
    elif os.path.lexists(dest):
        os.remove(dest)
    os.replace(tmp, dest)


# ---------------------------------------------------------------- skills, extensions

def local_items(dirs, skip=()):
    """name -> real path of what this computer has (not links into the pool)."""
    found = {}
    for d in dirs:
        try:
            names = sorted(os.listdir(d))
        except FileNotFoundError:
            continue
        for name in names:
            p = os.path.join(d, name)
            if name in skip or name.startswith(".") or ".sync-conflict-" in name or name.endswith(".incoming"):
                continue
            if not os.path.exists(p) or in_pool(p):
                continue  # a broken link, or one we made
            found.setdefault(name, os.path.realpath(p))
    return found


def share(kind, mine, link_dir):
    pool = os.path.join(SHARED, kind)
    os.makedirs(pool, exist_ok=True)
    os.makedirs(link_dir, exist_ok=True)
    added = linked = 0
    for name, path in mine.items():
        dest = os.path.join(pool, name)
        if not os.path.lexists(dest) or newest_mtime(path) > newest_mtime(dest) + 60:
            try:
                copy_into_pool(path, dest)
                added += 1
            except Exception as e:
                log(f"couldn't share {kind}/{name}: {e}")
    for name in sorted(os.listdir(pool)):
        if name.startswith(".") or name.endswith(".incoming") or ".sync-conflict-" in name or name in mine:
            continue
        link = os.path.join(link_dir, name)
        if os.path.lexists(link):
            if os.path.islink(link) and not os.path.exists(link):
                os.remove(link)  # broken: point it at the pool instead
            else:
                continue
        os.symlink(os.path.join(pool, name), link)
        linked += 1
    if added or linked:
        log(f"{kind}: shared {added}, linked in {linked}")


# ---------------------------------------------------------------- packages, MCP, version

def pkg_source(entry):
    return entry if isinstance(entry, str) else entry.get("source", "")


def pkg_name(source):
    """npm:@scope/pi-x@1.2 -> pi-x; git:github.com/u/pi-x -> pi-x; a local path stays itself."""
    if source.startswith("npm:"):
        spec = source[4:]
        at = spec.rfind("@")
        if at > 0:
            spec = spec[:at]
        return spec.split("/")[-1]
    if re.match(r"^(git:|https?://)", source):
        return source.rstrip("/").split("/")[-1].split("@")[0].removesuffix(".git")
    return source


def is_local(source):
    return not re.match(r"^(npm:|git:|https?://)", source)


def settings():
    try:
        return json.load(open(os.path.join(AGENT, "settings.json")))
    except Exception:
        return {}


def mcp_servers():
    try:
        d = json.load(open(os.path.join(AGENT, "mcp.json")))
        return d.get("mcpServers", d), ("mcpServers" in d)
    except Exception:
        return {}, True


def pi_version():
    out = run([pi_exe(), "--version"], timeout=60).stdout.strip().splitlines()
    return out[-1].strip() if out else "0"


def vtuple(v):
    return tuple(int(x) for x in re.findall(r"\d+", v)[:3]) or (0,)


def write_manifest():
    hosts = os.path.join(SHARED, "hosts")
    os.makedirs(hosts, exist_ok=True)
    servers, _ = mcp_servers()
    manifest = {"host": HOST, "pi": pi_version(), "packages": [pkg_source(p) for p in settings().get("packages", [])],
                "mcp": servers, "at": datetime.datetime.now().isoformat()}
    tmp = os.path.join(hosts, f".{HOST}.json.tmp")
    json.dump(manifest, open(tmp, "w"), indent=2)
    os.replace(tmp, os.path.join(hosts, f"{HOST}.json"))
    return manifest


def others():
    hosts = os.path.join(SHARED, "hosts")
    out = []
    for f in sorted(os.listdir(hosts)):
        if f.endswith(".json") and not f.startswith(".") and f != f"{HOST}.json" and ".sync-conflict-" not in f:
            try:
                out.append(json.load(open(os.path.join(hosts, f))))
            except Exception:
                pass
    return out


def skip_list():
    """Packages never copied between computers: ~/.heron/shared/pi-sync.json
    {"skip_packages": ["remote-pi", ...]} (a package removed on purpose)."""
    try:
        return set(json.load(open(os.path.join(SHARED, "pi-sync.json"))).get("skip_packages", []))
    except Exception:
        return set()


def sync_packages(mine, peers):
    have = {pkg_name(s) for s in mine["packages"]} | skip_list()
    for peer in peers:
        for source in peer.get("packages", []):
            name = pkg_name(source)
            if name in have:
                continue
            family = next((f for f in FAMILIES if name in f), set())
            if family & have:
                continue
            if is_local(source):
                path = source if os.path.isabs(source) else os.path.normpath(os.path.join(AGENT, source))
                if not os.path.exists(path):
                    continue
            out = run([pi_exe(), "install", source], timeout=900)
            if out.returncode == 0:
                have.add(name)
                log(f"installed {source} (from {peer['host']})")
            else:
                log(f"couldn't install {source}: {(out.stderr or out.stdout).strip()[-300:]}")


def sync_mcp(peers):
    servers, wrapped = mcp_servers()
    added = []
    for peer in peers:
        for name, server in (peer.get("mcp") or {}).items():
            if name in servers:
                continue
            cmd = server.get("command")
            if cmd and not (shutil.which(cmd) or os.path.exists(os.path.expanduser(cmd))):
                continue
            servers[name] = server
            added.append(name)
    if added:
        path = os.path.join(AGENT, "mcp.json")
        if os.path.exists(path):
            shutil.copy2(path, path + ".bak-heron")
        json.dump({"mcpServers": servers} if wrapped else servers, open(path, "w"), indent=2)
        log(f"added MCP servers: {', '.join(added)}")


def sync_version(mine, peers):
    newest = max([mine["pi"]] + [p.get("pi", "0") for p in peers], key=vtuple)
    if vtuple(newest) > vtuple(mine["pi"]):
        out = run([pi_exe(), "update", "--self"], timeout=900)
        log(f"pi {mine['pi']} -> {pi_version()} ({'ok' if out.returncode == 0 else out.stderr.strip()[-200:]})")


def publish_history():
    hist = os.path.join(TOOLS_DIR, "hist", "hist.ts")
    bun = shutil.which("bun") or os.path.join(HOME, ".bun", "bin", "bun")
    if not (os.path.exists(hist) and os.path.exists(bun)):
        log("history not published: needs bun and ~/.pi/agent/tools/hist here")
        return
    try:
        out = run([bun, hist, "publish"], timeout=1800)
    except Exception as e:
        log(f"history not published: {e}")
        return
    said = (out.stdout or out.stderr).strip().splitlines()[-1:] or ["?"]
    if out.returncode != 0 or not said[0].startswith("up to date"):
        log(f"history: {said[0][-200:]}")


def main():
    os.makedirs(SHARED, exist_ok=True)
    share("skills", local_items(SKILL_DIRS), SKILL_DIRS[0])
    share("extensions", local_items([EXT_DIR], EXT_SKIP), EXT_DIR)
    share("tools", local_items([TOOLS_DIR]), TOOLS_DIR)
    publish_history()
    mine = write_manifest()
    peers = others()
    sync_version(mine, peers)
    sync_packages(mine, peers)
    sync_mcp(peers)
    write_manifest()
    log(f"done ({len(peers)} other computers)")


if __name__ == "__main__":
    if "--dry-run" in sys.argv:
        print(json.dumps({"skills": local_items(SKILL_DIRS), "extensions": local_items([EXT_DIR], EXT_SKIP)}, indent=1)[:3000])
    else:
        main()

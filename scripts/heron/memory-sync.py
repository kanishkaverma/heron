#!/usr/bin/env python3
"""Sync Heron's memory (~/.heron/memory) and its background tasks
(~/.heron/threads) between the Heron computers with Syncthing, privately over
the tailnet (no global discovery, no relays).

  memory-sync.py init                   make this computer's Syncthing identity; print its device id
  memory-sync.py peers '<json>'         share the folder with these peers and (re)start the service
      json: [{"name": "air", "id": "<device id>", "address": "tcp://100.x.y.z:22001"}, ...]

Syncthing runs as its own instance (home ~/.heron/syncthing, GUI on
127.0.0.1:8385, sync port 22001), so it never touches another Syncthing setup.
"""
import json
import os
import platform
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

HOME = os.path.expanduser("~")
ST_HOME = os.path.join(HOME, ".heron", "syncthing")
# id, label, path, ignore patterns (what stays on each computer)
FOLDERS = [
    ("heron-memory", "Heron memory", os.path.join(HOME, ".heron", "memory"), []),
    ("heron-threads", "Heron tasks", os.path.join(HOME, ".heron", "threads"),
     # (?d): a task removed on another computer can still be deleted here.
     # Build output (a repo a task built in its work folder) never syncs: one
     # cargo target/ is tens of thousands of files and stalls every other task.
     ["(?d)session", "(?d)events.jsonl", "(?d)stderr.log", "*.tmp", "(?d).DS_Store",
      "(?d)node_modules", "(?d)target", "(?d)DerivedData", "(?d).build"]),
    ("heron-shared", "Heron shared Pi setup", os.path.join(HOME, ".heron", "shared"),
     ["node_modules", "*.incoming", ".*.json.tmp", "(?d).DS_Store"]),
]
PORT = 22001
GUI = "127.0.0.1:8385"


def syncthing():
    return shutil.which("syncthing") or next(
        (p for p in ["/opt/homebrew/bin/syncthing", os.path.join(HOME, ".local/bin/syncthing")] if os.path.exists(p)), "syncthing")


def device_id():
    out = subprocess.run([syncthing(), "device-id", f"--home={ST_HOME}"], capture_output=True, text=True)
    if out.returncode == 0 and out.stdout.strip():
        return out.stdout.strip()
    tree = ET.parse(os.path.join(ST_HOME, "config.xml"))
    gui_dev = tree.getroot().find("device")
    return gui_dev.get("id")


def init():
    os.makedirs(ST_HOME, exist_ok=True)
    if not os.path.exists(os.path.join(ST_HOME, "config.xml")):
        subprocess.run([syncthing(), "generate", f"--home={ST_HOME}", "--no-port-probing"],
                       check=True, capture_output=True, text=True)
    for _, _, path, _ in FOLDERS:
        os.makedirs(path, exist_ok=True)
    print(device_id())


def sub(parent, tag, text=None, **attrs):
    el = parent.find(tag)
    if el is None:
        el = ET.SubElement(parent, tag)
    for k, v in attrs.items():
        el.set(k, v)
    if text is not None:
        el.text = text
    return el


def peers(spec):
    me = device_id()
    peers = [p for p in json.loads(spec) if p["id"] != me]
    path = os.path.join(ST_HOME, "config.xml")
    tree = ET.parse(path)
    root = tree.getroot()
    # Only Heron's folder; only these devices (plus this one).
    for f in root.findall("folder"):
        root.remove(f)
    for d in root.findall("device"):
        if d.get("id") != me:
            root.remove(d)
    for p in peers:
        d = ET.SubElement(root, "device", id=p["id"], name=p["name"], compression="metadata",
                          introducer="false", skipIntroductionRemovals="false", introducedBy="")
        ET.SubElement(d, "address").text = p["address"]
        ET.SubElement(d, "paused").text = "false"
        ET.SubElement(d, "autoAcceptFolders").text = "false"
    first_device = root.find("device")
    for fid, label, fpath, ignores in FOLDERS:
        os.makedirs(fpath, exist_ok=True)
        if ignores:
            open(os.path.join(fpath, ".stignore"), "w").write("\n".join(ignores) + "\n")
        folder = ET.Element("folder", id=fid, label=label, path=fpath, type="sendreceive",
                            rescanIntervalS="60", fsWatcherEnabled="true", fsWatcherDelayS="1",
                            ignorePerms="false", autoNormalize="true")
        ET.SubElement(folder, "filesystemType").text = "basic"
        for did in [me] + [p["id"] for p in peers]:
            ET.SubElement(folder, "device", id=did, introducedBy="")
        ET.SubElement(folder, "minDiskFree", unit="%").text = "1"
        ET.SubElement(folder, "maxConflicts").text = "10"
        root.insert(list(root).index(first_device), folder)
    gui = sub(root, "gui", enabled="true", tls="false")
    sub(gui, "address", GUI)
    opts = sub(root, "options")
    for lst in opts.findall("listenAddress"):
        opts.remove(lst)
    ET.SubElement(opts, "listenAddress").text = f"tcp://0.0.0.0:{PORT}"
    for tag, val in [("globalAnnounceEnabled", "false"), ("localAnnounceEnabled", "false"),
                     ("relaysEnabled", "false"), ("natEnabled", "false"), ("urAccepted", "-1"),
                     ("crashReportingEnabled", "false"), ("startBrowser", "false"),
                     ("autoUpgradeIntervalH", "0")]:
        sub(opts, tag, val)
    tree.write(path, encoding="utf-8", xml_declaration=True)
    service()
    print(f"{me}: sharing {', '.join(f[2] for f in FOLDERS)} with {', '.join(p['name'] for p in peers)}")


def service():
    exe = syncthing()
    args = [exe, "serve", f"--home={ST_HOME}", "--no-browser", "--no-restart", f"--gui-address={GUI}"]
    log = os.path.join(HOME, ".heron", "syncthing.log")
    if platform.system() == "Darwin":
        plist = os.path.join(HOME, "Library/LaunchAgents/com.kanishkaverma.heron-syncthing.plist")
        items = "".join(f"<string>{a}</string>" for a in args)
        open(plist, "w").write(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.kanishkaverma.heron-syncthing</string>
  <key>ProgramArguments</key><array>{items}</array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
""")
        uid = os.getuid()
        subprocess.run(["launchctl", "bootout", f"gui/{uid}/com.kanishkaverma.heron-syncthing"], capture_output=True)
        # bootout returns before the job is gone; bootstrap too soon fails (5).
        import time
        for _ in range(10):
            time.sleep(1)
            if subprocess.run(["launchctl", "bootstrap", f"gui/{uid}", plist], capture_output=True).returncode == 0:
                break
        else:
            subprocess.run(["launchctl", "kickstart", "-k", f"gui/{uid}/com.kanishkaverma.heron-syncthing"], check=True)
    else:
        unit_dir = os.path.join(HOME, ".config/systemd/user")
        os.makedirs(unit_dir, exist_ok=True)
        open(os.path.join(unit_dir, "heron-syncthing.service"), "w").write(f"""[Unit]
Description=Syncthing for Heron's memory
After=network-online.target

[Service]
ExecStart={' '.join(args)}
Restart=always
RestartSec=10
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
""")
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        subprocess.run(["systemctl", "--user", "enable", "--now", "heron-syncthing.service"], check=True)
        subprocess.run(["systemctl", "--user", "restart", "heron-syncthing.service"], check=True)


if __name__ == "__main__":
    if sys.argv[1:2] == ["init"]:
        init()
    elif sys.argv[1:2] == ["peers"]:
        peers(sys.argv[2])
    else:
        print(__doc__)
        sys.exit(2)

#!/usr/bin/env python3
"""Sync Heron's memory (~/.heron/memory) between the Heron computers with
Syncthing, privately over the tailnet (no global discovery, no relays).

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
FOLDER = os.path.join(HOME, ".heron", "memory")
FOLDER_ID = "heron-memory"
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
    os.makedirs(FOLDER, exist_ok=True)
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
    folder = ET.Element("folder", id=FOLDER_ID, label="Heron memory", path=FOLDER, type="sendreceive",
                        rescanIntervalS="60", fsWatcherEnabled="true", fsWatcherDelayS="1",
                        ignorePerms="false", autoNormalize="true")
    ET.SubElement(folder, "filesystemType").text = "basic"
    for did in [me] + [p["id"] for p in peers]:
        ET.SubElement(folder, "device", id=did, introducedBy="")
    ET.SubElement(folder, "minDiskFree", unit="%").text = "1"
    ET.SubElement(folder, "maxConflicts").text = "10"
    first_device = root.find("device")
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
    os.makedirs(FOLDER, exist_ok=True)
    service()
    print(f"{me}: sharing {FOLDER} with {', '.join(p['name'] for p in peers)}")


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
        subprocess.run(["launchctl", "bootstrap", f"gui/{uid}", plist], check=True)
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

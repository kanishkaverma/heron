# Heron

Heron is this fork's iPhone app: a one-chat personal assistant. It is
[Zeron](https://github.com/zeronsh/zeron)'s iOS app (MIT) with one change:
after you sign in there is only a chat.

- **One chat, any computer.** No session list, new-session page, side chats,
  terminals or voice. The app opens straight into the chat titled Heron. Its
  agent (Pi) runs on one of your computers at a time, in `~/heron` (which
  every Heron computer has). When that computer goes offline, the next one in
  line takes the chat over by itself (`heron/keeper`), catches up on the
  conversation from Zeron's synced transcript and carries on. The ⋯ menu shows
  which computer is running it.
- **Background threads you never see.** The agent hands longer work to
  background threads (`thread_start`): separate Pi runs on that computer that
  outlive the chat's turn. The app never shows them. When one finishes, its
  report wakes the chat and Heron tells you what matters.
- **Memory:** [OptMem](https://github.com/VictorTaelin/OptMem), its store
  at `~/.heron/memory`, kept identical on every Heron computer by Syncthing
  over the tailnet (`scripts/heron/memory-sync.py`). Heron wakes it every
  session and notes what it learns.
- **Tools and accounts:** the agent has the computer's shell, files and web,
  plus whatever Pi has connected (the Executor MCP gateway for Gmail,
  Calendar, Drive, Slack, WhatsApp, …). Heron drafts; it sends only after
  you say so.

## Pieces

| Where | What |
| --- | --- |
| `apps/ios/Zeron/Heron/` | The one-chat shell (`HeronRootController`) and how it finds or makes the chat (`HeronStep`) |
| `apps/ios/Zeron/Session/SessionViewController.swift` | The chat screen without companions, voice or side chats; Heron's ⋯ menu |
| `heron/agent/AGENTS.md` | Who Heron is: memory, how to talk, drafts-first, threads, accounts |
| `heron/agent/.pi/extensions/heron/` | The background threads: `thread_start` / `_list` / `_read` / `_message` / `_stop`, a detached runner, and the report that wakes the chat; catch-up when the chat arrives from another computer |
| `heron/keeper/heron-keeper.py` | Runs on every Heron computer: makes the Heron chat, keeps it on this computer's working model, takes it over when its computer is offline (priority: air, zeph, MacBook Pro) |
| `scripts/heron/memory-sync.py` | Syncthing for `~/.heron/memory` between the Heron computers |
| `scripts/heron/setup-computer.sh` | Sets up a computer: OptMem, `~/heron`, its working model, heron-keeper, a 12 h idle window for Zeron |
| `scripts/heron/sim.sh` | The fast loop: build, install, launch and screenshot in the iOS Simulator |
| `scripts/heron/install-on-iphone.sh` | Downloads CI's archive, signs it with your team, installs it on your iPhone |
| `.github/workflows/heron-ios.yml` | CI: unsigned device archive, plus unit and UI tests with screenshots in the simulator |

## Set up

1. On every computer Heron may run on (Zeron and Pi installed and signed in):
   `scripts/heron/setup-computer.sh`, then `scripts/heron/memory-sync.py init`
   on each and `memory-sync.py peers '<json>'` with all their ids
2. On a Mac paired with your iPhone: `scripts/heron/install-on-iphone.sh`
3. Open Heron and sign in with your Zeron account.

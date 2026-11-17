# Heron

Heron is this fork's iPhone app: a one-chat personal assistant. It is
[Zeron](https://github.com/zeronsh/zeron)'s iOS app (MIT) with one change:
after you sign in there is only a chat.

- **One chat, one computer.** No session list, new-session page, side chats,
  terminals or voice. The app opens straight into a chat titled Heron, whose
  agent (Pi) runs on one computer, in its `~/heron` folder. The ⋯ menu shows
  which computer and can switch it. With several computers and none chosen,
  the app asks once.
- **Background threads you never see.** The agent hands longer work to
  background threads (`thread_start`): separate Pi runs on that computer that
  outlive the chat's turn. The app never shows them. When one finishes, its
  report wakes the chat and Heron tells you what matters.
- **Memory:** [OptMem](https://github.com/VictorTaelin/OptMem)
  (`~/.optmem`). Heron wakes it every session and notes what it learns.
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
| `heron/agent/.pi/extensions/heron/` | The background threads: `thread_start` / `_list` / `_read` / `_message` / `_stop`, a detached runner, and the report that wakes the chat |
| `scripts/heron/setup-computer.sh` | Sets up the computer: OptMem, `~/heron`, a 12 h idle window for Zeron |
| `scripts/heron/install-on-iphone.sh` | Downloads CI's archive, signs it with your team, installs it on your iPhone |
| `.github/workflows/heron-ios.yml` | CI: unsigned device archive, plus unit and UI tests with screenshots in the simulator |

## Set up

1. On the computer Heron lives on (Zeron and Pi installed and signed in):
   `scripts/heron/setup-computer.sh`
2. On a Mac paired with your iPhone: `scripts/heron/install-on-iphone.sh`
3. Open Heron and sign in with your Zeron account.
